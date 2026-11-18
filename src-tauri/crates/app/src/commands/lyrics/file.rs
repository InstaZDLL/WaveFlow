//! Lyrics for a file played without importing it (#1128).
//!
//! Such a file has no library row, so the waterfall of [`fetch_lyrics`]
//! cannot run on it: there is no hash to cache under and no row to read
//! the path from. It used to be looked up like a radio song, by artist and
//! title only, and the lyrics sitting beside it, or in its own tag, were
//! never read: offline, or for a song no provider has, the panel stayed
//! empty with the `.lrc` right there.
//!
//! The local tiers now run on the file itself, in the library's order —
//! the tag, the sidecar, the description — and the query-based lookup of
//! [`fetch_radio_lyrics`] after them. `lyrics.prefer_lrclib` swaps the two
//! as it does for a library track. Nothing local is cached: there is no
//! row to key it on, and reading a tag and a directory costs less than
//! the cache lookup would.

use std::path::{Path, PathBuf};

use crate::{error::AppResult, external_playback::ExternalEntry, state::AppState};

use super::*;

/// Lyrics for the file at `path`, played without importing it: its own
/// (tag, sidecar, description), then the providers asked by `artist` and
/// `title`, or the other way round under `lyrics.prefer_lrclib`.
///
/// `path` must be a file of the queue of files played without importing
/// them: this reads beside it, and a path the session never opened is not
/// one to read from. Anything else gets the provider lookup alone.
#[tauri::command]
pub async fn fetch_file_lyrics(
    state: tauri::State<'_, AppState>,
    path: String,
    artist: String,
    title: String,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let queued = state
        .external_playback
        .queue()
        .is_some_and(|queue| is_queued(&queue.entries, Path::new(&path)));
    let local_path = queued.then(|| PathBuf::from(&path));
    let prefer_lrclib = {
        let pool = state.require_profile_pool().await?;
        read_prefer_lrclib(&pool).await?
    };

    if !prefer_lrclib {
        if let Some(payload) = local_file_payload(local_path.clone(), track_id).await {
            return Ok(Some(payload));
        }
    }
    let online = fetch_radio_lyrics(state, artist, title, track_id).await?;
    if online.is_some() || !prefer_lrclib {
        return Ok(online);
    }
    Ok(local_file_payload(local_path, track_id).await)
}

/// Whether `path` is one of the files the session opened.
fn is_queued(entries: &[ExternalEntry], path: &Path) -> bool {
    entries.iter().any(|entry| entry.path == path)
}

/// The local tiers on `path`, as a payload for `track_id`, or `None` when
/// there is no path or nothing local. Lofty and the directory listing are
/// blocking, hence the blocking pool.
async fn local_file_payload(path: Option<PathBuf>, track_id: i64) -> Option<LyricsPayload> {
    let path = path?;
    // A share read directly holds `wfnet://` locators, never a file
    // opened from the system; refused all the same rather than handed to
    // lofty as a path.
    if waveflow_core::network_locator::is_locator_path(&path) {
        return None;
    }
    let (content, source) = tokio::task::spawn_blocking(move || read_local_lyrics_of(&path))
        .await
        .ok()
        .flatten()?;
    Some(LyricsPayload {
        track_id,
        format: detect_format(&content),
        content,
        source,
        provider: None,
        tag_write_skipped: None,
        associated: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> ExternalEntry {
        ExternalEntry {
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn only_a_file_the_session_opened_is_read_beside() {
        let entries = [entry("/music/a.flac"), entry("/music/b.flac")];
        assert!(is_queued(&entries, Path::new("/music/b.flac")));
        assert!(!is_queued(&entries, Path::new("/etc/c.flac")));
        assert!(!is_queued(&[], Path::new("/music/a.flac")));
    }

    #[tokio::test]
    async fn the_sidecar_beside_an_opened_file_is_read() {
        // The reported case: the `.lrc` beside a file opened from the
        // system was never read, since only a library row led to it.
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"not really audio").unwrap();
        let lrc = "[00:01.00]la\n[00:02.00]lu\n[00:03.00]li\n";
        std::fs::write(dir.path().join("Song.lrc"), lrc).unwrap();

        let payload = local_file_payload(Some(audio), -7).await.expect("read");
        assert_eq!(payload.track_id, -7);
        assert_eq!(payload.content, lrc);
        assert!(matches!(payload.source, LyricsSource::LrcFile));
        assert!(matches!(payload.format, LyricsFormat::Lrc));
    }

    #[tokio::test]
    async fn no_path_or_nothing_beside_gives_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"not really audio").unwrap();
        assert!(local_file_payload(Some(audio), -7).await.is_none());
        assert!(local_file_payload(None, -7).await.is_none());
    }
}
