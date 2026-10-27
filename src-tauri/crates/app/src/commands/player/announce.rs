//! Announcing a new track: the frontend event, the Discord presence, the
//! OS now-playing card and the Last.fm / ListenBrainz scrobblers.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::{audio::AudioEngine, paths::AppPaths, queue::QueueTrack, state::AppState};

use super::*;

/// Emit `player:track-changed` with the full track payload so the
/// frontend can update the PlayerBar metadata (title, artist, album,
/// cover, duration) at the same moment the decoder starts decoding
/// the new track. Used by every command that kicks off a
/// `LoadAndPlay` plus the analytics task's auto-advance path.
///
/// Also refreshes the system-tray tooltip so right-clicking the tray
/// icon shows what's currently playing without opening the window.
pub(crate) fn emit_track_changed(
    app: &AppHandle,
    paths: &AppPaths,
    track: &QueueTrack,
    profile_id: Option<i64>,
) {
    // A library track is now current, so any remote play queue the user
    // was on is over. This is the single choke point every local-start
    // path funnels through (play_tracks / next / previous / jump /
    // auto-advance / the non-frontend surfaces), which is why the clear
    // lives here rather than being repeated at each one. Remote playback
    // never reaches this function — it drives the engine by URL and never
    // builds a `QueueTrack` — so it never clears itself out from under.
    if let Some(state) = app.try_state::<AppState>() {
        state.remote_playback.clear();
        state.external_playback.clear();
    }
    let payload = queue_track_to_payload_with_paths(paths, track.clone(), profile_id);
    let tooltip = match payload.artist_name.as_deref() {
        Some(artist) if !artist.is_empty() => format!("{} — {}", payload.title, artist),
        _ => payload.title.clone(),
    };
    let _ = app.emit("player:track-changed", payload.clone());
    if let Some(tray) = app.tray_by_id("waveflow") {
        let _ = tray.set_tooltip(Some(tooltip));
    }

    // Push the new track to the OS media overlay (SMTC / MPRIS /
    // MediaRemote). Optional — `init` returns None on platforms
    // where souvlaki failed to start.
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_metadata(
            payload.title,
            payload.artist_name,
            payload.album_title,
            payload.artwork_path,
            payload.duration_ms,
        );
        // Reset the OS progress bar to zero so the overlay's
        // scrubber matches the freshly loaded track. The decoder
        // will follow up with the Loading → Playing transition,
        // which keeps the OS state in sync after the actual
        // first samples reach the device.
        controls.update_playback(crate::audio::PlayerState::Playing, 0);
    }

    // Schedule a Last.fm `track.updateNowPlaying` ping after a short
    // settling delay. The delay filters out rapid skipping — if the
    // user blasts through five tracks looking for the right one, we
    // only announce the one they actually settle on. Best-effort: any
    // failure is logged but never surfaced to the UI.
    schedule_now_playing(app, track);

    // Push the new track to Discord Rich Presence (best-effort,
    // gated on the user-controlled opt-in flag). Done in a tokio
    // task because we need an async DB lookup for the public Deezer
    // cover URL — the local artwork path Discord can't reach.
    schedule_discord_presence(app, track);

    // Optional native OS toast (off by default). Same fire-and-
    // forget pattern as Discord RPC — gated on
    // `app_setting['notifications.track_change']`, no-op when off
    // or when the plugin failed to initialise.
    crate::notifications::schedule(app, track.title.clone(), track.artist_name.clone());
}

/// Spawn a tokio task that resolves the Deezer cover URL for the new
/// track and pushes the metadata to Discord. Held off the synchronous
/// path so the audio engine isn't waiting on SQLite.
fn schedule_discord_presence(app: &AppHandle, track: &QueueTrack) {
    let app = app.clone();
    let track_id = track.id;
    let title = track.title.clone();
    let artist = track.artist_name.clone();
    let album = track.album_title.clone();
    let duration_ms = track.duration_ms;

    tauri::async_runtime::spawn(async move {
        let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>()
        else {
            return;
        };
        let state = app.state::<AppState>();
        let cover_url = match state.require_profile_pool().await {
            Ok(pool) => {
                crate::discord_presence::resolve_cover_url(
                    &pool,
                    &state.paths.metadata_artwork_dir,
                    track_id,
                )
                .await
            }
            Err(_) => None,
        };
        presence.update_metadata(title, artist, album, cover_url, duration_ms, 0);
    });
}

/// Spawn a tokio task that, after a 4 s settling window, announces the
/// track as playing now to Last.fm and ListenBrainz if the user is still
/// on the same track. The settling check is the cheap atomic read of
/// `current_track_id` — no DB hit before we know we'll actually
/// fire the request.
fn schedule_now_playing(app: &AppHandle, track: &QueueTrack) {
    let app = app.clone();
    let track_id = track.id;
    let title = track.title.clone();
    let artist = track.artist_name.clone();
    let album = track.album_title.clone();
    let duration_ms = track.duration_ms;

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        // Still on the same track? If the user skipped during the
        // settling window, current_track_id has moved on — don't
        // announce a track we've already left.
        let engine = app.state::<Arc<AudioEngine>>();
        let still_current = engine
            .shared()
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire)
            == track_id;
        if !still_current {
            return;
        }

        // Need a real artist for either service — both APIs reject calls
        // without one, and our scanner falls back to the file name
        // when tags are empty.
        let Some(artist) = artist.filter(|s| !s.trim().is_empty()) else {
            return;
        };
        // Honour offline mode — skip the now-playing ping; the
        // scrobble queue will also be drained later when offline is off.
        if crate::offline::is_offline() {
            return;
        }

        // Each service on its own: one not being connected must not keep
        // the other from hearing about the track.
        announce_lastfm(&app, &artist, &title, album.as_deref(), duration_ms).await;
        announce_listenbrainz(&app, &artist, &title, album.as_deref(), duration_ms).await;
    });
}

/// Last.fm's `track.updateNowPlaying`, when a session is stored.
async fn announce_lastfm(
    app: &AppHandle,
    artist: &str,
    title: &str,
    album: Option<&str>,
    duration_ms: i64,
) {
    let state = app.state::<AppState>();
    let Ok(Some((api_key, api_secret, session_key, _username))) =
        crate::commands::integration::read_lastfm_credentials(&state).await
    else {
        return;
    };
    let client = waveflow_core::metadata::lastfm::LastfmClient::new();
    let duration_s = if duration_ms > 0 {
        Some(duration_ms / 1000)
    } else {
        None
    };
    match client
        .update_now_playing(
            &api_key,
            &api_secret,
            &session_key,
            artist,
            title,
            album,
            duration_s,
        )
        .await
    {
        Ok(()) => {
            tracing::debug!("lastfm updateNowPlaying ok");
        }
        Err(waveflow_core::metadata::lastfm::LastfmError::Api { code: 9, .. }) => {
            // Same recovery as the scrobbler: wipe the dead
            // session + raise the re-auth banner. Pulling the
            // pool here is best-effort — failure to acquire it
            // means we just drop the event.
            if let Ok(pool) = state.require_profile_pool().await {
                crate::scrobbler::handle_invalid_session(app, &pool).await;
            }
        }
        Err(err) => {
            tracing::warn!(%err, "lastfm updateNowPlaying failed");
        }
    }
}

/// ListenBrainz's "playing now", when a token is stored. Not retried and
/// not queued: it is only true while the track plays.
async fn announce_listenbrainz(
    app: &AppHandle,
    artist: &str,
    title: &str,
    album: Option<&str>,
    duration_ms: i64,
) {
    use waveflow_core::metadata::listenbrainz::{
        Listen, ListenBrainzClient, ListenBrainzError, ListenType,
    };
    let state = app.state::<AppState>();
    // One pool for the read and the forget below, so a refusal drops the
    // token of the profile that sent it.
    let Ok(pool) = state.require_profile_pool().await else {
        return;
    };
    let Ok(Some((token, _username))) =
        crate::commands::integration::read_listenbrainz_credentials(&pool).await
    else {
        return;
    };
    let listen = Listen {
        artist,
        track: title,
        release: album,
        duration_ms: Some(duration_ms),
        track_number: None,
        listened_at: None,
    };
    match ListenBrainzClient::new()
        .submit(&token, ListenType::PlayingNow, &listen)
        .await
    {
        Ok(()) => tracing::debug!("listenbrainz playing_now ok"),
        Err(ListenBrainzError::Unauthorized) => {
            // The token is dead: drop it, as the worker would — only if
            // it is still the stored one.
            if let Err(e) =
                crate::commands::integration::forget_listenbrainz_token(&pool, &token).await
            {
                tracing::warn!(?e, "forget ListenBrainz failed");
            }
        }
        Err(err) => tracing::warn!(%err, "listenbrainz playing_now failed"),
    }
}
