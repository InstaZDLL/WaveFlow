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

pub mod cds;
pub mod config;
pub mod description;
pub mod http;
pub mod ssdp;

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

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
    /// Shared with every server the worker starts, so a new set of
    /// titles reaches a running server without a restart.
    labels: cds::SharedLabels,
}

impl DlnaServer {
    /// Spin up the worker thread. The server stays idle until
    /// [`Self::start`] is called — the worker just listens on the
    /// channel.
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded::<Cmd>();
        let labels = cds::SharedLabels::default();
        let worker_labels = labels.clone();
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

                let mut state = WorkerState {
                    labels: worker_labels,
                    ..WorkerState::default()
                };
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
        Self { tx, labels }
    }

    /// Retitle the top-level folders. Takes effect on the next Browse,
    /// whether or not a server is running.
    pub fn set_labels(&self, labels: cds::RootLabels) {
        match self.labels.write() {
            Ok(mut guard) => *guard = labels,
            Err(poisoned) => *poisoned.into_inner() = labels,
        }
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
    /// Owns the SSDP announcer + responder background tasks. Replaced
    /// (and previous handle dropped → tasks aborted) on every Start.
    ssdp: Option<ssdp::SsdpHandle>,
    /// Handed to each server's [`http::ServerCtx`].
    labels: cds::SharedLabels,
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
        // One snapshot of the interfaces for the whole session: SSDP
        // joins each of them, and the first one is the address shown in
        // Settings. Each HTTP reply names the address its own request
        // came in on instead (see `http::LocalAddr`).
        let lan = lan_addrs();
        let lan_ip = lan
            .first()
            .map_or_else(|| actual.ip().to_string(), |a| a.ip.to_string());
        let url = format!("http://{lan_ip}:{}", actual.port());

        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let ctx = http::ServerCtx {
            server_name: cfg.server_name.clone(),
            base_url: url.clone(),
            pool: resources.pool.clone(),
            profile_artwork_dir: resources.profile_artwork_dir.clone(),
            metadata_artwork_dir: resources.metadata_artwork_dir.clone(),
            labels: self.labels.clone(),
        };
        let app = http::router(ctx);

        tokio::spawn(async move {
            let serve = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<http::LocalAddr>(),
            );
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

        // Spawn the SSDP announcer + responder. Failure here doesn't
        // bring down the HTTP server — controllers that already know
        // our LOCATION can keep talking to us; only auto-discovery
        // breaks. Surface the error in `last_error` so the Settings
        // UI can warn the user.
        let ssdp_err = match ssdp::spawn(cfg.server_name.clone(), lan, actual.port()) {
            Ok(handle) => {
                self.ssdp = Some(handle);
                None
            }
            Err(err) => {
                tracing::warn!(?err, "SSDP spawn failed");
                self.ssdp = None;
                Some(format!("SSDP: {err}"))
            }
        };

        self.shutdown = Some(shutdown_tx);
        self.status = DlnaStatus {
            enabled: cfg.enabled,
            running: true,
            server_name: cfg.server_name.clone(),
            bound_url: Some(url),
            last_error: ssdp_err,
        };
    }

    async fn stop(&mut self) {
        // Drop the SSDP handle first so we stop announcing alive
        // before the HTTP server goes away — controllers that probe
        // a stale LOCATION right after we shut down get a clean
        // connection refused instead of a hanging request.
        self.ssdp.take();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.status.running = false;
        self.status.bound_url = None;
    }
}

/// One IPv4 address of this host, with the netmask that tells which
/// peers sit on the same link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LanAddr {
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

impl LanAddr {
    /// Whether `peer` is on this address's subnet. A `/0` mask would
    /// claim every peer, so it claims none.
    pub fn contains(&self, peer: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        mask != 0 && u32::from(self.ip) & mask == u32::from(peer) & mask
    }
}

/// The address to hand a peer: the one on its subnet, else the
/// preferred one. An IPv4-mapped IPv6 peer counts as IPv4.
pub(crate) fn local_ip_for(addrs: &[LanAddr], peer: IpAddr) -> Option<Ipv4Addr> {
    let on_link = match peer.to_canonical() {
        IpAddr::V4(v4) => addrs.iter().find(|a| a.contains(v4)),
        IpAddr::V6(_) => None,
    };
    on_link.or_else(|| addrs.first()).map(|a| a.ip)
}

/// Every non-loopback IPv4 address of this host, best first (see
/// [`interface_rank`]). Empty when enumeration fails.
pub(crate) fn lan_addrs() -> Vec<LanAddr> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(err) => {
            tracing::warn!(?err, "listing network interfaces failed");
            return Vec::new();
        }
    };
    let mut ranked: Vec<(u8, LanAddr)> = interfaces
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match &i.addr {
            if_addrs::IfAddr::V4(v4) => Some((
                interface_rank(&i.name, v4.ip, i.is_oper_up(), i.is_p2p()),
                LanAddr {
                    ip: v4.ip,
                    netmask: v4.netmask,
                },
            )),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect();
    // Stable: within a rank, the OS order still decides.
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, addr)| addr).collect()
}

/// Lower is better. A receiver can only reach an address on its own
/// LAN, so a private address on a physical, running interface comes
/// first; a VPN tunnel or a VM / container bridge (Docker, WSL and
/// Hyper-V's default switch hand out private 172.x addresses too) is
/// demoted below any of those, and link-local comes last. Nothing is
/// dropped: a host whose only address looks virtual still gets served
/// on it.
fn interface_rank(name: &str, ip: Ipv4Addr, up: bool, p2p: bool) -> u8 {
    if ip.is_link_local() {
        return 4;
    }
    let demoted = !up || p2p || looks_virtual(name);
    match (demoted, ip.is_private()) {
        (false, true) => 0,
        (false, false) => 1,
        (true, true) => 2,
        (true, false) => 3,
    }
}

/// Interface names that virtualisation and VPN software give the
/// adapters it creates: the adapter's friendly name on Windows, the
/// kernel name on unix. A heuristic, which is why it only ever demotes
/// (see [`interface_rank`]).
fn looks_virtual(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "docker", "br-", "veth", "virbr", "vboxnet", "vmnet", "tun", "tap", "wg", "zt", "utun",
        "lxcbr", "lxdbr", "podman", "cni", "flannel",
    ];
    const WORDS: &[&str] = &[
        "vethernet",
        "virtualbox",
        "vmware",
        "hyper-v",
        "wsl",
        "docker",
        "tailscale",
        "zerotier",
        "wireguard",
        "openvpn",
        "nordlynx",
        "vpn",
    ];
    let name = name.to_ascii_lowercase();
    PREFIXES.iter().any(|p| name.starts_with(p)) || WORDS.iter().any(|w| name.contains(w))
}

/// The preferred LAN address, for a display that can show only one:
/// the DLNA status URL, and the address the MPD server shows in
/// Settings.
pub(crate) fn pick_lan_ip() -> Option<String> {
    lan_addrs().first().map(|a| a.ip.to_string())
}

#[cfg(test)]
mod tests;
