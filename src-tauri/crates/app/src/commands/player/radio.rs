//! Playing a stream URL, and the metadata of the station on air.

use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::{
    audio::{
        engine::{AudioCmd, LoadIntent, Turns},
        replay_gain::TrackGain,
        AudioEngine,
    },
    error::{AppError, AppResult},
    state::AppState,
};

/// Monotonic counter for the synthetic track ids assigned to live
/// radio streams. Negative ids are the canonical "non-library"
/// sentinel — see [`crate::audio::engine::AudioCmd::LoadUrlAndPlay`]
/// docs and the `track_id > 0` filter in `handle_playback_outcome`.
///
/// Starts at `0`; first call returns `-1`. Decrements per call so
/// back-to-back radio loads retain distinct ids — the queue panel /
/// OS overlay use the id as a stable key for their "is this still
/// the active row" check.
static RADIO_TRACK_ID: AtomicI64 = AtomicI64::new(0);

pub(crate) fn next_radio_track_id() -> i64 {
    // `fetch_sub` returns the value *before* the subtraction. We want
    // strictly-negative ids; sub then read.
    RADIO_TRACK_ID.fetch_sub(1, AtomicOrdering::Relaxed) - 1
}

/// Play a live HTTP(S) audio stream through the cpal engine.
///
/// Distinct from `player_play_tracks` because there's no library row,
/// no queue insertion, and no `play_event` credit at the end. The
/// metadata supplied here populates the PlayerBar / OS media overlay
/// directly via a `player:radio-metadata` event the frontend listens
/// to alongside `player:track-changed`.
///
/// `ext_hint` is an optional codec hint ("mp3", "aac", "ogg")
/// forwarded to the symphonia probe via the active stream's
/// `ext_hint`. Frontend derives this from the radio-browser `codec`
/// field when known so the probe locks on faster.
///
/// Scheme is gated to http/https so a maliciously-crafted file:// URL
/// can't get the decoder to open an arbitrary local file via this
/// path. Other paranoia (size limits, redirect caps) is delegated to
/// `HttpMediaSource::open`.
///
/// Answers the synthetic track id, or `None` for a station overtaken by a
/// newer pick before its turn came: nothing of it was published, so the caller
/// has nothing to show as playing.
#[tauri::command]
pub async fn player_play_url(
    app: AppHandle,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    url: String,
    title: Option<String>,
    artist: Option<String>,
    artwork_url: Option<String>,
    ext_hint: Option<String>,
) -> AppResult<Option<i64>> {
    tracing::info!(
        url = %crate::audio::http_source::redact_url(&url),
        title = ?title,
        artist = ?artist,
        ext_hint = ?ext_hint,
        "player_play_url invoked"
    );

    let parsed = url::Url::parse(&url).map_err(|e| AppError::Other(format!("invalid url: {e}")))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(AppError::Other(format!("unsupported scheme: {other}"))),
    }

    // Process-wide offline mode: short-circuit before opening a socket
    // so a deliberately offline session can't be tricked into network
    // chatter by a stale tab.
    if crate::offline::is_offline() {
        tracing::warn!("player_play_url short-circuited: offline mode enabled");
        return Err(AppError::Other("offline mode is enabled".into()));
    }

    let track_id = next_radio_track_id();
    // A station's HTTP probe happens on the decoder thread, but waiting for
    // the publish lock below is not free — and a click on a library track
    // must still be able to overtake this one (#622).
    let intent = engine.next_load_intent();

    let published = publish_on_turn(&**engine, intent, || {
        publish_station(
            &app,
            &engine,
            intent,
            track_id,
            url,
            title,
            artist,
            artwork_url,
            ext_hint,
        )
    })
    .await?;
    Ok(published.then_some(track_id))
}

/// Run `publish` if this intent still has its turn, under the publish lock like
/// every other producer (#632). `false` when something newer has claimed: then
/// nothing is published at all, so the sessions, the OS overlay and the player
/// stay what that newer pick made them. What a station carries beyond this (its
/// time-shift buffer, its titles, an HLS playlist) is opened by the decoder from
/// the load, so a station that never sends one starts none of it, and the one on
/// air keeps its own.
async fn publish_on_turn<E: Turns>(
    engine: &E,
    intent: LoadIntent,
    publish: impl FnOnce() -> AppResult<()>,
) -> AppResult<bool> {
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("station superseded before it started; not playing it");
        return Ok(false);
    }
    publish()?;
    Ok(true)
}

/// Everything a station changes as it takes over, in order.
#[allow(clippy::too_many_arguments)]
fn publish_station(
    app: &AppHandle,
    engine: &AudioEngine,
    intent: LoadIntent,
    track_id: i64,
    url: String,
    title: Option<String>,
    artist: Option<String>,
    artwork_url: Option<String>,
    ext_hint: Option<String>,
) -> AppResult<()> {
    // Starting a radio stream ends any remote play queue. Radio takes the
    // URL path too, so it never passes through `emit_track_changed` where
    // the clear otherwise happens — clear it here explicitly.
    if let Some(state) = app.try_state::<AppState>() {
        state.remote_playback.clear();
        state.external_playback.clear();
    }

    // Push the metadata to the OS overlay before dispatching the
    // command so the SMTC / MPRIS tile updates while the HTTP probe
    // is still settling. The decoder emits `player:radio-metadata`
    // *also* — that event drives the in-app PlayerBar.
    //
    // Cover URL is intentionally left `None`: `update_metadata`'s
    // cover param is treated as a local artwork path and shovelled
    // through the artwork server / `file://` resolver — feeding it an
    // external `https://e-cdns-images.dzcdn.net/…` URL would land it
    // in the wrong code path. The in-app PlayerBar still renders the
    // cover from the `player:radio-metadata` event we emit below.
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_metadata(
            title.clone().unwrap_or_else(|| "Live Radio".to_string()),
            artist.clone(),
            None,
            None,
            // Duration is unknown for live streams — pass 0 so the
            // overlay treats this as an open-ended scrubber.
            0,
        );
        controls.update_playback(crate::audio::PlayerState::Playing, 0);
    }

    tracing::info!(track_id, "dispatching AudioCmd::LoadUrlAndPlay");
    engine.send(AudioCmd::LoadUrlAndPlay {
        intent,
        // Radio: an endless body has no length and no end, so nothing here
        // could ever be published as a complete entry, and nothing to seek.
        cache: None,
        // A live station has no end and therefore no total.
        duration_ms: None,
        seekable_file: false,
        url,
        ext_hint,
        track_id,
        title,
        artist,
        artwork_url,
        // A live station: nothing knows its loudness.
        replay_gain: TrackGain::default(),
    })
}

/// Snapshot the current radio session's metadata, or `None` when no
/// stream is playing. `player_get_state` can't carry this (radio has no
/// library row), so a webview that mounts mid-stream — the mini-player
/// opened after a station started — calls this to hydrate the PlayerBar
/// + the station-favorite star instead of waiting for the next ICY
/// `StreamTitle` change (minutes away). Reads the process-wide snapshot
/// the engine keeps in [`crate::audio::events`].
#[tauri::command]
pub async fn get_current_radio_metadata(
) -> AppResult<Option<crate::audio::events::RadioMetadataPayload>> {
    Ok(crate::audio::events::last_radio_metadata())
}

#[cfg(test)]
mod tests;
