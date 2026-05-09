//! DLNA / UPnP MediaServer integration.
//!
//! Goal: expose the active profile's library over the LAN as a
//! `urn:schemas-upnp-org:device:MediaServer:1` so DLNA-compatible
//! receivers (Yamaha MusicCast, Sonos, Kodi, BubbleUPnP, VLC...) can
//! browse and stream the user's collection without any per-receiver
//! configuration.
//!
//! # Architecture
//!
//! Same dedicated-thread + crossbeam-channel pattern as
//! [`media_controls`](crate::media_controls) and
//! [`discord_presence`](crate::discord_presence): a single
//! [`DlnaServer`] handle ferries commands (`Start`, `Stop`,
//! `Reconfigure`) to a worker that owns the tokio runtime and the
//! axum + SSDP tasks. Lets the rest of the app keep a sync API even
//! though the implementation is async.
//!
//! # Module layout
//!
//! - [`config`]   — user-facing settings persisted in `app_setting`
//!   (enabled flag, server name, advertised port).
//! - [`http`]     — axum router for the SOAP control endpoint, the
//!   description XML, the `/stream/<track_id>` route with Range
//!   support and the `/art/<hash>` shim.
//! - [`ssdp`]     — UDP multicast announcer + M-SEARCH responder on
//!   `239.255.255.250:1900`.
//! - [`cds`]      — ContentDirectory Browse handler producing
//!   DIDL-Lite responses.
//! - [`description`] — generates the device + service XML descriptors
//!   on demand (host IP, port, server name vary per session).
//!
//! Étape 1 of the rollout (this commit) wires `config` + a no-op
//! [`DlnaServer`] handle so the Tauri command surface and Settings UI
//! can land before the protocol layers are filled in. The server
//! currently only opens a TCP listener on the configured port and
//! serves `/healthz`; SSDP and ContentDirectory follow in subsequent
//! commits.

pub mod config;
pub mod description;
pub mod http;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Sender};
use sqlx::SqlitePool;
use tokio::sync::oneshot;

use config::DlnaConfig;

/// Runtime status surfaced to the frontend. `bound_url` is `None`
/// while the server is starting up or stopped — the UI uses it both
/// as the access URL to copy AND as the "is it really live?" probe.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DlnaStatus {
    pub enabled: bool,
    pub running: bool,
    pub server_name: String,
    pub bound_url: Option<String>,
    /// Last error message surfaced from the worker, e.g. "port in use"
    /// or "no LAN interface". Cleared on successful start.
    pub last_error: Option<String>,
}

/// Wired to the active profile's pool + the per-profile artwork dir
/// so the HTTP layer can serve `/stream/<track_id>` and `/art/<hash>`
/// without round-tripping through `AppState` (the worker thread has
/// no `tauri::State` access).
#[derive(Debug, Clone)]
pub struct DlnaResources {
    pub pool: SqlitePool,
    pub profile_artwork_dir: PathBuf,
    pub metadata_artwork_dir: PathBuf,
}

#[derive(Debug)]
enum Cmd {
    Start(DlnaConfig, DlnaResources),
    Stop,
    Status(oneshot::Sender<DlnaStatus>),
}

/// Sync handle owned by `AppState`. Cheap to clone; all heavy work
/// happens on the worker thread.
#[derive(Clone)]
pub struct DlnaServer {
    tx: Sender<Cmd>,
}

impl DlnaServer {
    /// Spin up the worker thread. The server stays idle until
    /// [`Self::start`] is called — the worker just listens on the
    /// channel.
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded::<Cmd>();
        std::thread::Builder::new()
            .name("dlna-worker".into())
            .spawn(move || {
                // Tokio runtime stays scoped to this thread so the
                // worker cleanly tears down on Stop without leaking
                // task handles into the main app runtime.
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .thread_name("dlna-rt")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        tracing::error!(?err, "DLNA tokio runtime init failed");
                        return;
                    }
                };

                let mut state = WorkerState::default();
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Cmd::Start(cfg, res) => {
                            runtime.block_on(state.start(cfg, res));
                        }
                        Cmd::Stop => {
                            runtime.block_on(state.stop());
                        }
                        Cmd::Status(reply) => {
                            let _ = reply.send(state.status.clone());
                        }
                    }
                }
                runtime.block_on(state.stop());
            })
            .expect("spawn dlna-worker thread");
        Self { tx }
    }

    pub fn start(&self, cfg: DlnaConfig, resources: DlnaResources) {
        let _ = self.tx.send(Cmd::Start(cfg, resources));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    pub async fn status(&self) -> DlnaStatus {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Status(tx)).is_err() {
            return DlnaStatus::default();
        }
        rx.await.unwrap_or_default()
    }
}

/// Per-thread state — the running task handle and the published
/// status snapshot. Stays inside the worker so callers never see
/// half-mutated state.
#[derive(Default)]
struct WorkerState {
    status: DlnaStatus,
    /// Drops when set to `None` to abort the running axum task.
    shutdown: Option<oneshot::Sender<()>>,
}

impl WorkerState {
    async fn start(&mut self, cfg: DlnaConfig, resources: DlnaResources) {
        if self.shutdown.is_some() {
            // Reconfigure: stop the previous server before binding a
            // new one. Cheap because the worker owns the runtime.
            self.stop().await;
        }

        let port = cfg.port;
        let bind_addr = format!("0.0.0.0:{port}");

        let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%bind_addr, ?err, "DLNA bind failed");
                self.status = DlnaStatus {
                    enabled: cfg.enabled,
                    running: false,
                    server_name: cfg.server_name.clone(),
                    bound_url: None,
                    last_error: Some(format!("bind {bind_addr}: {err}")),
                };
                return;
            }
        };
        let actual = match listener.local_addr() {
            Ok(addr) => addr,
            Err(err) => {
                tracing::warn!(?err, "DLNA local_addr failed");
                self.status.last_error = Some(format!("local_addr: {err}"));
                return;
            }
        };
        let lan_ip = pick_lan_ip().unwrap_or_else(|| actual.ip().to_string());
        let url = format!("http://{lan_ip}:{}", actual.port());

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let ctx = http::ServerCtx {
            server_name: cfg.server_name.clone(),
            base_url: url.clone(),
            pool: resources.pool.clone(),
            profile_artwork_dir: resources.profile_artwork_dir.clone(),
            metadata_artwork_dir: resources.metadata_artwork_dir.clone(),
        };
        let app = http::router(ctx);

        tokio::spawn(async move {
            let serve = axum::serve(listener, app);
            tokio::select! {
                res = serve => {
                    if let Err(err) = res {
                        tracing::warn!(?err, "DLNA axum exited");
                    }
                }
                _ = shutdown_rx => {
                    tracing::info!("DLNA shutdown requested");
                }
            }
        });

        let _ = Arc::new(cfg.clone()); // keep cfg referenced for future SSDP config snapshot
        self.shutdown = Some(shutdown_tx);
        self.status = DlnaStatus {
            enabled: cfg.enabled,
            running: true,
            server_name: cfg.server_name.clone(),
            bound_url: Some(url),
            last_error: None,
        };
    }

    async fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.status.running = false;
        self.status.bound_url = None;
    }
}

/// Pick the first non-loopback IPv4 interface. Returned as a string
/// because every consumer (description.xml, status URL, SSDP LOCATION)
/// formats it back into URLs anyway — keeping it as `String` avoids
/// re-running the lookup at each touch point.
fn pick_lan_ip() -> Option<String> {
    let addrs = if_addrs::get_if_addrs().ok()?;
    addrs
        .into_iter()
        .filter(|a| !a.is_loopback())
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(v4) => Some(v4.to_string()),
            std::net::IpAddr::V6(_) => None,
        })
        .next()
}
