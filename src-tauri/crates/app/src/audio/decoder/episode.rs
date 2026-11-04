//! Decoding a podcast episode: the one load whose speed is its own.

use std::sync::atomic::Ordering;

use crossbeam_channel::Receiver;
use rtrb::Producer;
use serde::Serialize;
use symphonia::core::io::MediaSource;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedSender;
use waveflow_core::playback::replay_gain::TrackGain;
use waveflow_core::playback::stream::ActiveStream;

use super::super::analytics::AnalyticsMsg;
use super::super::engine::AudioCmd;
use super::super::events::{emit_radio_metadata, RadioMetadataPayload};
use super::super::state::{PlayerState, SharedPlayback};
use super::{
    handle_playback_outcome, restore_pcm_output, transition_state, unpause_and_drain_ring,
    was_released, ErrorPayload, PlaybackEnd, EVENT_ERROR,
};
use crate::podcast::playback::{EpisodeLoad, EpisodeSource, SOURCE_TYPE};
use crate::podcast::stream;

/// Tauri event: the engine changed the speed by itself, because an episode took
/// or gave back its own. A slider moved by hand needs no echo.
const EVENT_SPEED_CHANGED: &str = "player:speed-changed";

#[derive(Serialize, Clone)]
struct SpeedChangedPayload {
    speed: f32,
}

/// An episode's speed, held for as long as it decodes.
///
/// Held here rather than restored by whoever plays next, of which there are many:
/// the queue, an auto-advance, a media key, the tray, MPD, a station, a file, a
/// server track, a rebuild. Whichever way an episode ends, a panic included,
/// everything after it finds the user's speed in place and never knew.
struct HeldSpeed<'a> {
    shared: &'a SharedPlayback,
    app: &'a AppHandle,
}

impl<'a> HeldSpeed<'a> {
    fn new(shared: &'a SharedPlayback, app: &'a AppHandle, speed: f32) -> Self {
        let before = shared.playback_speed();
        shared.hold_episode_speed(speed);
        announce_speed(app, before, shared.playback_speed());
        Self { shared, app }
    }
}

impl Drop for HeldSpeed<'_> {
    fn drop(&mut self) {
        let before = self.shared.playback_speed();
        self.shared.release_episode_speed();
        announce_speed(self.app, before, self.shared.playback_speed());
    }
}

fn announce_speed(app: &AppHandle, before: f32, now: f32) {
    if (before - now).abs() > f32::EPSILON {
        let _ = app.emit(EVENT_SPEED_CHANGED, SpeedChangedPayload { speed: now });
    }
}

/// The episode as the player bar shows it. Nothing here is an address: there is
/// no station to favourite, and its picture is a file of ours.
fn metadata(load: &EpisodeLoad, duration_ms: u64) -> RadioMetadataPayload {
    RadioMetadataPayload {
        track_id: load.track_id,
        title: Some(load.title.clone()),
        artist: Some(load.channel.clone()),
        artwork_url: load.artwork_path.clone(),
        station_url: None,
        station_name: Some(load.channel.clone()),
        station_artist: None,
        station_artwork: load.artwork_path.clone(),
        is_remote: false,
        duration_ms: Some(duration_ms as i64),
        artwork_hash: None,
        remote_track_id: None,
        is_file: false,
        file_path: None,
        is_podcast: true,
        podcast_episode_id: Some(load.episode_id),
        podcast_channel_id: Some(load.channel_id),
    }
}

/// Opens what an episode plays from. The message of an error is one word, or the
/// phrase that says a command let go of it.
fn open_stream(
    load: &EpisodeLoad,
    shared: &SharedPlayback,
    app: &AppHandle,
) -> Result<ActiveStream, String> {
    match &load.source {
        EpisodeSource::File(path) => ActiveStream::open(
            path,
            load.track_id,
            load.duration_ms,
            SOURCE_TYPE.to_string(),
            None,
            TrackGain::default(),
            shared.dsd_taps.load(Ordering::Acquire) as usize,
            false,
        ),
        EpisodeSource::Stream {
            enclosure,
            origin,
            declared_type,
        } => {
            let source =
                crate::podcast::playback::open_stream(app, load.profile_id, enclosure, *origin)
                    .map_err(|refusal| refusal.message())?;
            // What the host says it is, or else what the feed said.
            let hint = stream::ext_hint(source.content_type())
                .or_else(|| stream::ext_hint(declared_type.as_deref()));
            let mut opened = ActiveStream::open_from_source(
                Box::new(source) as Box<dyn MediaSource>,
                hint,
                load.track_id,
                load.duration_ms,
                SOURCE_TYPE.to_string(),
                None,
                TrackGain::default(),
            )?;
            // Over a network, an accurate seek into an MP3 reads the file up to
            // there: most of an hour's download to resume near its end.
            opened.coarse_seek = true;
            Ok(opened)
        }
    }
}

/// Plays one episode to its end or until something replaces it.
#[allow(clippy::too_many_arguments)]
pub(super) fn play_episode(
    load: EpisodeLoad,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    cmd_rx: &Receiver<AudioCmd>,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
    analytics_tx: &UnboundedSender<AnalyticsMsg>,
) {
    shared.clear_ab_loop();
    transition_state(shared, app, PlayerState::Loading, Some(load.track_id));
    unpause_and_drain_ring(producer, shared);
    shared.restart_clock(load.start_ms, 0, false);
    shared
        .current_track_id
        .store(load.track_id, Ordering::Release);
    if let Err(err) = restore_pcm_output(app, producer) {
        tracing::warn!(%err, "no output for this episode");
        let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
        transition_state(shared, app, PlayerState::Idle, Some(load.track_id));
        return;
    }

    // Up front, so the bar has the episode during the handshake.
    emit_radio_metadata(app, metadata(&load, load.duration_ms));

    // From before the open: `play_track` reads it when the stream starts.
    let _speed = HeldSpeed::new(shared, app, load.speed);

    let mut stream = match open_stream(&load, shared, app) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "episode open failed");
            // A probe reads a read let go as the end of the file and says the
            // format is unknown, so a command waiting is the better witness.
            if !was_released(&err) && cmd_rx.is_empty() {
                let _ = app.emit(EVENT_ERROR, ErrorPayload::of("stream-failed", err));
            }
            transition_state(shared, app, PlayerState::Idle, Some(load.track_id));
            return;
        }
    };

    // A feed that does not state a length leaves it to the container, and a seekbar
    // that says so once it is known.
    let mut duration_ms = load.duration_ms;
    if duration_ms == 0 {
        duration_ms = stream.container_duration_ms.unwrap_or(0);
        stream.duration_ms = duration_ms;
        if duration_ms > 0 {
            emit_radio_metadata(app, metadata(&load, duration_ms));
        }
    }

    let outcome = super::track::play_track(
        stream,
        load.start_ms,
        producer,
        shared,
        cmd_rx,
        app,
        pending_cmd,
        analytics_tx,
    );

    // Read while the speed is still this episode's: the clock scales by it.
    let position_ms = shared.current_position_ms();
    let finished = matches!(outcome, Ok((PlaybackEnd::Natural, _, _)));
    // Ahead of the end's own message, which may step to the next episode.
    let _ = analytics_tx.send(AnalyticsMsg::EpisodeLeft {
        profile_id: load.profile_id,
        episode_id: load.episode_id,
        channel_id: load.channel_id,
        position_ms,
        duration_ms,
        finished,
    });
    handle_playback_outcome(outcome, shared, app, analytics_tx, None);
}
