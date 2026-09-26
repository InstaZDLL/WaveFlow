use std::collections::HashMap;

use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::{Mutex, RwLock};
use waveflow_core::plugin::runtime::{PluginRuntime, RuntimeConfig};

use crate::{
    db,
    dlna::DlnaServer,
    error::{AppError, AppResult},
    mpd::MpdServer,
    paths::AppPaths,
};

pub use crate::profile_pool::{ActiveProfile, Leased, ProfilePool};

/// Application-wide state managed by Tauri.
///
/// Carries:
/// - the resolved filesystem [`AppPaths`]
/// - the always-open global `app.db` pool
/// - an optional, swappable per-profile `data.db` pool
pub struct AppState {
    pub paths: AppPaths,
    /// Why a stored cache location could not be used this session, when
    /// that happened (issue #619). Kept so the Settings card can name
    /// the reason: caches quietly reappearing at the default location
    /// is, from the user side, indistinguishable from them having been
    /// wiped.
    /// Paired with the root it is about, not stored on its own. The
    /// field is a snapshot taken at startup and there is no moment
    /// afterwards that could refresh it -- so a bare reason outlives
    /// the situation it describes: move away from the drive that was
    /// missing and the card would go on reporting a fallback, and
    /// (because a fallback suppresses it) hide the restart the new move
    /// is waiting for. Carrying the root makes the snapshot
    /// self-invalidating: it applies only while that root is still the
    /// chosen one.
    pub cache_root_fallback: Option<(std::path::PathBuf, String)>,
    /// Serializes [`crate::commands::storage::set_cache_location`].
    ///
    /// That command reads the pending-move marker, copies a whole cache
    /// tree, then writes the marker back — a sequence with awaits all
    /// the way through. Two calls with different destinations would both
    /// pass the read, both copy, and the second write would name the
    /// only destination anyone remembers: the first one's copy of the
    /// entire artwork tree would sit on disk with nothing pointing at
    /// it. The frontend's own busy flag does not help, because it
    /// guards a button and not the IPC surface behind it.
    pub cache_move_lock: Arc<tokio::sync::Mutex<()>>,
    pub app_db: SqlitePool,
    pub profile: Arc<RwLock<Option<ActiveProfile>>>,
    /// DLNA / UPnP MediaServer worker. Always present (the worker
    /// thread is spawned at init even when DLNA is disabled) so the
    /// Settings page can call into it without re-spawning.
    pub dlna: DlnaServer,
    /// MPD protocol server (issue #471). Opt-in, off by default.
    pub mpd: MpdServer,
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
    /// The active remote play queue, if any (RFC-005). Streams its tracks
    /// by URL rather than from `queue_item`; cleared the moment a library
    /// track or a radio stream takes over. Always present — a stock build
    /// simply never populates it (the command that does is `sync_v2`-gated).
    pub remote_playback: crate::remote_playback::RemotePlayback,
}

/// Pick the profile to activate on startup.
///
/// Priority: the `app.last_profile_id` setting if it still exists,
/// otherwise the most-recently-used profile. Returns `None` only if the
/// table is genuinely empty (should not happen after `bootstrap` has run
/// `create_default_profile`, but handled defensively).
///
/// A free function rather than a method because two callers ask this
/// question and only one of them has an `AppState`: the startup
/// pre-flight ([`crate::db::schema_guard::preflight`]) runs before the
/// event loop, holding nothing but a read-only handle on `app.db`. If
/// the two ever disagreed, the pre-flight would vet one profile's
/// database and the app would open another.
pub(crate) use crate::profile_selection::resolve_target_profile;

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
        // The app-data root has to exist before `app.db` can be opened,
        // and `app.db` has to be open before the cache root can be read
        // out of it (issue #619) — so this is deliberately not the full
        // `ensure_dirs`, which would create the cache tree at the
        // default location a moment before learning it belongs
        // somewhere else.
        std::fs::create_dir_all(&paths.root)?;

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

        // Now that the setting is readable, settle where the caches
        // live and materialise the layout. A stored choice that cannot
        // be used this session (drive unplugged, share offline) falls
        // back to the default without clearing the choice; the reason
        // is kept so the frontend can say so rather than leaving the
        // user to conclude their artwork was deleted.
        let (paths, cache_root_fallback) =
            crate::commands::storage::resolve_cache_root(paths, &app_db).await;
        paths.ensure_dirs()?;
        crate::commands::storage::cleanup_moved_caches(&paths, &app_db).await;

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
            cache_root_fallback,
            cache_move_lock: Arc::new(tokio::sync::Mutex::new(())),
            app_db,
            profile: Arc::new(RwLock::new(None)),
            dlna: DlnaServer::spawn(),
            mpd: MpdServer::spawn(),
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
            remote_playback: crate::remote_playback::RemotePlayback::default(),
        };

        state.bootstrap().await?;

        Ok(state)
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
