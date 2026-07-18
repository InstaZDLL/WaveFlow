use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::{Mutex, Notify, RwLock};
use waveflow_core::plugin::runtime::{PluginRuntime, RuntimeConfig};

use crate::{
    db,
    dlna::DlnaServer,
    error::{AppError, AppResult},
    paths::AppPaths,
};

/// How long a profile switch waits for outstanding leases before it
/// force-closes the previous pool anyway.
///
/// A leaked or genuinely long-running lease (a full library scan holds
/// its pool for minutes) must never wedge profile switching, so the
/// wait is bounded. Hitting the bound degrades to the pre-lease
/// behaviour — the close races whatever is still running — which is
/// exactly the `PoolClosed` window this mechanism exists to shrink, so
/// it is logged at WARN rather than swallowed.
const LEASE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Refcount of live [`ProfileLease`] handles for one profile epoch.
///
/// Lives behind an `Arc` shared by the [`ActiveProfile`] and every
/// lease it hands out, so a lease keeps the counter reachable even
/// after the profile has been swapped out of [`AppState::profile`].
#[derive(Default)]
struct LeaseTracker {
    active: AtomicUsize,
    idle: Notify,
}

impl LeaseTracker {
    fn acquire(self: &Arc<Self>) -> ProfileLease {
        self.active.fetch_add(1, Ordering::AcqRel);
        ProfileLease {
            tracker: Arc::clone(self),
        }
    }

    /// Resolve once no lease is outstanding. Returns `false` if the
    /// deadline elapsed first.
    async fn wait_idle(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                // `enable()` registers this waiter *before* the count
                // is read. Creating the future is not enough — tokio
                // registers on first poll, so without this a lease
                // dropped between the load and the await would notify
                // nobody and we would block until the timeout.
                let notified = self.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();

                if self.active.load(Ordering::Acquire) == 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }
}

/// Keeps a profile's pool open for as long as the holder needs it.
///
/// Handed out by [`AppState::require_profile_pool`] /
/// [`AppState::require_profile_snapshot`] as part of [`ProfilePool`],
/// and released on drop — including on the error paths of a `?`, since
/// that is just an early scope exit.
pub struct ProfileLease {
    tracker: Arc<LeaseTracker>,
}

impl Drop for ProfileLease {
    fn drop(&mut self) {
        if self.tracker.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Last lease out: wake any profile switch parked in
            // `wait_idle`. `notify_waiters` (not `notify_one`) because
            // a switch and a shutdown can both be waiting on the same
            // epoch, and a stored permit would leak to the next
            // acquirer.
            self.tracker.idle.notify_waiters();
        }
    }
}

/// A profile pool plus the lease that keeps it open.
///
/// Derefs to the inner [`SqlitePool`], so it passes anywhere a
/// `&SqlitePool` is expected. sqlx's query methods take a generic
/// `E: Executor` though, and deref coercion does not fire against a
/// type variable — those call sites need an explicit `&*pool`.
pub struct ProfilePool {
    pool: SqlitePool,
    _lease: ProfileLease,
}

impl std::ops::Deref for ProfilePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl ProfilePool {
    /// Split into the raw pool and its lease.
    ///
    /// For the handful of call sites that must hand an owned
    /// `SqlitePool` to a type living in `waveflow-core` (which knows
    /// nothing about app-layer leases). Keep the returned lease alive
    /// alongside that value — [`Leased`] does exactly that.
    pub fn into_parts(self) -> (SqlitePool, ProfileLease) {
        (self.pool, self._lease)
    }

    /// Give up lease tracking and keep only the pool.
    ///
    /// Reserved for handles that deliberately outlive a profile switch
    /// (the DLNA worker holds its pool across requests and is rebuilt
    /// on switch). Holding a lease there would stall every switch until
    /// [`LEASE_DRAIN_TIMEOUT`]. Such a handle is back to the pre-#332
    /// exposure and must tolerate `PoolClosed`.
    pub fn into_unleashed(self) -> SqlitePool {
        self.pool
    }
}

/// A value built from a leased pool, carrying the lease with it.
///
/// Derefs to the inner value, so repository helpers can keep returning
/// "a repository" while the lease rides along invisibly and releases
/// when the caller drops it.
pub struct Leased<T> {
    inner: T,
    _lease: ProfileLease,
}

impl<T> Leased<T> {
    pub fn new(inner: T, lease: ProfileLease) -> Self {
        Self {
            inner,
            _lease: lease,
        }
    }
}

impl<T> std::ops::Deref for Leased<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for Leased<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Active per-profile database. Closed and replaced on profile switch.
pub struct ActiveProfile {
    pub profile_id: i64,
    pub pool: SqlitePool,
    leases: Arc<LeaseTracker>,
}

impl ActiveProfile {
    fn new(profile_id: i64, pool: SqlitePool) -> Self {
        Self {
            profile_id,
            pool,
            leases: Arc::new(LeaseTracker::default()),
        }
    }

    fn lease(&self) -> ProfilePool {
        ProfilePool {
            pool: self.pool.clone(),
            _lease: self.leases.acquire(),
        }
    }

    /// Wait for outstanding leases to drain, then close the pool.
    ///
    /// Called only after this epoch has been swapped out of
    /// [`AppState::profile`], so no new lease can be issued against it
    /// and the count is monotonically decreasing.
    async fn close_when_idle(self) {
        if !self.leases.wait_idle(LEASE_DRAIN_TIMEOUT).await {
            tracing::warn!(
                profile_id = self.profile_id,
                outstanding = self.leases.active.load(Ordering::Acquire),
                timeout_secs = LEASE_DRAIN_TIMEOUT.as_secs(),
                "profile leases still held at switch; closing pool anyway",
            );
        }
        self.pool.close().await;
    }
}

/// Application-wide state managed by Tauri.
///
/// Carries:
/// - the resolved filesystem [`AppPaths`]
/// - the always-open global `app.db` pool
/// - an optional, swappable per-profile `data.db` pool
pub struct AppState {
    pub paths: AppPaths,
    pub app_db: SqlitePool,
    pub profile: Arc<RwLock<Option<ActiveProfile>>>,
    /// DLNA / UPnP MediaServer worker. Always present (the worker
    /// thread is spawned at init even when DLNA is disabled) so the
    /// Settings page can call into it without re-spawning.
    pub dlna: DlnaServer,
    /// Wake handle for the sync drain task (Phase 1.f.desktop.4a).
    /// CRUD command sites notify after `tx.commit()` so a chatty
    /// user's edits reach the server without waiting for the
    /// periodic tick. Defaults to an unparked notifier on a fresh
    /// `AppState` — the live task is spawned in `lib.rs::run` once
    /// the AppHandle is available.
    pub drain: Arc<crate::sync::drain::DrainHandle>,
    /// Mutual-exclusion lock around [`crate::sync::drain::drain_once`].
    /// The background task and the `sync_drain_now` Tauri command
    /// share the same `Arc<Mutex<()>>` so a manual user-driven push
    /// never races a periodic tick (would otherwise read the same
    /// `sync_pending_op` rows and double-send — server absorbs the
    /// duplicates via the `operation_id` UNIQUE but the wasted
    /// round-trip + duplicated `total_sent` accounting is avoidable).
    /// Held only by the gated `commands::sync` module — the stub
    /// build never reads it, hence the `dead_code` allow.
    #[allow(dead_code)]
    pub drain_lock: Arc<tokio::sync::Mutex<()>>,
    /// Mutual-exclusion lock around [`crate::sync::backfill::run_backfill`]
    /// (Phase B.2). Holds for the duration of a backfill pass so a
    /// concurrent Tauri command surfaces `AlreadyRunning` instead of
    /// firing a parallel sweep that would race the same digest +
    /// entity fetches. Independent of [`drain_lock`] — a backfill can
    /// trigger drains internally without deadlocking. Same dead-code
    /// caveat as `drain_lock` in stub builds.
    #[allow(dead_code)]
    pub backfill_lock: Arc<tokio::sync::Mutex<()>>,
    /// Wake handle for the sync WebSocket subscriber (Phase
    /// 1.f.desktop.4b). The `server_account` commands fire it after
    /// the user signs in / signs out / changes mode so the
    /// subscriber doesn't sit on its idle gate while something has
    /// actually changed. Defaults to an unparked handle; the live
    /// task spawns in `lib.rs::run` once the AppHandle is available.
    /// Wake() is no-op in stub builds.
    #[allow(dead_code)]
    pub ws: Arc<crate::sync::ws::SubscribeHandle>,
    /// Plugin SDK runtime. One engine + one shared HTTP client per
    /// process; `Clone` is cheap (wraps the inner `Arc`). The offline
    /// probe is wired to [`crate::offline::is_offline`] so plugin
    /// HTTP calls short-circuit on the same flag as Deezer / Last.fm
    /// / LRCLIB.
    pub plugins: PluginRuntime,
    /// Per-plugin serialisation locks. Used by
    /// [`crate::commands::plugins`] to make the manifest-existence
    /// check + the `app_setting` upsert in `set_plugin_enabled`
    /// atomic against a concurrent `uninstall_plugin` for the same
    /// id — otherwise the enable toggle could observe a present
    /// manifest, then the uninstall removes the install dir + drops
    /// the row, then the toggle's INSERT lands as an orphan.
    ///
    /// Held while async work runs, so the inner lock is
    /// `tokio::sync::Mutex`. The map itself is also a tokio mutex
    /// because insertions happen on the async side and we want to
    /// keep the same lock primitive throughout. Map size is bounded
    /// by how many distinct plugin ids the user ever touches in
    /// one session — sub-dozen for v1.5.0, no GC needed.
    pub plugin_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl AppState {
    /// Initialize the application state during Tauri setup.
    ///
    /// Resolves filesystem paths, ensures the root directories exist, opens
    /// `app.db` (running any pending migrations) and runs a bootstrap pass
    /// so the app always starts with **exactly one active profile**:
    ///
    /// 1. If the `profile` table is empty, a "Default" profile is created
    ///    (directory layout + fresh `data.db`).
    /// 2. The `app.last_profile_id` setting is consulted; if it points to a
    ///    still-existing profile, that profile is activated. Otherwise the
    ///    most-recently-used profile is activated as a fallback.
    pub async fn init(handle: &AppHandle) -> AppResult<Self> {
        let paths = AppPaths::from_handle(handle)?;
        paths.ensure_dirs()?;

        // One-shot cleanup for the 1.5.0 → 1.5.1 transition: before
        // this release, `ensure_bundled_plugins` copied every bundled
        // plugin into `<app-data>/plugins/<id>/` at boot, wasting
        // ~150 KB per id and confusing users who went folder
        // spelunking (issue #280). The new model resolves bundled
        // plugins directly from `BaseDirectory::Resource`, so any
        // leftover writable copy under `<app-data>/plugins/` is dead
        // weight that ALSO shadows the resource copy on case-
        // insensitive filesystems if `list_installed_plugins` is
        // ever extended to prefer sideloaded on a name collision.
        // Drop them. Idempotent: re-running finds nothing to remove.
        // Logged-only on failure — a stuck cleanup must not block
        // the rest of startup.
        if has_valid_bundled_plugins_dir(&paths) {
            if let Err(e) = cleanup_bundled_plugin_leftovers(&paths).await {
                tracing::warn!(%e, "bundled plugin leftover cleanup failed");
            }
        }

        let app_db = db::app_db::open(&paths.app_db).await?;

        // Hydrate the global offline-mode flag from app_setting so
        // any outbound HTTP call honours the persisted preference
        // before the user opens Settings. The flag is process-wide
        // (see `crate::offline`) because offline is a network-stack
        // concern, not per-profile.
        let offline_initial: Option<String> =
            sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'network.offline_mode'")
                .fetch_optional(&app_db)
                .await
                .ok()
                .flatten();
        crate::offline::set(
            offline_initial
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        );

        // Hydrate the Musixmatch opt-in flag from app_setting. Default
        // off (Musixmatch hits a reverse-engineered private endpoint;
        // not authorised by their ToS). Users who want it enable via
        // `app_setting['lyrics.musixmatch_enabled'] = 'true'` until the
        // v1.6 Settings toggle ships.
        let musixmatch_initial: Option<String> = sqlx::query_scalar(
            "SELECT value FROM app_setting WHERE key = 'lyrics.musixmatch_enabled'",
        )
        .fetch_optional(&app_db)
        .await
        .ok()
        .flatten();
        crate::commands::lyrics::set_musixmatch_enabled(
            musixmatch_initial
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        );

        // Plugin runtime — one per process. The offline probe reads
        // the same `crate::offline` atomic Deezer / Last.fm / LRCLIB
        // do, so flipping the user-facing offline switch reaches
        // plugin HTTP without a separate wiring path.
        //
        // MUST run on `spawn_blocking`: `reqwest::blocking::Client::build`
        // spawns its own internal tokio runtime on a sidecar thread,
        // and reqwest panics ("Cannot drop a runtime in a context
        // where blocking is not allowed") when that construction is
        // attempted from inside an outer async context. Tauri's
        // setup callback hosts the entire `AppState::init`, so the
        // direct call here would tank startup. The blocking task
        // returns a `PluginRuntime` we can move back to the async
        // side — clones share the inner Arc, no second-build pain.
        let probe: waveflow_core::plugin::runtime::OfflineProbe =
            Arc::new(crate::offline::is_offline);
        let plugins = tokio::task::spawn_blocking(move || {
            PluginRuntime::new_with_offline_probe(RuntimeConfig::default(), probe)
        })
        .await
        .map_err(|e| AppError::Other(format!("plugin runtime init join: {e}")))?
        .map_err(|e| AppError::Other(format!("plugin runtime init: {e}")))?;

        let state = Self {
            paths,
            app_db,
            profile: Arc::new(RwLock::new(None)),
            dlna: DlnaServer::spawn(),
            // Placeholder until `lib.rs::run` wires the live task.
            // CRUD command sites can `notify()` against it harmlessly
            // before the task spawns (no waiter parked yet); the
            // first real tick will pick up any queued work.
            drain: Arc::new(crate::sync::drain::DrainHandle),
            drain_lock: Arc::new(tokio::sync::Mutex::new(())),
            backfill_lock: Arc::new(tokio::sync::Mutex::new(())),
            ws: Arc::new(crate::sync::ws::SubscribeHandle),
            plugins,
            plugin_locks: Arc::new(Mutex::new(HashMap::new())),
        };

        state.bootstrap().await?;

        Ok(state)
    }

    /// Ensure at least one profile exists, then activate the most relevant
    /// one. Called once at the end of [`Self::init`].
    async fn bootstrap(&self) -> AppResult<()> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profile")
            .fetch_one(&self.app_db)
            .await?;

        if count == 0 {
            self.create_default_profile().await?;
        }

        if let Some(profile_id) = self.resolve_target_profile().await? {
            self.activate_profile(profile_id).await?;
        }

        Ok(())
    }

    /// Create the built-in "Default" profile: DB row, filesystem layout and
    /// a freshly migrated `data.db`. Invoked only on the very first launch.
    async fn create_default_profile(&self) -> AppResult<()> {
        let now = Utc::now().timestamp_millis();

        let insert = sqlx::query(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES (?, 'emerald', NULL, '', ?, ?)",
        )
        .bind("Default")
        .bind(now)
        .bind(now)
        .execute(&self.app_db)
        .await?;

        let profile_id = insert.last_insert_rowid();
        let rel_dir = AppPaths::profile_rel_dir(profile_id);

        sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
            .bind(&rel_dir)
            .bind(profile_id)
            .execute(&self.app_db)
            .await?;

        self.paths.ensure_profile_dirs(profile_id)?;
        let pool =
            db::profile_db::open(&self.paths.profile_db(profile_id), &self.paths.app_db).await?;
        pool.close().await;

        tracing::info!(profile_id, "created default profile");
        Ok(())
    }

    /// Pick the profile to activate on startup.
    ///
    /// Priority: the `app.last_profile_id` setting if it still exists,
    /// otherwise the most-recently-used profile. Returns `None` only if the
    /// table is genuinely empty (should not happen after `bootstrap` has run
    /// `create_default_profile`, but handled defensively).
    async fn resolve_target_profile(&self) -> AppResult<Option<i64>> {
        let last_profile_id: Option<String> =
            sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'app.last_profile_id'")
                .fetch_optional(&self.app_db)
                .await?;

        if let Some(id_str) = last_profile_id {
            if let Ok(id) = id_str.parse::<i64>() {
                let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM profile WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.app_db)
                    .await?;
                if exists.is_some() {
                    return Ok(Some(id));
                }
            }
        }

        let fallback: Option<i64> =
            sqlx::query_scalar("SELECT id FROM profile ORDER BY last_used_at DESC LIMIT 1")
                .fetch_optional(&self.app_db)
                .await?;

        Ok(fallback)
    }

    /// Open (or reopen) the per-profile `data.db` for `profile_id`. If a
    /// profile is currently active, its pool is closed first so that WAL
    /// files can be cleanly checkpointed.
    ///
    /// The previous pool is closed **without** the write lock held —
    /// the close first drains outstanding leases (see
    /// [`ActiveProfile::close_when_idle`]) and then waits for in-flight
    /// queries, which can block for a noticeable fraction of a second
    /// on a busy profile switch. Holding the write lock across that
    /// await would freeze every other command that hits
    /// `state.profile.read().await` for the duration.
    ///
    /// Swapping the epoch under the write lock before draining is what
    /// makes the drain terminate: once the old [`ActiveProfile`] is out
    /// of `self.profile`, no further lease can be issued against it.
    pub async fn activate_profile(&self, profile_id: i64) -> AppResult<()> {
        self.paths.ensure_profile_dirs(profile_id)?;

        let db_path = self.paths.profile_db(profile_id);
        let pool = db::profile_db::open(&db_path, &self.paths.app_db).await?;

        let previous = {
            let mut guard = self.profile.write().await;
            let previous = guard.take();
            *guard = Some(ActiveProfile::new(profile_id, pool));
            previous
        };
        if let Some(previous) = previous {
            previous.close_when_idle().await;
        }

        Ok(())
    }

    /// Close the active profile pool, if any, leaving no profile active.
    /// Waits for outstanding leases first, same as [`Self::activate_profile`].
    pub async fn deactivate_profile(&self) {
        let previous = {
            let mut guard = self.profile.write().await;
            guard.take()
        };
        if let Some(previous) = previous {
            previous.close_when_idle().await;
        }
    }

    /// Return a leased handle on the active profile's pool, or an error
    /// if none is active. The pool is cheap to clone (it's an `Arc`
    /// internally).
    ///
    /// The returned [`ProfilePool`] holds a lease: a concurrent profile
    /// switch will not close this pool until the handle is dropped, so
    /// a multi-step command that keeps it across awaits cannot hit
    /// `PoolClosed` mid-flight (issue #332). Keep it alive for as long
    /// as you query — binding it to `_` releases it immediately.
    #[allow(dead_code)]
    pub async fn require_profile_pool(&self) -> AppResult<ProfilePool> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(ActiveProfile::lease)
            .ok_or(AppError::NoActiveProfile)
    }

    /// Return the active profile id, or an error if none is active.
    ///
    /// Used by upcoming library/scan/queue commands.
    #[allow(dead_code)]
    pub async fn require_profile_id(&self) -> AppResult<i64> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(|p| p.profile_id)
            .ok_or(AppError::NoActiveProfile)
    }

    /// Atomic snapshot of the active profile's `(pool, profile_id)` under
    /// a single lock acquisition. Prefer this over separate
    /// `require_profile_pool` + `require_profile_id` calls when a command
    /// needs both: two separate awaits can straddle a `switch_profile`
    /// and pair one profile's pool with another profile's id (and any
    /// path derived from it, e.g. `profile_artwork_dir`).
    ///
    /// The pool half carries a lease — see [`Self::require_profile_pool`].
    pub async fn require_profile_snapshot(&self) -> AppResult<(ProfilePool, i64)> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(|p| (p.lease(), p.profile_id))
            .ok_or(AppError::NoActiveProfile)
    }
}

// `BUNDLED_PLUGINS` + `is_bundled_plugin` moved to
// `waveflow_core::plugin` so `PluginPaths` can route bundled ids to
// the resource dir without re-importing app-layer state. Callers
// inside `crate::commands::plugins` now `use waveflow_core::plugin::is_bundled_plugin;`.

/// One-shot cleanup of pre-1.5.1 leftovers: drop any subdir of
/// `<app-data>/plugins/` whose name is in
/// [`waveflow_core::plugin::BUNDLED_PLUGINS`]. Before this release,
/// `ensure_bundled_plugins` copied every bundled .wasm + manifest
/// into the writable app-data tree at boot; the new model resolves
/// them straight from `BaseDirectory::Resource` so those copies are
/// dead weight (~150 KB per id) that ALSO confused users who went
/// folder spelunking (issue #280). Idempotent: a 1.5.1 fresh install
/// finds no leftovers and does nothing.
///
/// FS ops run on `spawn_blocking` — `remove_dir_all` on a multi-MB
/// plugin tree (future bundled plugins with assets, or a Web Radio
/// embedding a SQLite seed) can stretch into double-digit ms and
/// we don't want to tie up a tokio worker during boot.
fn has_valid_bundled_plugins_dir(paths: &AppPaths) -> bool {
    matches!(
        paths.bundled_plugins_dir.as_deref(),
        Some(path) if path.exists() && path.is_dir()
    )
}

async fn cleanup_bundled_plugin_leftovers(paths: &AppPaths) -> AppResult<()> {
    let Some(bundled_root) = paths.bundled_plugins_dir.clone() else {
        return Ok(());
    };
    let plugins_root = paths.plugin_paths().plugins_root;
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        if !(bundled_root.exists() && bundled_root.is_dir()) {
            tracing::warn!(
                path = %bundled_root.display(),
                "bundled plugins resource dir unavailable; preserving app-data bundled plugin fallback",
            );
            return Ok(());
        }
        for id in waveflow_core::plugin::BUNDLED_PLUGINS {
            let leftover = plugins_root.join(id);
            match std::fs::remove_dir_all(&leftover) {
                Ok(()) => {
                    tracing::info!(plugin_id = %id, "removed pre-1.5.1 bundled plugin leftover");
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(AppError::Io(e)),
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("bundled plugin cleanup join: {e}")))?
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use std::time::Instant;

    /// Stand-in for an active profile backed by an in-memory database.
    /// Exercises the real [`ActiveProfile`] lease + close path without
    /// needing an `AppHandle` (an `AppState` cannot be built in a unit
    /// test).
    async fn active_profile() -> ActiveProfile {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        ActiveProfile::new(1, pool)
    }

    #[tokio::test]
    async fn wait_idle_returns_immediately_without_leases() {
        let tracker = Arc::new(LeaseTracker::default());
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn outstanding_lease_blocks_the_drain() {
        let tracker = Arc::new(LeaseTracker::default());
        let lease = tracker.acquire();

        assert!(
            !tracker.wait_idle(Duration::from_millis(50)).await,
            "drain must time out while a lease is held",
        );

        drop(lease);
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn every_lease_must_drop_before_the_drain_completes() {
        let tracker = Arc::new(LeaseTracker::default());
        let first = tracker.acquire();
        let second = tracker.acquire();

        drop(first);
        assert!(
            !tracker.wait_idle(Duration::from_millis(50)).await,
            "one of two leases released is not idle",
        );

        drop(second);
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    /// The waiter must be woken by the drop, not by polling — so the
    /// drain resolves promptly rather than sitting out its timeout.
    #[tokio::test]
    async fn dropping_the_last_lease_wakes_a_parked_drain() {
        let tracker = Arc::new(LeaseTracker::default());
        let lease = tracker.acquire();

        let holder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lease);
        });

        let started = Instant::now();
        assert!(tracker.wait_idle(Duration::from_secs(10)).await);
        let waited = started.elapsed();

        assert!(
            waited >= Duration::from_millis(90),
            "drain resolved before the lease was released ({waited:?})",
        );
        assert!(
            waited < Duration::from_secs(5),
            "drain waited far past the release, so it was not woken ({waited:?})",
        );
        holder.await.unwrap();
    }

    /// The regression this whole mechanism exists for (issue #332): a
    /// multi-step command holds a leased pool across an await while a
    /// profile switch closes the epoch. The command's later query must
    /// still succeed instead of failing with `PoolClosed`.
    #[tokio::test]
    async fn leased_pool_survives_a_concurrent_profile_switch() {
        let profile = active_profile().await;
        let leased = profile.lease();

        // The switch takes the epoch out of `AppState::profile` and
        // drains it. Runs concurrently with the command below.
        let switch = tokio::spawn(async move { profile.close_when_idle().await });

        // Step 1 of the command, then an await that straddles the switch.
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&*leased)
            .await
            .expect("first step");
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Step 2 — this is what used to blow up with `PoolClosed`.
        let value: i64 = sqlx::query_scalar("SELECT 2")
            .fetch_one(&*leased)
            .await
            .expect("second step must not hit PoolClosed");
        assert_eq!(value, 2);

        assert!(!switch.is_finished(), "switch closed the pool under us");
        drop(leased);
        switch.await.unwrap();
    }

    /// Once the lease is released the pool really is closed — the drain
    /// must not leak the old epoch.
    #[tokio::test]
    async fn pool_closes_after_the_lease_is_released() {
        let profile = active_profile().await;
        let leased = profile.lease();
        let observer = (*leased).clone();

        let switch = tokio::spawn(async move { profile.close_when_idle().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!observer.is_closed());

        drop(leased);
        switch.await.unwrap();
        assert!(observer.is_closed());
    }

    /// A lease taken from the *new* epoch must not hold the old one
    /// open: each `ActiveProfile` owns its own tracker.
    #[tokio::test]
    async fn epochs_track_leases_independently() {
        let previous = active_profile().await;
        let next = active_profile().await;

        let leased_next = next.lease();
        // Nothing outstanding on `previous`, so its drain is immediate
        // even though the new epoch is leased.
        assert!(previous.leases.wait_idle(Duration::from_millis(50)).await);
        assert!(!next.leases.wait_idle(Duration::from_millis(50)).await);
        drop(leased_next);
    }
}
