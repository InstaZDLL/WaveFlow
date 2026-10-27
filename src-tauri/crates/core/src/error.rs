//! Generic error type shared by every `waveflow-core` function — and,
//! through a `#[from]` bridge in `crates/app/src/error.rs`, by the
//! Tauri command surface.
//!
//! Variants here are deliberately storage- and runtime-agnostic so
//! the same type can flow through axum handlers in the future
//! `waveflow-server`. Anything Tauri-specific (the command serialisation
//! bridge, `tauri::Error`, the `MissingAppDataDir` shape that depends on
//! `dirs::data_dir()`) lives in the app crate's `AppError`.

use thiserror::Error;

/// Top-level error type for `waveflow-core`.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("profile not found: id={0}")]
    ProfileNotFound(i64),

    #[error("no profile is currently active")]
    NoActiveProfile,

    /// Catch-all for the audio-pipeline + image-composition error
    /// surfaces (smart-playlist covers, future scanner artwork
    /// helpers). Mirrors `AppError::Audio` so the wire format stays
    /// the same when the variant crosses the boundary.
    #[error("audio error: {0}")]
    Audio(String),

    #[error("{0}")]
    Other(String),
}

pub type CoreResult<T> = Result<T, CoreError>;

/// Render an error *with its cause chain*, for a log line or a message
/// the interface shows verbatim.
///
/// `reqwest::Error`'s `Display` stops at the outermost layer — a
/// failed request reads only "error sending request", which says the
/// transport failed but never says why. The reason (DNS lookup,
/// connection refused, timeout, TLS handshake) lives in `source()`,
/// so without unwinding it a bug report arrives with no way to tell
/// those cases apart. That is exactly what happened in issue #452.
///
/// The chain is depth-capped: `source()` is author-controlled and
/// nothing forbids a cyclic or absurdly deep implementation.
///
/// Shared by the plugin host (a guest's failed request, a load failure
/// read through `RuntimeError::detail`) and by [`describe_http_error`],
/// which every network path of the app goes through. One walker, so a
/// fix to the dedup or the cap reaches all of them.
pub fn describe_error(err: &dyn std::error::Error) -> String {
    describe_chain(err.to_string(), err.source())
}

/// [`describe_error`] with the outermost message already rendered, so
/// a caller can hand it a cleaned-up first layer.
fn describe_chain(first: String, mut source: Option<&dyn std::error::Error>) -> String {
    const MAX_DEPTH: usize = 8;
    let mut out = first;
    // The last message actually appended, tracked separately from
    // `out`: testing against the accumulated string would compare a
    // cause to the tail of *everything* written so far, so a distinct
    // cause that happens to end the buffer ("failed to connect" then
    // "connect") would be dropped as a repeat.
    let mut last = out.clone();
    let mut depth = 0;
    while let Some(cause) = source {
        if depth >= MAX_DEPTH {
            out.push_str(": …");
            break;
        }
        // Skip a layer that just restates its parent — `reqwest` and
        // `hyper` both do this, and "timed out: timed out" reads
        // worse than the single line it repeats. So does an error
        // that prints its source after a colon and also returns it
        // from `source()` (`#[error("request failed: {0}")]` over a
        // `#[from]` field): "request failed: timed out" followed by
        // "timed out". Only the previous layer counts, either exactly
        // or as what follows its last `": "`.
        let text = cause.to_string();
        let restated = text == last || last.ends_with(&format!(": {text}"));
        if !restated {
            out.push_str(": ");
            out.push_str(&text);
        }
        last = text;
        source = cause.source();
        depth += 1;
    }
    out
}

/// A transport error rendered for the log or the interface: the cause
/// chain unwound ([`describe_error`]), and no URL in it.
///
/// `reqwest` prints the request URL in its own `Display` (" for url
/// (…)"), and those URLs carry secrets: a stream ticket in the path, a
/// key in the query, a radio's credentials in the userinfo. The URL is
/// taken off the first layer, and every URL any layer prints anyway — a
/// redirect target in a redirect error, a custom policy's message — is
/// cut down to its origin (`scheme://host:port`). The host stays: it is
/// what tells a DNS failure on one server from a refusal on another.
///
/// Takes the error by reference so a type that keeps a `reqwest::Error`
/// (already stripped with `without_url`) can render it in its own
/// `Display`.
pub fn describe_http_error(err: &reqwest::Error) -> String {
    let mut first = err.to_string();
    if let Some(url) = err.url() {
        first = first.replace(&format!(" for url ({url})"), "");
    }
    redact_urls(&describe_chain(first, std::error::Error::source(err)))
}

/// [`describe_http_error`] for an error this crate cannot name: a
/// `reqwest::Error` of another major version, wrapped by a dependency (the
/// updater plugin's). Its first layer keeps reqwest's " for url (…)" — the
/// version-specific `url()` is out of reach — but that URL, like any other
/// in the chain, is cut down to its origin.
pub fn describe_error_without_urls(err: &dyn std::error::Error) -> String {
    redact_urls(&describe_error(err))
}

/// Cut every URL in `text` down to its origin. A URL is a run of
/// non-whitespace holding `://`; trailing punctuation that wraps it in
/// prose (`)`, `,`, …) is kept outside. A run that does not parse is
/// replaced whole — it cannot be told apart from a URL, so it is not
/// kept.
fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let Some(marker) = word.find("://") else {
            out.push_str(piece);
            continue;
        };
        // The scheme starts after any wrapping punctuation: "(https://…".
        let start = word[..marker]
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map_or(0, |(at, c)| at + c.len_utf8());
        let end = word
            .trim_end_matches([')', ']', '>', '"', '\'', ',', ';', ':', '.', '!', '?'])
            .len()
            .max(marker + 3);
        out.push_str(&word[..start]);
        match reqwest::Url::parse(&word[start..end]) {
            Ok(url) if url.has_host() => out.push_str(&url.origin().ascii_serialization()),
            _ => out.push_str("<url>"),
        }
        out.push_str(&piece[end..]);
    }
    out
}

#[cfg(test)]
mod tests;
