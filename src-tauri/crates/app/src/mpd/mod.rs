//! MPD protocol server — control WaveFlow from any MPD client.
//!
//! MPD is a plain-text TCP protocol that has been the Linux
//! remote-control lingua franca since 2003. Speaking it hands us a
//! large parc of already-written clients for free: MALP and MaximumMPD
//! on a phone, `mpc` and ncmpcpp in a terminal, waybar status modules,
//! Home Assistant. In particular it is **a phone remote without any
//! mobile UI of our own** (issue #471).
//!
//! # How it fits with what we already ship
//!
//! - [`crate::media_controls`] (MPRIS / SMTC) — OS media keys, same machine.
//! - [`crate::dlna`] — **serves** our files to a receiver on the LAN.
//! - This — **controls** our player, from anywhere on the LAN.
//!
//! # Bind address
//!
//! `0.0.0.0`, matching [`crate::dlna`]. The `mpd.enabled` flag (off by
//! default) *is* the security decision. This is deliberate and settled
//! in #471: the DLNA server already binds every interface with no
//! authentication at all and exposes strictly more (it will hand over
//! the audio files themselves), so gating the less-exposing service
//! behind more locks would be incoherent — and on loopback the feature
//! loses its entire point, since the phone remote is what justifies it.
//!
//! The accepted risk is that MPD grants *write* access where DLNA is
//! read-only: anyone on the LAN can pause playback or change the
//! volume. There's no arbitrary audio-file read/download (unlike DLNA,
//! which serves the files) and no command/shell execution — but the
//! responses DO expose the queued tracks' file paths + metadata
//! (`playlistinfo` / `currentsong`), so those are visible on the LAN.
//! `mpd.password` exists for users on a shared network.
//!
//! # Architecture
//!
//! Same dedicated-thread + crossbeam-channel shape as [`crate::dlna`]:
//! a [`MpdServer`] handle ferries `Start` / `Stop` / `Status` to a
//! worker owning its own tokio runtime, so the rest of the app keeps a
//! sync API. Each accepted socket becomes a task in [`connection`].
//!
//! Unlike a webview-hosted player, our audio engine is in-process: the
//! dispatcher reads [`crate::audio::state::SharedPlayback`] atomics
//! directly and drives playback through [`crate::player_actions`]. No
//! request/response bridge to the UI, so the server keeps working with
//! the window closed to tray — which is exactly when a remote matters.

pub mod commands;
pub mod config;
pub mod connection;
pub mod idle;
pub mod protocol;
pub mod songs;

use std::sync::{atomic::AtomicU32, Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Sender};
use tauri::{AppHandle, Listener};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use config::MpdConfig;
use idle::{IdleBus, Subsystem};

/// Runtime status surfaced to the frontend. `bound_address` doubles as
/// the "is it really live?" probe, same trick [`crate::dlna::DlnaStatus`]
/// uses — the Settings card shows it verbatim so the user knows what to
/// type into their client.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MpdStatus {
    pub enabled: bool,
    pub running: bool,
    /// `host:port` reachable from the LAN, `None` while stopped.
    pub bound_address: Option<String>,
    pub port: Option<u16>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
enum Cmd {
    Start(MpdConfig, AppHandle),
    Stop,
    Status(oneshot::Sender<MpdStatus>),
}

/// Sync handle owned by `AppState`. Cheap to clone.
#[derive(Clone)]
pub struct MpdServer {
    tx: Sender<Cmd>,
}

impl MpdServer {
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded::<Cmd>();
        std::thread::Builder::new()
            .name("mpd-worker".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .thread_name("mpd-rt")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        tracing::error!(?err, "MPD tokio runtime init failed");
                        return;
                    }
                };

                let mut state = WorkerState::default();
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Cmd::Start(cfg, app) => runtime.block_on(state.start(cfg, app)),
                        Cmd::Stop => runtime.block_on(state.stop()),
                        Cmd::Status(reply) => {
                            let snapshot = state
                                .status
                                .lock()
                                .map(|s| s.clone())
                                .unwrap_or_default();
                            let _ = reply.send(snapshot);
                        }
                    }
                }
                runtime.block_on(state.stop());
            })
            .expect("spawn mpd-worker thread");
        Self { tx }
    }

    pub fn start(&self, cfg: MpdConfig, app: AppHandle) {
        let _ = self.tx.send(Cmd::Start(cfg, app));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub async fn status(&self) -> MpdStatus {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Status(tx)).is_err() {
            return MpdStatus::default();
        }
        rx.await.unwrap_or_default()
    }
}

#[derive(Default)]
struct WorkerState {
    /// Shared so the detached accept task can record a fatal accept error
    /// (running = false + `last_error`); the worker + `Cmd::Status` read it.
    status: Arc<Mutex<MpdStatus>>,
    cancel: Option<CancellationToken>,
    /// Tauri event subscriptions feeding the idle bus. Held so they can
    /// be dropped on stop — otherwise a restart stacks a second set and
    /// every change notifies twice.
    listeners: Vec<tauri::EventId>,
    app: Option<AppHandle>,
}

impl WorkerState {
    async fn start(&mut self, cfg: MpdConfig, app: AppHandle) {
        if self.cancel.is_some() {
            self.stop().await;
        }

        let listener = match bind_with_scan(cfg.port).await {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(port = cfg.port, ?err, "MPD bind failed");
                self.set_status(MpdStatus {
                    enabled: cfg.enabled,
                    running: false,
                    bound_address: None,
                    port: None,
                    last_error: Some(format!("bind :{}: {err}", cfg.port)),
                });
                return;
            }
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);
        let host = crate::dlna::pick_lan_ip().unwrap_or_else(|| "127.0.0.1".into());

        let cancel = CancellationToken::new();
        let idle_bus = IdleBus::new();
        let playlist_version = Arc::new(AtomicU32::new(0));

        self.listeners = subscribe_to_player_events(&app, &idle_bus, &playlist_version);

        let ctx = commands::Ctx {
            app: app.clone(),
            config: cfg.clone(),
            idle: idle_bus,
            playlist_version,
        };

        let accept_cancel = cancel.clone();
        let accept_status = Arc::clone(&self.status);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = accept_cancel.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) => {
                            tracing::debug!(%peer, "MPD client connected");
                            let ctx = ctx.clone();
                            let cancel = accept_cancel.clone();
                            tokio::spawn(async move {
                                if let Err(err) = connection::handle(ctx, stream, cancel).await {
                                    tracing::debug!(%peer, ?err, "MPD connection ended");
                                }
                            });
                        }
                        Err(err) if is_transient_accept_error(&err) => {
                            // A single client aborting mid-handshake, or fd
                            // exhaustion, must not tear down the whole server.
                            // Back off briefly so an EMFILE/ENFILE storm can't
                            // spin the loop while descriptors are exhausted.
                            tracing::warn!(?err, "transient MPD accept error; continuing");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(err) => {
                            // Fatal (the listener itself is gone): stop the loop
                            // and make the reported status reflect it.
                            tracing::error!(?err, "fatal MPD accept error; stopping");
                            if let Ok(mut s) = accept_status.lock() {
                                s.running = false;
                                s.bound_address = None;
                                s.port = None;
                                s.last_error = Some(format!("accept: {err}"));
                            }
                            break;
                        }
                    }
                }
            }
        });

        self.cancel = Some(cancel);
        self.app = Some(app);
        self.set_status(MpdStatus {
            enabled: cfg.enabled,
            running: true,
            bound_address: Some(format!("{host}:{port}")),
            port: Some(port),
            last_error: None,
        });
        tracing::info!(%host, port, "MPD server listening");
    }

    /// Overwrite the shared status (recovering a poisoned lock rather than
    /// panicking — a stale status is better than crashing the worker).
    fn set_status(&self, next: MpdStatus) {
        let mut guard = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = next;
    }

    async fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(app) = self.app.take() {
            for id in self.listeners.drain(..) {
                app.unlisten(id);
            }
        }
        if let Ok(mut s) = self.status.lock() {
            s.running = false;
            s.bound_address = None;
            s.port = None;
        }
    }
}

/// An accept error that must NOT kill the listener: a client aborting before
/// the handshake completes, or transient fd exhaustion. Everything else
/// (the listening socket itself dying) is fatal.
fn is_transient_accept_error(err: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        err.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::Interrupted
    ) {
        return true;
    }
    // EMFILE (24) / ENFILE (23) — per-process / system fd exhaustion. These
    // BSD-derived codes are shared by Linux + macOS; on Windows the numbers
    // differ, so gate the check to Unix and let Windows rely on `kind()`.
    #[cfg(unix)]
    if matches!(err.raw_os_error(), Some(24) | Some(23)) {
        return true;
    }
    false
}

/// Bind `port`, falling back through the next
/// [`config::PORT_SCAN_LEN`] ports when it is taken.
///
/// A second WaveFlow instance, or an actual `mpd` daemon already on
/// 6600, would otherwise leave the feature silently dead.
async fn bind_with_scan(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let mut last_err = None;
    // Inclusive range so the start port is always tried — `port..port+N` is
    // empty at 65535 (`saturating_add` clamps both ends to the same value),
    // which would skip binding entirely. `saturating_add` on the last
    // candidate also clamps to 65535 near the top instead of wrapping/skipping.
    let last = port.saturating_add(config::PORT_SCAN_LEN.saturating_sub(1));
    for candidate in port..=last {
        match tokio::net::TcpListener::bind(("0.0.0.0", candidate)).await {
            Ok(listener) => return Ok(listener),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrInUse, "no free port in range")
    }))
}

/// Bridge the Tauri events the frontend already listens to onto the
/// idle bus, so `idle` wakes on the same changes the UI sees.
///
/// Registering these per-server (rather than once at boot) keeps a
/// disabled server from holding subscriptions, and lets [`WorkerState::stop`]
/// tear them down so a restart doesn't double-fire.
fn subscribe_to_player_events(
    app: &AppHandle,
    bus: &IdleBus,
    playlist_version: &Arc<AtomicU32>,
) -> Vec<tauri::EventId> {
    let mut ids = Vec::new();

    for (event, subsystem) in [
        ("player:state", Subsystem::Player),
        ("player:track-changed", Subsystem::Player),
    ] {
        let bus = bus.clone();
        ids.push(app.listen(event, move |_| bus.notify(subsystem)));
    }

    {
        let bus = bus.clone();
        let version = Arc::clone(playlist_version);
        ids.push(app.listen("player:queue-changed", move |_| {
            version.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            bus.notify(Subsystem::Playlist);
        }));
    }

    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_scan_falls_forward_when_the_port_is_taken() {
        // Occupy a port, then ask the scanner for it: it must land on a
        // later one rather than failing outright.
        let squatter = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();

        let listener = bind_with_scan(taken)
            .await
            .expect("should find a free port");
        let got = listener.local_addr().unwrap().port();
        assert_ne!(got, taken);
        assert!(got > taken && got < taken + config::PORT_SCAN_LEN);
    }
}
