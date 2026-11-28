//! This library's tracks, as entries of the queue.

use serde::Serialize;
use sqlx::Row;
use tauri::AppHandle;
use waveflow_core::lan;
use waveflow_core::network_locator;
use waveflow_core::wavesync::{
    Action, Entry, EntryId, Gain, ParticipantId, Position, MAX_FILE_BYTES, MAX_UPCOMING,
};

use super::{Session, WaveSync};
use crate::error::{AppError, AppResult, ShownKind};
use crate::state::AppState;

/// Containers and codecs that a stream read by range cannot play yet
/// (RFC-010 decision 10).
const DSD: &[&str] = &["dsd", "dsf", "dff"];

/// What adding tracks did.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOutcome {
    pub added: usize,
    /// Tracks that cannot be shared: on a network share, DSD, over the size
    /// cap, or whose file is gone.
    pub skipped: usize,
}

/// The entries for `track_ids`, in order, with the track each names, and how
/// many tracks could not be shared.
async fn build(state: &AppState, track_ids: &[i64]) -> AppResult<(Vec<(Entry, i64)>, usize)> {
    let pool = state.require_profile_pool().await?;
    let mut entries = Vec::new();
    let mut skipped = 0;
    for &id in track_ids {
        // The cap is on what goes in, not on what was looked at: unshareable
        // tracks first must not hide shareable ones after them.
        if entries.len() == MAX_UPCOMING {
            skipped += 1;
            continue;
        }
        let row = sqlx::query(
            r#"
            SELECT t.title, t.file_path, t.duration_ms, t.sample_rate, t.channels,
                   t.codec, t.container,
                   t.rg_track_gain_db, t.rg_track_peak, t.rg_album_gain_db, t.rg_album_peak,
                   al.title AS album_title,
                   (SELECT GROUP_CONCAT(name, char(31)) FROM (
                      SELECT ar2.name FROM track_artist ta2
                      JOIN artist ar2 ON ar2.id = ta2.artist_id
                      WHERE ta2.track_id = t.id
                      ORDER BY ta2.position
                   )) AS artists
            FROM track t
            LEFT JOIN album al ON al.id = t.album_id
            WHERE t.id = ? AND t.is_available = 1
            "#,
        )
        .bind(id)
        .fetch_optional(&*pool)
        .await?;
        let Some(row) = row else {
            skipped += 1;
            continue;
        };
        let path: String = row.get("file_path");
        // A track on a network share is not shared at first (decision 10);
        // nor is a file that is no longer there.
        if network_locator::is_locator(&path) {
            skipped += 1;
            continue;
        }
        // A regular file: a directory that took the indexed path's place has
        // a size too, and nothing a player can read.
        let metadata = match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let size = metadata.len();
        let extension = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let codec = [
            row.get::<Option<String>, _>("codec"),
            row.get::<Option<String>, _>("container"),
            Some(extension.clone()),
        ]
        .into_iter()
        .flatten()
        .map(|c| {
            c.to_ascii_lowercase()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(16)
                .collect::<String>()
        })
        .find(|c| !c.is_empty())
        .unwrap_or_else(|| "audio".to_string());
        if size == 0
            || size > MAX_FILE_BYTES
            || DSD.contains(&codec.as_str())
            || DSD.contains(&extension.as_str())
        {
            skipped += 1;
            continue;
        }
        let artists: Option<String> = row.get("artists");
        let gain = Gain {
            track_gain_db: row
                .get::<Option<f64>, _>("rg_track_gain_db")
                .map(|v| v as f32),
            track_peak: row.get::<Option<f64>, _>("rg_track_peak").map(|v| v as f32),
            album_gain_db: row
                .get::<Option<f64>, _>("rg_album_gain_db")
                .map(|v| v as f32),
            album_peak: row.get::<Option<f64>, _>("rg_album_peak").map(|v| v as f32),
        };
        let duration_ms: i64 = row.get("duration_ms");
        entries.push((
            Entry {
                id: EntryId::from_bytes(lan::random()),
                // Set by the host from the connection.
                owner: ParticipantId(0),
                title: row.get("title"),
                artists: artists
                    .map(|a| a.split('\u{1f}').map(str::to_string).collect())
                    .unwrap_or_default(),
                album: row.get("album_title"),
                duration_ms: duration_ms.max(0) as u64,
                codec,
                sample_rate: row
                    .get::<Option<i64>, _>("sample_rate")
                    .filter(|r| *r > 0)
                    .map_or(44_100, |r| r as u32),
                channels: row
                    .get::<Option<i64>, _>("channels")
                    .filter(|c| (1..=8).contains(c))
                    .map_or(2, |c| c as u16),
                size,
                full_hash: None,
                gain: Some(gain),
            },
            id,
        ));
    }
    Ok((entries, skipped))
}

impl WaveSync {
    /// Put `track_ids`, from this library, at the end of the queue of the
    /// session running when the call began.
    pub async fn add(
        &self,
        app: &AppHandle,
        state: &AppState,
        track_ids: &[i64],
    ) -> AppResult<AddOutcome> {
        let (generation, hosting) = {
            let inner = self.lock();
            match inner.session {
                Some(Session::Host(_)) => (inner.generation, true),
                Some(Session::Guest(_)) => (inner.generation, false),
                None => return Err(no_session()),
            }
        };
        let (built, skipped) = build(state, track_ids).await?;
        if built.is_empty() {
            return Err(AppError::shown(
                ShownKind::WavesyncNotShareable,
                "None of these tracks can be shared.",
            ));
        }
        let added = built.len();
        let entries: Vec<Entry> = built.iter().map(|(entry, _)| entry.clone()).collect();
        let action = Action::Add {
            entries,
            position: Position::End,
        };
        if hosting {
            {
                let mut inner = self.lock();
                // The session that was asked, still: a profile switch during
                // the build ends it, and its tracks go nowhere else.
                if inner.generation != generation {
                    return Err(no_session());
                }
                let Some(Session::Host(side)) = inner.session.as_mut() else {
                    return Err(no_session());
                };
                side.act(action).map_err(super::refusal_error)?;
                side.own
                    .extend(built.iter().map(|(entry, track)| (entry.id, *track)));
            }
            self.emit(app);
        } else {
            // Known as this machine's before the request, so that the ops
            // answering it find them, and kept while other ops come first.
            let ids: Vec<EntryId> = built.iter().map(|(entry, _)| entry.id).collect();
            self.with_guest(generation, |side| {
                side.own
                    .extend(built.iter().map(|(entry, track)| (entry.id, *track)));
                side.pending_own.extend(ids.iter().copied());
            })
            .ok_or_else(no_session)?;
            let answered = self.request(generation, action).await;
            self.with_guest(generation, |side| {
                for id in &ids {
                    side.pending_own.remove(id);
                    if answered.is_err() {
                        side.own.remove(id);
                    }
                }
                // Without an answer, what the ops brought in stays, and the
                // rest goes.
                side.prune_own();
            });
            if !answered? {
                self.emit(app);
                return Err(AppError::shown(
                    ShownKind::WavesyncNoAnswer,
                    "The host did not answer.",
                ));
            }
            self.emit(app);
        }
        tracing::info!(added, skipped, "wavesync: tracks added");
        Ok(AddOutcome { added, skipped })
    }

    /// Take `entry` out of the queue.
    pub async fn remove(&self, app: &AppHandle, entry: &str) -> AppResult<()> {
        let entry: EntryId = entry.parse().map_err(|_| {
            AppError::shown(
                ShownKind::WavesyncEntryGone,
                "That track is no longer in the queue.",
            )
        })?;
        let (generation, hosting) = {
            let inner = self.lock();
            match inner.session {
                Some(Session::Host(_)) => (inner.generation, true),
                Some(Session::Guest(_)) => (inner.generation, false),
                None => return Err(no_session()),
            }
        };
        if hosting {
            return self.host_act(app, Action::Remove { entry });
        }
        let answered = self.request(generation, Action::Remove { entry }).await?;
        self.emit(app);
        if !answered {
            return Err(AppError::shown(
                ShownKind::WavesyncNoAnswer,
                "The host did not answer.",
            ));
        }
        Ok(())
    }
}

fn no_session() -> AppError {
    AppError::shown(
        ShownKind::WavesyncNoSession,
        "No WaveSync session is running.",
    )
}
