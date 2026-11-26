//! OS-level media controls: SMTC on Windows and
//! MediaRemoteCommandCenter on macOS through `souvlaki`, MPRIS on Linux
//! through our own object (`mpris`) — souvlaki's has no `Shuffle` or
//! `LoopStatus`, so KDE's media widget greyed out both buttons. The
//! handle exposed via Tauri state is a thin `crossbeam-channel` wrapper
//! around a dedicated thread that owns the platform [`Backend`]
//! (souvlaki's `MediaControls` is `!Send` on Windows).
//!
//! Update flow:
//! - `commands::player::emit_track_changed` pushes new metadata.
//! - `audio::decoder::transition_state` pushes playback state changes.
//! - `commands::player::player_seek` pushes the new position so the
//!   OS overlay's progress bar resyncs immediately.
//! - `commands::player::emit_options_changed` — which every change of
//!   shuffle or repeat already goes through, whichever surface made it —
//!   and `player_get_state` (launch, profile switch) push the shuffle and
//!   repeat modes in force.
//! - While playing, the controls thread re-reads the engine's position
//!   every second ([`POSITION_REFRESH`]): MPRIS `Position` is otherwise
//!   frozen at the last pushed value (#807).
//!
//! Event flow (OS keys / overlay buttons):
//! - The platform callback runs on its own thread (souvlaki's, zbus's
//!   executor, or the SMTC event thread). Each request is translated into a [`ControlEvent`] and
//!   handed to [`crate::player_actions`], which does the queue/database
//!   side on Tauri's tokio runtime.
//!
//! Shuffle and repeat, per platform:
//! - Linux: `Shuffle` and `LoopStatus`, read and written. Repeat maps one
//!   to one (off ↔ `None`, all ↔ `Playlist`, one ↔ `Track`). Shuffle is a
//!   boolean on the bus: on reads as either grouping (tracks or whole
//!   albums), and turning it on picks the grouping the listener last
//!   chose, as the player's own button does.
//! - Windows: `ShuffleEnabled` and `AutoRepeatMode` on the SMTC session,
//!   which souvlaki leaves alone, set and watched by `smtc` on the very
//!   session object souvlaki drives. Repeat maps one to one (off ↔ `None`,
//!   all ↔ `List`, one ↔ `Track`).
//! - macOS: shuffle only — souvlaki publishes `changeShuffleModeCommand`
//!   but no repeat command.
//!
//! Cover art on Windows:
//! - SMTC's `RandomAccessStreamReference::CreateFromUri` only accepts
//!   `http(s)`, `ms-appx`, `ms-appdata` schemes. `file://` makes the
//!   entire `set_metadata` call fail. To work around this we run a
//!   tiny tiny_http server on `127.0.0.1:<random>` that serves files
//!   from a whitelist registry built up as covers are advertised, and
//!   hand SMTC `http://127.0.0.1:<port>/artwork/<basename>` URLs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{unbounded, RecvTimeoutError, Sender};
use tauri::{AppHandle, Manager};

use crate::audio::{AudioEngine, PlayerState};
use crate::queue::RepeatMode;

#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "windows")]
mod smtc;

/// What the OS overlay asked of the player, whichever platform said it.
#[derive(Debug, Clone, PartialEq)]
enum ControlEvent {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    SetPosition(Duration),
    /// A relative move, in milliseconds, negative for backwards.
    SeekBy(i64),
    SetShuffle(bool),
    /// Asked by MPRIS and SMTC; souvlaki has no repeat command on macOS.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    SetRepeat(RepeatMode),
}

/// How often the controls thread re-publishes the position while playing.
///
/// The MPRIS object answers `Position` with the last value it was handed
/// and never advances it, and we hand it one only on a state change or a
/// seek.
/// `playerctl position`, and every lyrics client built on it, read the
/// track's start for the whole track (#807). Once a second is the cadence
/// of the clients that poll it, and each publish is one D-Bus signal.
const POSITION_REFRESH: Duration = Duration::from_secs(1);

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
    Options {
        shuffle: bool,
        repeat: RepeatMode,
    },
}

/// Whitelist of cover-art files the local artwork server is allowed to
/// serve. Keyed by URL-safe basename (e.g. `<blake3>.jpeg`); SMTC fetches
/// `/artwork/<basename>` and the server resolves to the absolute path.
type ArtworkRegistry = Arc<std::sync::RwLock<HashMap<String, PathBuf>>>;

/// Handle exposed via `tauri::State`. Cheap to clone.
pub struct MediaControlsHandle {
    tx: Sender<Msg>,
    /// Port + registry of the local artwork server. `None` when the
    /// server failed to bind (then covers fall back to `file://` on
    /// non-Windows or to no cover on Windows).
    #[allow(dead_code)] // unused on non-Windows targets
    artwork: Option<ArtworkServer>,
}

// The struct's existence (Some vs None) is what gates the cover URL
// path in `update_metadata` — the fields are kept for future debug
// logging and to keep the type recognisable at a glance.
#[derive(Clone)]
#[allow(dead_code)]
struct ArtworkServer {
    port: u16,
    registry: ArtworkRegistry,
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
        let cover_url = build_cover_url(cover_path.as_deref(), self.artwork.as_ref());
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

    /// The shuffle and repeat modes in force. `shuffle` is whether it is
    /// on at all: no platform tells tracks from albums.
    pub fn update_options(&self, shuffle: bool, repeat: RepeatMode) {
        let _ = self.tx.send(Msg::Options { shuffle, repeat });
    }
}

/// The platform's media controls, as the controls thread drives them.
#[cfg(not(target_os = "linux"))]
struct Backend {
    controls: souvlaki::MediaControls,
    /// Shuffle and repeat on the same SMTC session. `None` when the
    /// session would not take the handlers: the rest of the overlay still
    /// works without them.
    #[cfg(target_os = "windows")]
    options: Option<smtc::Options>,
}

#[cfg(target_os = "linux")]
struct Backend(mpris::Mpris);

#[cfg(not(target_os = "linux"))]
type BackendError = souvlaki::Error;

#[cfg(target_os = "linux")]
type BackendError = zbus::Error;

#[cfg(not(target_os = "linux"))]
impl Backend {
    fn start(hwnd: Option<*mut std::ffi::c_void>, app: AppHandle) -> Option<Self> {
        let config = souvlaki::PlatformConfig {
            dbus_name: "waveflow",
            display_name: "WaveFlow",
            hwnd,
        };
        let mut controls = match souvlaki::MediaControls::new(config) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(?err, "media_controls: init failed");
                return None;
            }
        };
        let souvlaki_app = app.clone();
        if let Err(err) = controls.attach(move |event: souvlaki::MediaControlEvent| {
            if let Some(event) = from_souvlaki(event) {
                handle_event(event, souvlaki_app.clone());
            }
        }) {
            tracing::warn!(?err, "media_controls: attach failed");
            return None;
        }
        #[cfg(target_os = "windows")]
        let options = hwnd.and_then(|hwnd| {
            let handler = move |request| {
                let event = match request {
                    smtc::Request::Shuffle(on) => ControlEvent::SetShuffle(on),
                    smtc::Request::Repeat(mode) => ControlEvent::SetRepeat(mode),
                };
                handle_event(event, app.clone());
            };
            smtc::Options::attach(hwnd, handler)
                .inspect_err(|err| tracing::warn!(%err, "media_controls: shuffle / repeat"))
                .ok()
        });
        #[cfg(not(target_os = "windows"))]
        let _ = app;
        Some(Self {
            controls,
            #[cfg(target_os = "windows")]
            options,
        })
    }

    fn set_metadata(&mut self, cached: &CachedMetadata) {
        let duration =
            (cached.duration_ms > 0).then(|| Duration::from_millis(cached.duration_ms as u64));
        let meta = souvlaki::MediaMetadata {
            title: Some(&cached.title),
            artist: cached.artist.as_deref(),
            album: cached.album.as_deref(),
            cover_url: cached.cover_url.as_deref(),
            duration,
        };
        if let Err(err) = self.controls.set_metadata(meta) {
            tracing::warn!(?err, "media_controls: set_metadata");
        }
    }

    fn set_playback(&mut self, state: PlayerState, position_ms: u64) -> Result<(), BackendError> {
        use souvlaki::{MediaPlayback, MediaPosition};
        let progress = Some(MediaPosition(Duration::from_millis(position_ms)));
        let pb = match state {
            PlayerState::Playing => MediaPlayback::Playing { progress },
            PlayerState::Paused => MediaPlayback::Paused { progress },
            // Idle / Loading / Ended → Stopped. Loading is a brief
            // transition; the next Playing event lands within ~50 ms.
            _ => MediaPlayback::Stopped,
        };
        self.controls.set_playback(pb)
    }

    fn set_options(&mut self, shuffle: bool, repeat: RepeatMode) {
        #[cfg(target_os = "windows")]
        if let Some(options) = &self.options {
            if let Err(err) = options.set(shuffle, repeat) {
                tracing::warn!(%err, "media_controls: set_options");
            }
        }
        // souvlaki's shuffle command, and no repeat one.
        #[cfg(target_os = "macos")]
        {
            let _ = repeat;
            if let Err(err) = self.controls.set_shuffle(Some(shuffle)) {
                tracing::warn!(?err, "media_controls: set_shuffle");
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = (shuffle, repeat);
    }
}

/// souvlaki's events in our vocabulary. `None` for what the player does
/// not handle (volume, raise, quit, open URI).
#[cfg(not(target_os = "linux"))]
fn from_souvlaki(event: souvlaki::MediaControlEvent) -> Option<ControlEvent> {
    use souvlaki::{MediaControlEvent as E, MediaPosition, SeekDirection};
    let signed = |direction: SeekDirection, ms: i64| match direction {
        SeekDirection::Forward => ms,
        SeekDirection::Backward => -ms,
    };
    Some(match event {
        E::Play => ControlEvent::Play,
        E::Pause => ControlEvent::Pause,
        E::Toggle => ControlEvent::Toggle,
        E::Stop => ControlEvent::Stop,
        E::Next => ControlEvent::Next,
        E::Previous => ControlEvent::Previous,
        E::SetPosition(MediaPosition(d)) => ControlEvent::SetPosition(d),
        E::Seek(direction) => ControlEvent::SeekBy(signed(direction, 5_000)),
        E::SeekBy(direction, d) => ControlEvent::SeekBy(signed(
            direction,
            i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        )),
        // Emitted on macOS only, once `set_options` has published a state;
        // Windows' shuffle and repeat come through `smtc`.
        E::SetShuffle(on) => ControlEvent::SetShuffle(on),
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
impl Backend {
    fn start(_hwnd: Option<*mut std::ffi::c_void>, app: AppHandle) -> Option<Self> {
        let handler: mpris::Handler = Arc::new(move |request| {
            handle_event(from_mpris(request), app.clone());
        });
        match mpris::Mpris::start("waveflow", "WaveFlow", handler) {
            Ok(mpris) => Some(Self(mpris)),
            Err(err) => {
                tracing::warn!(%err, "media_controls: init failed");
                None
            }
        }
    }

    fn set_metadata(&mut self, cached: &CachedMetadata) {
        let metadata = mpris::Metadata {
            title: Some(cached.title.clone()),
            artist: cached.artist.clone(),
            album: cached.album.clone(),
            art_url: cached.cover_url.clone(),
            length_us: (cached.duration_ms > 0).then(|| cached.duration_ms.saturating_mul(1_000)),
        };
        if let Err(err) = self.0.set_metadata(metadata) {
            tracing::warn!(%err, "media_controls: set_metadata");
        }
    }

    fn set_playback(&mut self, state: PlayerState, position_ms: u64) -> Result<(), BackendError> {
        let status = match state {
            PlayerState::Playing => mpris::Status::Playing,
            PlayerState::Paused => mpris::Status::Paused,
            // Idle / Loading / Ended → Stopped. Loading is a brief
            // transition; the next Playing event lands within ~50 ms.
            _ => mpris::Status::Stopped,
        };
        let position_us = i64::try_from(position_ms)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000);
        self.0.set_playback(status, position_us)
    }

    fn set_options(&mut self, shuffle: bool, repeat: RepeatMode) {
        if let Err(err) = self.0.set_options(shuffle, loop_status(repeat)) {
            tracing::warn!(%err, "media_controls: set_options");
        }
    }
}

/// WaveFlow's repeat modes as MPRIS names them. One to one: the two
/// vocabularies describe the same three behaviours.
#[cfg(target_os = "linux")]
fn loop_status(repeat: RepeatMode) -> mpris::LoopStatus {
    match repeat {
        RepeatMode::Off => mpris::LoopStatus::None,
        RepeatMode::All => mpris::LoopStatus::Playlist,
        RepeatMode::One => mpris::LoopStatus::Track,
    }
}

#[cfg(target_os = "linux")]
fn repeat_mode(status: mpris::LoopStatus) -> RepeatMode {
    match status {
        mpris::LoopStatus::None => RepeatMode::Off,
        mpris::LoopStatus::Playlist => RepeatMode::All,
        mpris::LoopStatus::Track => RepeatMode::One,
    }
}

#[cfg(target_os = "linux")]
fn from_mpris(request: mpris::Request) -> ControlEvent {
    use mpris::Request as R;
    match request {
        R::Play => ControlEvent::Play,
        R::Pause => ControlEvent::Pause,
        R::PlayPause => ControlEvent::Toggle,
        R::Stop => ControlEvent::Stop,
        R::Next => ControlEvent::Next,
        R::Previous => ControlEvent::Previous,
        R::Seek(offset_us) => ControlEvent::SeekBy(offset_us / 1_000),
        // `mpris` drops a negative position before it gets here.
        R::SetPosition(position_us) => {
            ControlEvent::SetPosition(Duration::from_micros(position_us.max(0) as u64))
        }
        R::SetShuffle(on) => ControlEvent::SetShuffle(on),
        R::SetLoopStatus(status) => ControlEvent::SetRepeat(repeat_mode(status)),
    }
}

/// Re-publish the live position, read from the engine rather than
/// extrapolated here: the engine's clock already accounts for the playback
/// speed, a seek and a crossfade hand-off.
fn refresh_position(controls: &mut Backend, app: &AppHandle) {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return;
    };
    let shared = engine.shared();
    // The state message for a pause may still be in flight: never publish
    // a Playing the engine has already left.
    if shared.state() != PlayerState::Playing {
        return;
    }
    if let Err(err) = controls.set_playback(PlayerState::Playing, shared.current_position_ms()) {
        tracing::debug!(?err, "media_controls: position refresh");
    }
}

/// Resolve a local cover-art path into a URL the OS media overlay can
/// fetch. On Windows we register the file with the local artwork
/// server and return its `http://127.0.0.1:<port>/artwork/<basename>`
/// URL. On Linux/macOS MPRIS / MediaRemote take `file://` directly.
fn build_cover_url(path: Option<&str>, artwork: Option<&ArtworkServer>) -> Option<String> {
    let path = path?;

    #[cfg(target_os = "windows")]
    {
        let server = artwork?;
        let pb = PathBuf::from(path);
        let basename = pb.file_name()?.to_str()?.to_string();
        // Register the absolute path so the HTTP handler can resolve
        // it. Same-basename overwrites are fine — covers are content-
        // hashed so the path under a given basename is stable.
        if let Ok(mut map) = server.registry.write() {
            map.insert(basename.clone(), pb);
        }
        // Basenames are blake3 hex + a known extension, so no URL
        // encoding is needed in practice.
        Some(format!(
            "http://127.0.0.1:{}/artwork/{}",
            server.port, basename
        ))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = artwork; // server isn't spawned on these platforms
        url::Url::from_file_path(path).ok().map(|u| u.to_string())
    }
}

/// Bind a tiny localhost HTTP server that serves cover art to SMTC.
/// Returns `None` if binding fails — callers fall back to no cover.
///
/// Only spawned on Windows because MPRIS / MediaRemote consume
/// `file://` directly and there's no benefit to opening a TCP port on
/// those platforms (and Snap/Flatpak sandboxes may forbid it).
#[cfg(target_os = "windows")]
fn spawn_artwork_server() -> Option<ArtworkServer> {
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "artwork server: bind failed");
            return None;
        }
    };
    let port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => {
            tracing::warn!("artwork server: missing TCP address");
            return None;
        }
    };
    let registry: ArtworkRegistry = Arc::new(std::sync::RwLock::new(HashMap::new()));
    let registry_for_thread = registry.clone();

    let spawn = std::thread::Builder::new()
        .name("waveflow-artwork-http".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let response = serve_artwork(&registry_for_thread, request.url());
                let _ = match response {
                    Some((bytes, content_type)) => {
                        let header = tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            content_type.as_bytes(),
                        )
                        .ok();
                        let mut resp = tiny_http::Response::from_data(bytes);
                        if let Some(h) = header {
                            resp = resp.with_header(h);
                        }
                        request.respond(resp)
                    }
                    None => request.respond(tiny_http::Response::empty(404)),
                };
            }
        });

    if let Err(err) = spawn {
        tracing::warn!(%err, "artwork server: thread spawn failed");
        return None;
    }

    tracing::info!(port, "artwork server bound on 127.0.0.1");
    Some(ArtworkServer { port, registry })
}

/// Resolve a `/artwork/<basename>` request to the file bytes plus a
/// content-type guess, or `None` for any unknown / malformed path.
#[cfg(target_os = "windows")]
fn serve_artwork(registry: &ArtworkRegistry, url: &str) -> Option<(Vec<u8>, &'static str)> {
    // tiny_http URLs include the query string; strip it.
    let path_only = url.split('?').next().unwrap_or(url);
    let basename = path_only.strip_prefix("/artwork/")?;
    // Reject any path traversal — registry keys are flat basenames.
    if basename.contains('/') || basename.contains('\\') || basename.contains("..") {
        return None;
    }
    let abs = {
        let map = registry.read().ok()?;
        map.get(basename).cloned()?
    };
    let bytes = std::fs::read(&abs).ok()?;
    let ext = abs
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let content_type = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    };
    Some((bytes, content_type))
}

/// Initialize the OS media controls. Returns `None` if the platform
/// integration fails to initialize — playback continues to work, the
/// app just doesn't appear in the OS overlay.
///
/// On Windows we need the main window's `HWND`; on macOS souvlaki and on
/// Linux the MPRIS object own their own connection. Must be called after the main window has
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

    #[cfg(target_os = "windows")]
    let artwork = spawn_artwork_server();

    #[cfg(not(target_os = "windows"))]
    let artwork: Option<ArtworkServer> = None;

    let (tx, rx) = unbounded::<Msg>();
    let event_app = app.clone();
    let event_app_for_ticks = app.clone();

    let spawn = std::thread::Builder::new()
        .name("waveflow-media-controls".into())
        .spawn(move || {
            // Consume the whole carrier (`self`-taking method) so the
            // closure captures it as a single Send unit instead of
            // partial-capturing the !Send inner pointer.
            let hwnd = hwnd_carrier.into_inner();
            let Some(mut controls) = Backend::start(hwnd, event_app) else {
                return;
            };

            let tick_app = event_app_for_ticks;
            // Whether the last state handed over was Playing: a paused or
            // stopped position does not move, so it needs no refresh.
            let mut playing = false;
            loop {
                let msg = match rx.recv_timeout(POSITION_REFRESH) {
                    Ok(msg) => msg,
                    Err(RecvTimeoutError::Timeout) => {
                        if playing {
                            refresh_position(&mut controls, &tick_app);
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                match msg {
                    Msg::Metadata(meta) => controls.set_metadata(&meta),
                    Msg::Playback { state, position_ms } => {
                        playing = state == PlayerState::Playing;
                        if let Err(err) = controls.set_playback(state, position_ms) {
                            tracing::warn!(?err, "media_controls: set_playback");
                        }
                    }
                    Msg::Options { shuffle, repeat } => controls.set_options(shuffle, repeat),
                }
            }
        });

    match spawn {
        Ok(_join) => Some(MediaControlsHandle { tx, artwork }),
        Err(err) => {
            tracing::warn!(%err, "media_controls: failed to spawn thread");
            None
        }
    }
}

/// Translate an overlay request into the equivalent player command. Runs
/// on the platform's callback thread, so anything that touches the per-
/// profile DB pool is dispatched onto Tauri's tokio runtime.
fn handle_event(event: ControlEvent, app: AppHandle) {
    match event {
        // Both go through `player_actions`: a bare `AudioCmd::Resume` is
        // dropped by the decoder when no track is open, which left Play
        // dead on the overlay after a launch and at the end of the queue
        // (#609). `play` resumes or loads the resume point and never
        // pauses; `toggle_play_pause` is the rule the tray follows.
        ControlEvent::Play => crate::player_actions::play(&app, "media_controls"),
        // The open music video's, when there is one (#1073).
        ControlEvent::Pause => crate::player_actions::pause(&app),
        ControlEvent::Toggle => crate::player_actions::toggle_play_pause(&app, "media_controls"),
        // An open music video closes with it (#1073).
        ControlEvent::Stop => crate::player_actions::stop(&app),
        ControlEvent::Next => spawn_next(app),
        ControlEvent::Previous => spawn_previous(app),
        ControlEvent::SetPosition(d) => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.seek(d.as_millis() as u64);
        }
        ControlEvent::SeekBy(delta_ms) => seek_relative(&app, delta_ms),
        // Saved first, then published back to the overlay by
        // `emit_options_changed`, the way a click in the player is: the
        // overlay shows the new mode only once it is the mode in force.
        ControlEvent::SetShuffle(on) => {
            tauri::async_runtime::spawn(async move {
                if let Err(err) = crate::player_actions::set_shuffle(&app, on).await {
                    tracing::warn!(%err, "media_controls: shuffle not changed");
                }
            });
        }
        ControlEvent::SetRepeat(mode) => {
            tauri::async_runtime::spawn(async move {
                if let Err(err) = crate::player_actions::set_repeat(&app, mode).await {
                    tracing::warn!(%err, "media_controls: repeat not changed");
                }
            });
        }
    }
}

fn seek_relative(app: &AppHandle, delta_ms: i64) {
    let engine = app.state::<Arc<AudioEngine>>();
    let cur = engine.position_ms();
    let new_ms = if delta_ms >= 0 {
        cur.saturating_add(delta_ms.unsigned_abs())
    } else {
        cur.saturating_sub(delta_ms.unsigned_abs())
    };
    let _ = engine.seek(new_ms);
}

/// Queue advance for the OS overlay.
///
/// Thin wrappers over [`crate::player_actions`], which the tray menu
/// and the MPD server share: the sequence (advance, emit
/// `player:track-changed`, emit `player:queue-changed`, hand the track
/// to the decoder) used to be copy-pasted here and in `lib.rs`, and the
/// two had already drifted. The platform calls us from its own thread,
/// so the async work is spawned onto Tauri's runtime.
fn spawn_next(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        crate::player_actions::next(&app, "media_controls").await;
    });
}

/// Same Spotify rule (seek to 0 past 3 s, otherwise jump back a track),
/// implemented once in [`crate::player_actions::previous`].
fn spawn_previous(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        crate::player_actions::previous(&app, "media_controls").await;
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Each repeat mode has exactly one `LoopStatus`, and back: a mode
    /// set from the widget is the one the widget then shows.
    #[test]
    fn repeat_modes_and_loop_statuses_map_one_to_one() {
        let pairs = [
            (RepeatMode::Off, mpris::LoopStatus::None),
            (RepeatMode::All, mpris::LoopStatus::Playlist),
            (RepeatMode::One, mpris::LoopStatus::Track),
        ];
        for (mode, status) in pairs {
            assert_eq!(loop_status(mode), status);
            assert_eq!(repeat_mode(status), mode);
        }
    }

    #[test]
    fn mpris_requests_become_player_events() {
        use mpris::Request as R;
        assert_eq!(
            from_mpris(R::SetShuffle(true)),
            ControlEvent::SetShuffle(true)
        );
        assert_eq!(
            from_mpris(R::SetLoopStatus(mpris::LoopStatus::Track)),
            ControlEvent::SetRepeat(RepeatMode::One)
        );
        assert_eq!(from_mpris(R::PlayPause), ControlEvent::Toggle);
        // Microseconds on the bus, milliseconds in the player, the sign kept.
        assert_eq!(
            from_mpris(R::Seek(-5_000_000)),
            ControlEvent::SeekBy(-5_000)
        );
        assert_eq!(
            from_mpris(R::SetPosition(12_345_000)),
            ControlEvent::SetPosition(Duration::from_millis(12_345))
        );
    }
}
