//! Playback helpers for the remote source (RFC-005).
//!
//! A projected [`RemoteTrack`](super::read::RemoteTrack) carries metadata
//! but no audio: the server's tracks have no local file. To play one, the
//! server mints a **stream ticket** — a sealed, time-limited credential
//! (TTL ~1h) — which authorises the ticketed stream endpoint on its own,
//! so `<audio src>` / [`HttpMediaSource`](crate::audio::http_source) can
//! fetch it without an `Authorization` header.
//!
//! ## The URL is built from our base, never the server's
//!
//! `POST /tracks/{id}/stream-ticket` deliberately answers with a
//! **relative** path (`/api/v2/stream/<ticket>`). We prepend the binding's
//! own `base_url` and reject anything absolute or protocol-relative: an
//! absolute URL from the server could redirect playback to a host the user
//! never authenticated against.
//!
//! ## Transcoding is asked for here, or nowhere
//!
//! The stream route has always accepted `format` and `bitrate`; this module
//! simply never sent them, so every remote stream was the original file.
//! They are a property of the *request*, not of the ticket, which is why the
//! preference is resolved at this seam and the rest of the playback path is
//! untouched — it receives a URL either way.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    remote::client::RemoteClient,
    state::AppState,
};

/// `profile_setting` keys. Written by the settings UI through
/// `useProfileSetting`, read here — the URL is built backend-side, so the
/// preference has to be legible from both.
const FORMAT_KEY: &str = "remote.transcode.format";
const BITRATE_KEY: &str = "remote.transcode.bitrate";

/// The server's own defaults, mirrored so the client sends an explicit
/// bitrate rather than relying on them silently.
const DEFAULT_MP3_BITRATE: u32 = 192;
const DEFAULT_OPUS_BITRATE: u32 = 128;
/// The server rejects anything outside this with a 400, so clamp rather
/// than let a stored value from a hand-edited row break playback.
const MIN_BITRATE: u32 = 32;
const MAX_BITRATE: u32 = 512;

/// What the profile wants the server to encode a remote stream as.
///
/// `Off` is the default and means the original bytes: transcoding degrades
/// audio to save bandwidth, which is a trade nobody should be opted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeFormat {
    Off,
    Mp3,
    Opus,
}

impl TranscodeFormat {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("mp3") => Self::Mp3,
            Some("opus") => Self::Opus,
            // Anything unrecognised — including a value from a newer build —
            // plays the original rather than guessing at a codec.
            _ => Self::Off,
        }
    }

    fn query_value(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Mp3 => Some("mp3"),
            Self::Opus => Some("opus"),
        }
    }

    fn default_bitrate(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Mp3 => DEFAULT_MP3_BITRATE,
            Self::Opus => DEFAULT_OPUS_BITRATE,
        }
    }
}

/// The resolved preference: a format and the bitrate to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranscodePreference {
    pub format: TranscodeFormat,
    pub bitrate: u32,
}

impl TranscodePreference {
    pub const OFF: Self = Self {
        format: TranscodeFormat::Off,
        bitrate: 0,
    };

    pub fn is_on(&self) -> bool {
        self.format != TranscodeFormat::Off
    }
}

/// What the server says about its own transcoder.
///
/// `available` is a startup capability (both FFmpeg tools were found), and
/// the two ceilings are what a `429` on the stream route enforces.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct TranscodeStatus {
    pub available: bool,
    pub active: usize,
    pub global_limit: usize,
    pub per_user_limit: usize,
}

impl TranscodeStatus {
    /// Whether asking for a transcode right now stands a chance.
    ///
    /// The per-account ceiling cannot be checked from here — the response
    /// reports the whole server's `active`, not ours — so this is the
    /// server-wide gate only. It is a pre-check, not a reservation: two
    /// clients can pass it at once and one of them still be refused.
    fn has_headroom(&self) -> bool {
        self.available && self.active < self.global_limit
    }
}

#[derive(Deserialize)]
struct StreamTicketResponse {
    /// Relative, e.g. `/api/v2/stream/<ticket>`.
    url: String,
}

async fn client(state: &AppState) -> AppResult<RemoteClient<'_>> {
    RemoteClient::try_build(state)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))
}

/// Read the profile's transcode preference.
///
/// A missing or unreadable row is `OFF`, which is also the default: a
/// preference we cannot read must not silently degrade someone's audio.
pub async fn preference(pool: &SqlitePool) -> TranscodePreference {
    let format = TranscodeFormat::parse(
        read_setting(pool, FORMAT_KEY)
            .await
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    );
    if format == TranscodeFormat::Off {
        return TranscodePreference::OFF;
    }
    let bitrate = read_setting(pool, BITRATE_KEY)
        .await
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or_else(|| format.default_bitrate())
        .clamp(MIN_BITRATE, MAX_BITRATE);
    TranscodePreference { format, bitrate }
}

async fn read_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Ask the server what its transcoder can do right now.
///
/// Offline is refused before the request, like every other outbound path:
/// this one is reachable from the settings card as well as from
/// [`ticket_url`], and only the latter was checking.
pub async fn status(state: &AppState) -> AppResult<TranscodeStatus> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let client = client(state).await?;
    client
        .send_json(client.request(reqwest::Method::GET, "/api/v2/transcode/status"))
        .await
        .map_err(|err| AppError::Other(format!("transcode status: {}", err.message)))
}

/// Mint a stream ticket for a remote track and return a locally-playable
/// absolute URL (`{base_url}/api/v2/stream/<ticket>`), safe to hand to
/// `player_play_url` / `HttpMediaSource`.
///
/// When the profile asks for a transcode, the query is appended here — and
/// only after the server says it has room. That pre-check is the whole
/// answer to a `429`: the refusal arrives when the *decoder* opens the URL,
/// on the audio thread, where there is nothing sensible to retry with. So
/// the fallback to the original bytes is decided before the request rather
/// than discovered by being refused, and a saturated server costs the user
/// bandwidth instead of a track that will not play.
pub async fn ticket_url(state: &AppState, track_id: &str) -> AppResult<String> {
    // Read the preference before the client is built: `try_build` releases
    // its own lease before any network call, and holding one across a
    // request would stall a profile switch for its whole duration.
    let wanted = {
        let pool = state.require_profile_pool().await?;
        preference(&pool).await
    };

    let base = raw_ticket_url(state, track_id).await?;
    if !wanted.is_on() {
        return Ok(base);
    }
    Ok(format!("{base}{}", transcode_query(state, wanted).await))
}

/// A stream URL for the track's **original** bytes, ignoring the transcode
/// preference.
///
/// What a download keeps, and what reconciliation hashes, must be the file the
/// server holds — not a re-encode of it. The preference exists to spend less
/// bandwidth on a stream heard once; baking it into a stored copy would make a
/// bandwidth choice permanent, and silently.
pub async fn raw_ticket_url(state: &AppState, track_id: &str) -> AppResult<String> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    let client = client(state).await?;

    let resp: StreamTicketResponse = client
        .send_json(client.request(
            reqwest::Method::POST,
            &format!("/api/v2/tracks/{track_id}/stream-ticket"),
        ))
        .await
        .map_err(|err| AppError::Other(format!("stream ticket: {}", err.message)))?;

    // Trust only our own base. A relative path is the contract; an absolute
    // or protocol-relative one would move playback off the authenticated
    // host, so refuse it rather than follow it.
    let rel = resp.url;
    if !rel.starts_with('/') || rel.starts_with("//") {
        return Err(AppError::Other(
            "remote server returned a non-relative stream URL".into(),
        ));
    }
    Ok(format!("{}{}", client.base_url(), rel))
}

/// The `?format=&bitrate=` suffix, or an empty string when the original
/// bytes are what should be fetched.
async fn transcode_query(state: &AppState, preference: TranscodePreference) -> String {
    let Some(format) = preference.format.query_value() else {
        return String::new();
    };
    // A status call that fails is not a reason to fail playback: fall back
    // to the original bytes, which always work.
    match status(state).await {
        Ok(status) if status.has_headroom() => {
            format!("?format={format}&bitrate={}", preference.bitrate)
        }
        Ok(status) => {
            tracing::debug!(
                active = status.active,
                global_limit = status.global_limit,
                available = status.available,
                "server has no transcode headroom; streaming the original"
            );
            String::new()
        }
        Err(err) => {
            tracing::debug!(%err, "transcode status unavailable; streaming the original");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_format_is_off_rather_than_a_guess() {
        assert_eq!(TranscodeFormat::parse(None), TranscodeFormat::Off);
        assert_eq!(TranscodeFormat::parse(Some("off")), TranscodeFormat::Off);
        assert_eq!(TranscodeFormat::parse(Some("flac")), TranscodeFormat::Off);
        assert_eq!(TranscodeFormat::parse(Some("mp3")), TranscodeFormat::Mp3);
        assert_eq!(TranscodeFormat::parse(Some("opus")), TranscodeFormat::Opus);
    }

    #[test]
    fn raw_carries_no_query_because_the_server_rejects_one() {
        // `format=raw` with a bitrate is a 400 server-side, so `Off` must
        // produce no query at all rather than an explicit raw.
        assert_eq!(TranscodeFormat::Off.query_value(), None);
    }

    #[test]
    fn headroom_needs_both_a_transcoder_and_room() {
        let full = TranscodeStatus {
            available: true,
            active: 4,
            global_limit: 4,
            per_user_limit: 2,
        };
        assert!(!full.has_headroom(), "at the ceiling is not headroom");

        let no_ffmpeg = TranscodeStatus {
            available: false,
            active: 0,
            global_limit: 4,
            per_user_limit: 2,
        };
        assert!(!no_ffmpeg.has_headroom(), "idle but unable is not headroom");

        let ok = TranscodeStatus {
            available: true,
            active: 3,
            global_limit: 4,
            per_user_limit: 2,
        };
        assert!(ok.has_headroom());
    }
}
