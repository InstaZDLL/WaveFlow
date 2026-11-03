//! WebDAV and SMB shares read directly as library folders.
//!
//! ## A transport, not a provider
//!
//! A share has no catalogue, no account and no user data: it holds files, and
//! the tags in them are read here by the same scanner as a local folder's. So
//! a share is a [`library_folder`] whose path is a
//! [locator](waveflow_core::network_locator), and its tracks are ordinary
//! `track` rows, with their likes, ratings, playlists, history, gapless and
//! crossfade. The server source ([`crate::remote`]) and the provider plugins of
//! RFC-007 are the opposite case, a catalogue somebody else owns, which is why
//! they land in a projection of their own. Reasoning:
//! `docs/features/library.md`, "Network shares".
//!
//! ## Read-only
//!
//! [`NetworkFs`](fs::NetworkFs) has no write. Every path that would write to a
//! track's file asks [`refuse_if_network`] first.
//!
//! ## Availability is not `is_available`
//!
//! `track.is_available = 0` hides a track everywhere, and means the share
//! answered and the file is gone. A share that does not answer changes no row:
//! that is the state kept here, in memory, per source. [`is_blocked`] crosses
//! it with the cache, for the queue and for the list of grey rows.
//!
//! ## Threads
//!
//! The decoder thread and the scan's blocking tasks call in through
//! [`block_on`], on a small runtime of its own: there is no reactor on the
//! decoder thread, and both backends are async. Never from an async task; those
//! use [`run`].

pub mod cache;
pub mod fs;
pub mod media;
pub mod reader;
pub mod scan;
pub mod smb;
pub mod store;
pub mod webdav;

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};
use waveflow_core::network_locator;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use fs::{Entry, NetError, NetworkFs};

/// Tells the interface a source's state changed.
pub const CHANGED_EVENT: &str = "network-source:changed";

/// What playback reports when the share is gone. The decoder's error carries
/// it, so the toast can say the connection was lost instead of "this track".
pub const UNREACHABLE_MESSAGE: &str = "network source unreachable";

const STAT_TIMEOUT: Duration = Duration::from_secs(10);
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// How often a source that is down is asked again.
const PROBE_EVERY: Duration = Duration::from_secs(60);
/// A share has no watcher. How often its listing is walked again.
const RESCAN_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Left to the app to settle after launch, before that first walk.
const FIRST_RESCAN_AFTER: Duration = Duration::from_secs(30);

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("wf-netlib")
            .enable_all()
            .build()
            .expect("network library runtime")
    })
}

/// Run a future to its end from a thread that may block.
pub fn block_on<F: Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

/// Leave a future running on the clients' runtime, from any thread.
pub(crate) fn spawn<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    runtime().spawn(future);
}

/// Run a future from an async task, on the runtime the clients belong to.
pub async fn run<F>(future: F) -> Result<F::Output, NetError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime()
        .spawn(future)
        .await
        .map_err(|e| NetError::Protocol(format!("network task: {e}")))
}

/// What is known of a source right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Not asked yet this session. Treated as usable: a failure says otherwise.
    Unknown,
    Online,
    Unreachable,
    /// It answers and refuses us, so asking again cannot help.
    AuthFailed,
}

/// A share of the active profile.
pub struct Source {
    pub uuid: String,
    pub name: String,
    fs: Box<dyn NetworkFs>,
    state: Mutex<(SourceState, Option<String>)>,
    checking_access: AtomicBool,
}

impl Source {
    pub fn state(&self) -> SourceState {
        self.state
            .lock()
            .map(|s| s.0)
            .unwrap_or(SourceState::Unknown)
    }

    pub fn last_error(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.1.clone())
    }

    /// Whether anything of this source can be asked for at all.
    pub fn is_down(&self) -> bool {
        crate::offline::is_offline()
            || matches!(
                self.state(),
                SourceState::Unreachable | SourceState::AuthFailed
            )
    }

    fn set_state(&self, next: SourceState, error: Option<String>) {
        let changed = match self.state.lock() {
            Ok(mut state) => {
                let changed = state.0 != next;
                *state = (next, error);
                changed
            }
            Err(_) => false,
        };
        if changed {
            tracing::info!(source = %self.uuid, state = ?next, "network source state");
            notify_changed();
        }
    }

    /// File what an answer says about the source. A missing file says it
    /// answers; `Offline` and a released read say nothing about it.
    fn note<T>(&self, result: &Result<T, NetError>) {
        match result {
            Ok(_) | Err(NetError::NotFound) | Err(NetError::Protocol(_)) => {
                self.set_state(SourceState::Online, None);
            }
            // A refusal on one path may be just that path (a system folder on
            // a Windows share): only signing in says something about the share.
            Err(NetError::Auth(_) | NetError::SessionRefused(_)) => self.check_access_soon(),
            Err(err @ (NetError::Unreachable(_) | NetError::Handshake(_) | NetError::Tls(_))) => {
                self.set_state(SourceState::Unreachable, Some(err.to_string()));
            }
            Err(NetError::Offline) | Err(NetError::Released) => {}
        }
    }

    /// The one way out to the network: the offline switch, a deadline, and
    /// the answer filed against the source's state.
    async fn guarded<T>(
        &self,
        limit: Duration,
        operation: impl Future<Output = Result<T, NetError>>,
    ) -> Result<T, NetError> {
        if crate::offline::is_offline() {
            return Err(NetError::Offline);
        }
        let result = tokio::time::timeout(limit, operation)
            .await
            .unwrap_or_else(|_| Err(NetError::Unreachable("no answer in time".into())));
        self.note(&result);
        result
    }

    pub async fn probe(&self) -> Result<(), NetError> {
        let result = self.guarded(PROBE_TIMEOUT, self.fs.probe()).await;
        if let Err(NetError::Auth(detail) | NetError::SessionRefused(detail)) = &result {
            self.set_state(SourceState::AuthFailed, Some(detail.clone()));
        }
        result
    }

    /// Something was refused: find out in the background whether that is the
    /// account as a whole. At most one such check at a time.
    fn check_access_soon(&self) {
        if self.state() == SourceState::AuthFailed
            || self.checking_access.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let Some(me) = source(&self.uuid) else {
            self.checking_access.store(false, Ordering::Release);
            return;
        };
        runtime().spawn(async move {
            let _ = me.probe().await;
            me.checking_access.store(false, Ordering::Release);
        });
    }

    pub async fn list(&self, dir: &str) -> Result<Vec<Entry>, NetError> {
        self.guarded(LIST_TIMEOUT, self.fs.list(dir)).await
    }

    pub async fn stat(&self, path: &str) -> Result<Entry, NetError> {
        self.guarded(STAT_TIMEOUT, self.fs.stat(path)).await
    }

    /// A read, for a scan or a background download: it rides out a dropped
    /// connection, which should not stop a scan or grey a library.
    pub async fn read_at(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, NetError> {
        self.read_patiently(path, offset, len, READ_TIMEOUT).await
    }

    /// A read that tries again until `stall` has gone by, for the transfer
    /// behind playback: a brief drop of the link must not turn the rows grey
    /// or end the track. Only the last failure is filed against the source.
    pub async fn read_patiently(
        &self,
        path: &str,
        offset: u64,
        len: u64,
        stall: Duration,
    ) -> Result<Vec<u8>, NetError> {
        let deadline = Instant::now() + stall;
        loop {
            if crate::offline::is_offline() {
                return Err(NetError::Offline);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let result = tokio::time::timeout(left, self.fs.read_at(path, offset, len))
                .await
                .unwrap_or_else(|_| Err(NetError::Unreachable("no answer in time".into())));
            match result {
                Err(NetError::Unreachable(_) | NetError::Handshake(_))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                other => {
                    self.note(&other);
                    return other.map_err(|err| match err {
                        NetError::Unreachable(_) | NetError::Handshake(_) | NetError::Tls(_) => {
                            NetError::Unreachable(UNREACHABLE_MESSAGE.into())
                        }
                        other => other,
                    });
                }
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    sources: HashMap<String, Arc<Source>>,
    cache_dir: Option<std::path::PathBuf>,
    cache_max_bytes: u64,
}

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(RwLock::default)
}

fn app_handle() -> &'static OnceLock<AppHandle> {
    static APP: OnceLock<AppHandle> = OnceLock::new();
    &APP
}

fn notify_changed() {
    if let Some(app) = app_handle().get() {
        let _ = app.emit(CHANGED_EVENT, ());
        // What the queue steps over just changed, and the decoder may already
        // hold a next track chosen under the old answer.
        crate::audio::next_track::revise_soon(app);
    }
}

/// The source a locator names, in the active profile.
pub fn source(uuid: &str) -> Option<Arc<Source>> {
    registry().read().ok()?.sources.get(uuid).cloned()
}

fn all_sources() -> Vec<Arc<Source>> {
    registry()
        .read()
        .map(|r| r.sources.values().cloned().collect())
        .unwrap_or_default()
}

/// Whether the active profile has a share at all, the one question every
/// local-only path asks before doing any of this.
pub fn has_sources() -> bool {
    registry().read().is_ok_and(|r| !r.sources.is_empty())
}

pub(crate) fn cache_config() -> Option<(std::path::PathBuf, u64)> {
    let registry = registry().read().ok()?;
    Some((registry.cache_dir.clone()?, registry.cache_max_bytes))
}

/// Build the backend a source row names.
pub(crate) fn build_fs(row: &store::SourceRow) -> Result<Box<dyn NetworkFs>, NetError> {
    match row.kind.as_str() {
        "webdav" => Ok(Box::new(webdav::WebDavFs::new(
            &row.endpoint,
            row.username.clone(),
            row.secret.clone(),
            row.allow_plain_http,
        )?)),
        "smb" => Ok(Box::new(smb::SmbFs::new(
            &row.endpoint,
            &row.share,
            row.username.clone(),
            row.secret.clone(),
        )?)),
        other => Err(NetError::Protocol(format!(
            "unsupported source kind: {other}"
        ))),
    }
}

/// Make the registry the sources of this profile. Each starts out unknown and
/// is asked how it is by [`reload_active`]: new credentials are a reason to ask
/// again anyway.
pub async fn reload(pool: &SqlitePool, cache_dir: std::path::PathBuf) -> AppResult<()> {
    let rows = store::list(pool).await?;
    let max_gb = store::cache_max_gb(pool).await;
    let mut sources = HashMap::new();
    for row in rows {
        match build_fs(&row) {
            Ok(fs) => {
                sources.insert(
                    row.uuid.clone(),
                    Arc::new(Source {
                        uuid: row.uuid,
                        name: row.name,
                        fs,
                        state: Mutex::new((SourceState::Unknown, None)),
                        checking_access: AtomicBool::new(false),
                    }),
                );
            }
            // A row this build cannot use stays in the database, and its
            // tracks read as unavailable rather than failing the profile.
            Err(err) => tracing::warn!(error = %err, "network source not usable"),
        }
    }
    if let Ok(mut registry) = registry().write() {
        registry.sources = sources;
        registry.cache_dir = Some(cache_dir);
        registry.cache_max_bytes = max_gb * 1024 * 1024 * 1024;
    }
    notify_changed();
    Ok(())
}

/// [`reload`] for the active profile, then ask every source how it is.
pub async fn reload_active(app: &AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    reload(
        &pool,
        state
            .paths
            .profile_cache_dir(profile_id)
            .join("network-audio"),
    )
    .await?;
    probe_all(false);
    Ok(())
}

/// A new budget for the cache, without touching the sources.
pub async fn reload_cache_limit(pool: &SqlitePool) {
    let max_gb = store::cache_max_gb(pool).await;
    if let Ok(mut registry) = registry().write() {
        registry.cache_max_bytes = max_gb * 1024 * 1024 * 1024;
    }
}

/// No active profile any more: nothing of the last one stays in memory.
pub fn clear() {
    media::stop_prewarm();
    if let Ok(mut registry) = registry().write() {
        registry.sources.clear();
        registry.cache_dir = None;
    }
    notify_changed();
}

/// Ask the sources how they are, all of them or only those that are down. In
/// the background: an unreachable address takes its full timeout to say so.
fn probe_all(only_down: bool) {
    if crate::offline::is_offline() {
        return;
    }
    for source in all_sources() {
        if only_down && source.state() != SourceState::Unreachable {
            continue;
        }
        runtime().spawn(async move {
            let _ = source.probe().await;
        });
    }
}

/// The offline switch moved, which changes what can play.
pub fn offline_changed() {
    notify_changed();
    probe_all(true);
}

/// Whether a track cannot be played right now: on a share that is down, with
/// no local copy. A path on this machine is never blocked here; a missing file
/// is `is_available`'s business.
pub fn is_blocked(file_path: &str) -> bool {
    let Some(parsed) = network_locator::parse(file_path) else {
        return false;
    };
    let down = source(parsed.source).map_or(true, |source| source.is_down());
    down && cache::slot(file_path).is_none()
}

/// Whether a track can be offered to the decoder for a gapless or crossfaded
/// transition: opening it there must not wait on the network.
pub fn ready_for_transition(file_path: &str) -> bool {
    !network_locator::is_locator(file_path) || cache::slot(file_path).is_some()
}

/// Where a track's bytes can be read on this machine: the path itself, or a
/// share's track's cached copy. For what must read a whole file (analysis, the
/// spectrogram) and has no business downloading one for it.
pub fn local_copy(file_path: &str) -> Option<std::path::PathBuf> {
    if !network_locator::is_locator(file_path) {
        return Some(std::path::PathBuf::from(file_path));
    }
    cache::slot(file_path)?.use_path()
}

/// What to report for a track that would not open: when its share is down,
/// that is the reason, whatever the probe made of the missing bytes.
pub fn explain_open_failure(path: &Path, error: String) -> String {
    let down = path
        .to_str()
        .and_then(network_locator::parse)
        .and_then(|parsed| source(parsed.source))
        .is_some_and(|source| source.is_down());
    if down {
        format!("{UNREACHABLE_MESSAGE}: {error}")
    } else {
        error
    }
}

/// The refusal every path that would write to a track's file starts with.
pub fn refuse_if_network(file_path: &str) -> AppResult<()> {
    if network_locator::is_locator(file_path) {
        return Err(AppError::Other(
            "this track is on a network share, which WaveFlow only reads".into(),
        ));
    }
    Ok(())
}

/// `<source name>/<path>` for a locator, what a person reads in a progress
/// row. Any other path comes back as it is.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    match network_locator::parse(&text) {
        Some(parsed) => {
            let name = source(parsed.source)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "?".to_string());
            if parsed.path.is_empty() {
                name
            } else {
                format!("{name}/{}", parsed.path)
            }
        }
        None => text.into_owned(),
    }
}

/// Track ids that are blocked right now. Empty, and at no cost, for anyone
/// whose shares all answer.
pub async fn blocked_track_ids(pool: &SqlitePool) -> AppResult<Vec<i64>> {
    let down: Vec<String> = all_sources()
        .iter()
        .filter(|s| s.is_down())
        .map(|s| s.uuid.clone())
        .collect();
    if down.is_empty() && !has_unregistered_tracks(pool).await? {
        return Ok(Vec::new());
    }
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, file_path FROM track WHERE file_path LIKE 'wfnet://%'")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, path)| is_blocked(path))
        .map(|(id, _)| id)
        .collect())
}

/// Tracks left behind by a source this build could not load.
async fn has_unregistered_tracks(pool: &SqlitePool) -> AppResult<bool> {
    let known = all_sources().len() as i64;
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM network_source")
        .fetch_one(pool)
        .await?;
    Ok(rows != known)
}

/// Install what the core and the decoder need, then keep the sources current
/// for the life of the process.
pub fn start(app: AppHandle) {
    let _ = app_handle().set(app.clone());
    waveflow_core::playback::stream::set_network_opener(Box::new(media::open));
    tauri::async_runtime::spawn(async move {
        if let Err(err) = reload_active(&app).await {
            tracing::debug!(%err, "network sources: nothing to load yet");
        }
        tokio::time::sleep(FIRST_RESCAN_AFTER).await;
        rescan_all(&app).await;
        let mut last_rescan = Instant::now();
        loop {
            tokio::time::sleep(PROBE_EVERY).await;
            probe_all(true);
            if last_rescan.elapsed() >= RESCAN_EVERY {
                rescan_all(&app).await;
                last_rescan = Instant::now();
            }
        }
    });
}

/// Walk every share again: a listing tells what changed without reading a
/// byte of audio. A share that is down waits for the next round, and so does
/// everything while a scan is already running.
async fn rescan_all(app: &AppHandle) {
    if !has_sources() || crate::offline::is_offline() {
        return;
    }
    let state = app.state::<AppState>();
    let Ok((pool, profile_id)) = state.require_profile_snapshot().await else {
        return;
    };
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let folders: Vec<(i64, String)> =
        match sqlx::query_as("SELECT id, path FROM library_folder WHERE path LIKE 'wfnet://%'")
            .fetch_all(&*pool)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(%err, "network rescan: could not read the folders");
                return;
            }
        };
    let mut changed = false;
    for (folder_id, path) in folders {
        if crate::commands::scan::scan_in_flight() {
            return;
        }
        let down = network_locator::parse(&path)
            .and_then(|p| source(p.source))
            .map_or(true, |s| s.is_down());
        if down {
            continue;
        }
        match crate::commands::scan::scan_folder_inner(
            &pool,
            &artwork_dir,
            folder_id,
            Some(app),
            false,
        )
        .await
        {
            Ok(summary) => changed |= summary.added + summary.updated + summary.removed > 0,
            Err(err) => tracing::debug!(%err, folder_id, "network rescan failed"),
        }
    }
    if changed {
        crate::commands::enrichment::maybe_run(app);
        let _ = app.emit("library:rescanned", ());
    }
}

#[cfg(test)]
mod tests;
