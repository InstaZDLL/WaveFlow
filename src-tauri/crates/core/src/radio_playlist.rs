//! Station playlists: the `.m3u` / `.m3u8` and `.pls` files a radio
//! hands out instead of its stream.
//!
//! Adding a station by hand means accepting what its website links to,
//! and that is as often a small playlist naming the stream as the stream
//! itself — `listen.pls`, `live.m3u`. Both formats are a few lines of
//! text, parsed here without a network or a filesystem so the rules can
//! be tested on their own; fetching and resolving belong to the caller.
//!
//! **HLS is not a station list.** An `.m3u8` can also be an HTTP Live
//! Streaming playlist — a list of short media segments, not a list of
//! stations. It is recognised by its `#EXT-X-` tags and reported as such:
//! the playlist is then the stream itself, which the player polls
//! (`playback::hls`), not a list to take a station from.

/// One stream a playlist names, in the order it names them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlaylistEntry {
    /// As written: absolute, or relative to the playlist's own URL. The
    /// caller resolves and keeps only `http` / `https`.
    pub url: String,
    /// `#EXTINF:-1,Title` / `TitleN=` — the station's name, when the
    /// playlist gives one.
    pub title: Option<String>,
}

/// Why a playlist gave no station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistError {
    /// An HTTP Live Streaming playlist: segments, not a station.
    Hls,
    /// Nothing in it names a stream.
    Empty,
}

impl std::fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hls => "an HLS playlist, not a station",
            Self::Empty => "the playlist names no stream",
        })
    }
}

impl std::error::Error for PlaylistError {}

/// Whether `path` (a URL path or a file name) is spelled like a playlist.
pub fn has_playlist_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".m3u", ".m3u8", ".pls"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Whether `url` is an absolute `http` / `https` URL — the only kind a
/// station can be. Case-insensitive on the scheme, as URLs are.
pub fn is_http_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Parse a playlist, whichever of the two it is.
///
/// `.pls` announces itself with a `[playlist]` header; everything else is
/// read as M3U, which is also what a bare list of URLs is.
pub fn parse_playlist(text: &str) -> Result<Vec<PlaylistEntry>, PlaylistError> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().map(str::trim).find(|l| !l.is_empty());
    let entries = if first.is_some_and(|l| l.eq_ignore_ascii_case("[playlist]")) {
        parse_pls(text)
    } else {
        parse_m3u(text)?
    };
    if entries.is_empty() {
        Err(PlaylistError::Empty)
    } else {
        Ok(entries)
    }
}

fn parse_m3u(text: &str) -> Result<Vec<PlaylistEntry>, PlaylistError> {
    let mut out = Vec::new();
    let mut title: Option<String> = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if strip_prefix_ignore_case(line, "#EXT-X-").is_some() {
            return Err(PlaylistError::Hls);
        }
        if let Some(info) = strip_prefix_ignore_case(line, "#EXTINF:") {
            // `#EXTINF:<duration>,<title>` — the title is everything after
            // the first comma, commas included.
            title = info
                .split_once(',')
                .map(|(_, t)| t.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        out.push(PlaylistEntry {
            url: line.to_string(),
            title: title.take(),
        });
    }
    Ok(out)
}

fn parse_pls(text: &str) -> Vec<PlaylistEntry> {
    use std::collections::BTreeMap;
    // `FileN=` and `TitleN=`, numbered from 1 and in any order; the
    // number is what pairs a title with its file.
    let mut files: BTreeMap<u32, String> = BTreeMap::new();
    let mut titles: BTreeMap<u32, String> = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if let Some(n) = numbered(key, "file") {
            files.insert(n, value.to_string());
        } else if let Some(n) = numbered(key, "title") {
            titles.insert(n, value.to_string());
        }
    }
    files
        .into_iter()
        .map(|(n, url)| PlaylistEntry {
            url,
            title: titles.remove(&n),
        })
        .collect()
}

/// `File3` → `Some(3)` for `name = "file"`, ignoring case.
fn numbered(key: &str, name: &str) -> Option<u32> {
    strip_prefix_ignore_case(key, name)?.parse().ok()
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len()
        && s.is_char_boundary(prefix.len())
        && s[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
