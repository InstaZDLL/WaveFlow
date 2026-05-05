//! OS-level media controls: SMTC on Windows, MPRIS on Linux,
//! MediaRemoteCommandCenter on macOS — all behind a single `souvlaki`
//! frontend. The handle exposed via Tauri state is a thin
//! `crossbeam-channel` wrapper around a dedicated thread that owns the
//! `souvlaki::MediaControls` instance (which is `!Send` on Windows).
//!
//! Update flow:
//! - `commands::player::emit_track_changed` pushes new metadata.
//! - `audio::decoder::transition_state` pushes playback state changes.
//! - `commands::player::player_seek` pushes the new position so the
//!   OS overlay's progress bar resyncs immediately.
//!
//! Event flow (OS keys / overlay buttons):
//! - The souvlaki callback runs on souvlaki's own thread. We forward
//!   each `MediaControlEvent` into Tauri's tokio runtime where the
//!   queue/database side of the player commands is at home.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{unbounded, Sender};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tauri::{AppHandle, Manager};

use crate::{
    audio::{AudioCmd, AudioEngine, PlayerState},
    commands,
    queue::{self, Direction},
    state::AppState,
};

/// Cached metadata held inside the controls thread so we can re-emit on
/// every state transition (souvlaki forgets metadata between some
/// playback updates on Windows otherwise).
#[derive(Default)]
struct CachedMetadata {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    cover_url: Option<String>,
    duration_ms: i64,
}

/// `*mut c_void` (the Win32 HWND we hand to souvlaki) is `!Send`. The
/// pointer is only ever consumed by souvlaki on the dedicated controls
/// thread and the main window outlives that thread (it shuts down with
/// the app), so shipping it across the spawn boundary inside this
/// transparent wrapper is safe.
struct HwndCarrier(Option<*mut std::ffi::c_void>);
unsafe impl Send for HwndCarrier {}

impl HwndCarrier {
    fn into_inner(self) -> Option<*mut std::ffi::c_void> {
        self.0
    }
}

/// Update message sent to the controls thread.
enum Msg {
    Metadata(CachedMetadata),
    Playback {
        state: PlayerState,
        position_ms: u64,
    },
}

/// Handle exposed via `tauri::State`. Cheap to clone.
pub struct MediaControlsHandle {
    tx: Sender<Msg>,
}

impl MediaControlsHandle {
    pub fn update_metadata(
        &self,
        title: String,
        artist: Option<String>,
        album: Option<String>,
        cover_path: Option<String>,
        duration_ms: i64,
    ) {
        let cover_url = build_cover_url(cover_path.as_deref());
        let _ = self.tx.send(Msg::Metadata(CachedMetadata {
            title,
            artist,
            album,
            cover_url,
            duration_ms,
        }));
    }

    pub fn update_playback(&self, state: PlayerState, position_ms: u64) {
        let _ = self.tx.send(Msg::Playback { state, position_ms });
    }
}

/// Resolve a local cover-art path into a URL that the platform's media
/// overlay can actually fetch. MPRIS happily takes a `file://` URI;
/// SMTC's `RandomAccessStreamReference::CreateFromUri` only accepts
/// `http(s)`, `ms-appx`, and `ms-appdata` schemes — passing `file://`
/// makes the entire `set_metadata` call fail (the whole tile drops,
/// not just the thumbnail), so on Windows we deliberately ship the
/// metadata without a cover. Wiring SMTC artwork properly requires
/// either a patched souvlaki or a hand-rolled SMTC binding that uses
/// `CreateFromFile(StorageFile)`.
fn build_cover_url(path: Option<&str>) -> Option<String> {
    let path = path?;
    #[cfg(target_os = "windows")]
    {
        let _ = path; // silence unused warning when the cover is dropped
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        url::Url::from_file_path(path).ok().map(|u| u.to_string())
    }
}

/// Initialize the OS media controls. Returns `None` if the platform
/// integration fails to initialize — playback continues to work, the
/// app just doesn't appear in the OS overlay.
///
/// On Windows we need the main window's `HWND`; on Linux/macOS souvlaki
/// owns its own connection. Must be called after the main window has
/// been created (i.e. inside `tauri::Builder::setup`).
pub fn init(app: AppHandle) -> Option<MediaControlsHandle> {
    #[cfg(target_os = "windows")]
    let hwnd_carrier = {
        let window = app.get_webview_window("main")?;
        match window.hwnd() {
            Ok(h) => HwndCarrier(Some(h.0 as *mut _)),
            Err(err) => {
                tracing::warn!(%err, "media_controls: HWND lookup failed");
                return None;
            }
        }
    };

    #[cfg(not(target_os = "windows"))]
    let hwnd_carrier = HwndCarrier(None);

    let (tx, rx) = unbounded::<Msg>();
    let event_app = app.clone();

    let spawn = std::thread::Builder::new()
        .name("waveflow-media-controls".into())
        .spawn(move || {
            // Consume the whole carrier (`self`-taking method) so the
            // closure captures it as a single Send unit instead of
            // partial-capturing the !Send inner pointer.
            let hwnd = hwnd_carrier.into_inner();
            let config = PlatformConfig {
                dbus_name: "waveflow",
                display_name: "WaveFlow",
                hwnd,
            };

            let mut controls = match MediaControls::new(config) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(?err, "media_controls: init failed");
                    return;
                }
            };

            if let Err(err) = controls.attach(move |event: MediaControlEvent| {
                handle_event(event, event_app.clone());
            }) {
                tracing::warn!(?err, "media_controls: attach failed");
                return;
            }

            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::Metadata(meta) => {
                        push_metadata(&mut controls, &meta);
                    }
                    Msg::Playback { state, position_ms } => {
                        let progress = Some(MediaPosition(Duration::from_millis(position_ms)));
                        let pb = match state {
                            PlayerState::Playing => MediaPlayback::Playing { progress },
                            PlayerState::Paused => MediaPlayback::Paused { progress },
                            // Idle / Loading / Ended → Stopped. Loading
                            // is a brief transition; the next Playing
                            // event lands within ~50 ms.
                            _ => MediaPlayback::Stopped,
                        };
                        if let Err(err) = controls.set_playback(pb) {
                            tracing::warn!(?err, "media_controls: set_playback");
                        }
                    }
                }
            }
        });

    match spawn {
        Ok(_join) => Some(MediaControlsHandle { tx }),
        Err(err) => {
            tracing::warn!(%err, "media_controls: failed to spawn thread");
            None
        }
    }
}

fn push_metadata(controls: &mut MediaControls, cached: &CachedMetadata) {
    let duration = if cached.duration_ms > 0 {
        Some(Duration::from_millis(cached.duration_ms as u64))
    } else {
        None
    };
    let meta = MediaMetadata {
        title: Some(&cached.title),
        artist: cached.artist.as_deref(),
        album: cached.album.as_deref(),
        cover_url: cached.cover_url.as_deref(),
        duration,
    };
    if let Err(err) = controls.set_metadata(meta) {
        tracing::warn!(?err, "media_controls: set_metadata");
    }
}

/// Translate a souvlaki event into the equivalent player command. Runs
/// on souvlaki's callback thread, so anything that touches the per-
/// profile DB pool is dispatched onto Tauri's tokio runtime.
fn handle_event(event: MediaControlEvent, app: AppHandle) {
    match event {
        MediaControlEvent::Play => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.send(AudioCmd::Resume);
        }
        MediaControlEvent::Pause => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.send(AudioCmd::Pause);
        }
        MediaControlEvent::Toggle => {
            let engine = app.state::<Arc<AudioEngine>>();
            let cmd = match engine.shared().state() {
                PlayerState::Playing => AudioCmd::Pause,
                _ => AudioCmd::Resume,
            };
            let _ = engine.send(cmd);
        }
        MediaControlEvent::Stop => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.send(AudioCmd::Stop);
        }
        MediaControlEvent::Next => spawn_next(app),
        MediaControlEvent::Previous => spawn_previous(app),
        MediaControlEvent::SetPosition(MediaPosition(d)) => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.send(AudioCmd::Seek(d.as_millis() as u64));
        }
        MediaControlEvent::Seek(direction) => {
            seek_relative(&app, direction, 5_000);
        }
        MediaControlEvent::SeekBy(direction, delta) => {
            seek_relative(&app, direction, delta.as_millis() as u64);
        }
        // Volume / Raise / Quit / OpenUri aren't wired — souvlaki
        // emits them only when the host app advertises support, which
        // we don't.
        _ => {}
    }
}

fn seek_relative(app: &AppHandle, direction: SeekDirection, delta_ms: u64) {
    let engine = app.state::<Arc<AudioEngine>>();
    let cur = engine.shared().current_position_ms();
    let new_ms = match direction {
        SeekDirection::Forward => cur.saturating_add(delta_ms),
        SeekDirection::Backward => cur.saturating_sub(delta_ms),
    };
    let _ = engine.send(AudioCmd::Seek(new_ms));
}

/// Mirror of `lib.rs::spawn_next` — the OS overlay needs the same
/// queue-advance + emit-track-changed sequence the tray menu uses.
fn spawn_next(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let engine = app.state::<Arc<AudioEngine>>();
        let pool = match state.require_profile_pool().await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(%err, "media_controls next: no profile pool");
                return;
            }
        };
        let profile_id = state.require_profile_id().await.ok();
        let repeat = queue::read_repeat_mode(&pool).await;
        let next = match queue::advance(&pool, Direction::Next, repeat).await {
            Ok(Some(track)) => track,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(%err, "media_controls next: advance failed");
                return;
            }
        };
        commands::player::emit_track_changed(&app, &state.paths, &next, profile_id);
        commands::player::emit_queue_changed(&app);
        let replay_gain_db = commands::player::fetch_replay_gain_db(&pool, next.id).await;
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: next.as_path(),
            start_ms: 0,
            track_id: next.id,
            duration_ms: next.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
            replay_gain_db,
        });
    });
}

/// Mirror of `lib.rs::spawn_previous` — same Spotify rule (seek to 0
/// past 3 s, otherwise jump back a track).
fn spawn_previous(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let engine = app.state::<Arc<AudioEngine>>();
        if engine.shared().current_position_ms() > 3000 {
            let _ = engine.send(AudioCmd::Seek(0));
            return;
        }
        let pool = match state.require_profile_pool().await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(%err, "media_controls previous: no profile pool");
                return;
            }
        };
        let profile_id = state.require_profile_id().await.ok();
        let repeat = queue::read_repeat_mode(&pool).await;
        let prev = match queue::advance(&pool, Direction::Previous, repeat).await {
            Ok(Some(track)) => track,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(%err, "media_controls previous: advance failed");
                return;
            }
        };
        commands::player::emit_track_changed(&app, &state.paths, &prev, profile_id);
        commands::player::emit_queue_changed(&app);
        let replay_gain_db = commands::player::fetch_replay_gain_db(&pool, prev.id).await;
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: prev.as_path(),
            start_ms: 0,
            track_id: prev.id,
            duration_ms: prev.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
            replay_gain_db,
        });
    });
}
