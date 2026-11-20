//! A track's music video (discussion #1073): the video file named like the
//! track, beside it or in a `Clips` folder beside it, or the track's own
//! file when it is a video. Found on the disk at each asking, by
//! [`waveflow_core::scanner::clips::find_clip`], or on the share a track
//! lies on by [`crate::netlib::clip::find_clip`] (#1133); nothing is stored.
//!
//! The clip plays in the webview with its own sound and its own length.
//! It is not the album version, so nothing ties it to the track's
//! timeline: the frontend pauses the track while it plays and resumes it
//! after. What this module adds besides the lookup is the flag that lets a
//! play / pause from outside the window (media keys, tray, taskbar,
//! global shortcut) reach the clip instead of starting the track under it
//! — see [`crate::player_actions::set_clip_open`] — and the gain that
//! keeps the clip at the track's level ([`get_clip_gain`]).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use waveflow_core::scanner::clips::{self, ClipOrigin};

use crate::audio::AudioEngine;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackClip {
    /// Absolute path of the video, for `convertFileSrc` (or the loopback
    /// server on Linux).
    pub local_path: String,
    pub origin: ClipOrigin,
}

/// Where a library track's clip plays from.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LibraryTrackClip {
    /// A file on this machine.
    Local(TrackClip),
    /// A file on the share the track lies on, which the webview cannot
    /// read: streamed to it from the share by the loopback server
    /// ([`crate::media_loopback::share_url`]).
    #[serde(rename_all = "camelCase")]
    Share { url: String, origin: ClipOrigin },
}

/// `track_id`'s clip, or `None`. A server track asks
/// `get_remote_track_clip`.
#[tauri::command]
pub async fn get_track_clip(
    app: AppHandle,
    state: State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<LibraryTrackClip>> {
    let (row, share) = {
        // Released before the lookup, which can wait on a slow disk or a
        // share: a profile switch waits for every lease to come back. The
        // share is resolved while the lease is held, so that it is this
        // profile's source of that uuid, not the next one's.
        let pool = state.require_profile_pool().await?;
        let row: Option<(String,)> = sqlx::query_as("SELECT file_path FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
        let share = row.as_ref().and_then(|(path,)| {
            let parsed = waveflow_core::network_locator::parse(path)?;
            crate::netlib::source(parsed.source)
        });
        (row, share)
    };
    let Some((file_path,)) = row else {
        return Ok(None);
    };
    if waveflow_core::network_locator::is_locator(&file_path) {
        let Some(source) = share else {
            return Ok(None);
        };
        return Ok(share_clip(&app, source, file_path).await);
    }
    Ok(local_clip(&app, file_path)
        .await
        .map(LibraryTrackClip::Local))
}

/// The clip beside the track at `locator`, on a share, and the address the
/// webview plays it from.
async fn share_clip(
    app: &AppHandle,
    source: std::sync::Arc<crate::netlib::Source>,
    locator: String,
) -> Option<LibraryTrackClip> {
    // A listing or two and a few ranged header reads on the share, each
    // waited on: off the runtime.
    let found =
        tokio::task::spawn_blocking(move || crate::netlib::clip::find_clip(source, &locator))
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(?err, "clip lookup on a share failed");
                None
            })?;
    let url = crate::media_loopback::share_url(app, &found.locator, &found.grant).await?;
    Some(LibraryTrackClip::Share {
        url,
        origin: found.origin,
    })
}

/// The clip beside the local file `file_path`, made readable by the webview.
async fn local_clip(app: &AppHandle, file_path: String) -> Option<TrackClip> {
    if waveflow_core::network_locator::is_locator(&file_path) {
        return None;
    }
    // A directory listing or two and a few header reads: off the runtime,
    // since the folder can be a spun-down drive.
    let found = tokio::task::spawn_blocking(move || clips::find_clip(&PathBuf::from(file_path)))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(?err, "clip lookup task failed");
            None
        });
    let (path, origin) = found?;
    // The file alone, not its folder: the static asset scope names the
    // app's own directories, and an album folder has no reason to become
    // readable by the webview as a whole.
    if let Err(err) = tauri::Manager::asset_protocol_scope(app).allow_file(&path) {
        tracing::warn!(%err, "could not allow a clip in the asset scope");
    }
    Some(TrackClip {
        local_path: path.to_string_lossy().into_owned(),
        origin,
    })
}

/// Where a server track's clip plays from (RFC-029 decision 9).
#[cfg(feature = "sync_v2")]
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteTrackClip {
    /// The clip beside the local file the track was reconciled with: played
    /// from the disk like any local clip.
    Local(TrackClip),
    /// The server's: played through a ticket minted when it opens
    /// ([`crate::remote::clip::ticket`]), since a ticket lasts an hour and an
    /// offer can wait longer than that.
    Server,
}

/// `remote_track_id`'s clip, or `None`. A track reconciled with a local file
/// plays the clip beside that file when it has one; otherwise the server's
/// is offered when it has one ([`crate::remote::clip::on_server`]), asking
/// the server only when the mirror knows nothing and `probe` is set.
#[cfg(feature = "sync_v2")]
#[tauri::command]
pub async fn get_remote_track_clip(
    app: AppHandle,
    state: State<'_, AppState>,
    remote_track_id: String,
    probe: bool,
) -> AppResult<Option<RemoteTrackClip>> {
    let pool = state.require_profile_pool().await?;
    if let Some(path) =
        crate::remote::reconciliation::linked_local_file(&pool, &remote_track_id).await?
    {
        if let Some(clip) = local_clip(&app, path.to_string_lossy().into_owned()).await {
            return Ok(Some(RemoteTrackClip::Local(clip)));
        }
    }
    let on_server = crate::remote::clip::on_server(&state, &pool, &remote_track_id, probe).await?;
    Ok(on_server.then_some(RemoteTrackClip::Server))
}

/// The frontend opened or closed a clip. While one is open, a play / pause
/// from outside the window drives the clip (a `clip:control` event)
/// instead of the track.
#[tauri::command]
pub fn player_set_clip_open(open: bool) {
    crate::player_actions::set_clip_open(open);
}

/// The factor the clip of `track_id` multiplies the volume slider by, so
/// that switching from the track to its clip keeps the level: what the
/// engine puts on that track's samples besides the slider, its ReplayGain
/// (mode, pre-amp, fallback and clipping prevention, by the decoder's own
/// formula) times the normalization — see
/// [`SharedPlayback::clip_gain`](crate::audio::state::SharedPlayback::clip_gain).
///
/// Linear, and above 1 for a boost: a `<video>` cannot play louder than
/// its source, so the frontend caps the product with the slider at 1.
/// `remote` is set for a server row, which the engine plays with no
/// measured gain. Asked again at each `player:loudness-changed`.
#[tauri::command]
pub async fn get_clip_gain(
    state: State<'_, AppState>,
    engine: State<'_, Arc<AudioEngine>>,
    track_id: i64,
    remote: bool,
) -> AppResult<f32> {
    // A clip still plays without a profile (none is open during a switch):
    // what is known about the track is then nothing, as for a server row.
    let library_gain = if remote {
        None
    } else {
        match state.require_profile_pool().await {
            Ok(pool) => Some(crate::commands::player::fetch_replay_gain(&pool, track_id).await),
            Err(_) => None,
        }
    };
    Ok(engine.shared().clip_gain(track_id, library_gain))
}
