use crate::{
    db,
    error::{AppError, AppResult},
    paths::AppPaths,
    profile_pool::{ActiveProfile, ProfilePool},
    profile_selection::resolve_target_profile,
    state::AppState,
};
use chrono::Utc;

impl AppState {
    /// Ensure at least one profile exists, then activate the most relevant
    /// one. Called once at the end of host initialization.
    pub(crate) async fn bootstrap(&self) -> AppResult<()> {
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

    /// Pick the profile to activate on startup. See
    /// [`resolve_target_profile`] — the free function is the
    /// implementation, so the startup pre-flight can ask the same
    /// question without an `AppState`.
    async fn resolve_target_profile(&self) -> AppResult<Option<i64>> {
        resolve_target_profile(&self.app_db).await
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
            // Drop any remote play session *under the same lock* as the
            // swap, so no concurrent `profile.read()` can observe the new
            // profile alongside the previous one's playback state (queue,
            // cursor) (RFC-005). The fallible setup above runs before the
            // lock, so a failed `ensure_profile_dirs` / `open` still leaves
            // the current session intact. The clear only takes the
            // `remote_playback` mutex briefly and never re-enters the
            // profile lock, so the profile→remote_playback order is
            // consistent and deadlock-free.
            self.remote_playback.clear();
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
            let previous = guard.take();
            // Same reason as activate_profile: no remote session may outlive
            // the profile whose tracks it points at (RFC-005). Cleared under
            // the profile write lock so the session and the profile go away
            // together; the clear only touches the `remote_playback` mutex,
            // never the profile lock, so the ordering stays deadlock-free.
            self.remote_playback.clear();
            previous
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
    /// switch will not close this pool while the handle is alive (issue
    /// #332). Keep it alive for as long as you query — binding it to `_`
    /// releases it immediately.
    ///
    /// **The guarantee is time-bounded, not absolute.** The drain gives
    /// up after `LEASE_DRAIN_TIMEOUT` and closes the pool anyway, so a
    /// command that holds a lease longer than that across a switch can
    /// still hit `PoolClosed` — it just logs a WARN first. Commands
    /// running well past that (a full library scan) must keep tolerating
    /// the error; what the lease buys is that ordinary multi-step
    /// commands no longer race the close at all.
    #[allow(dead_code)]
    pub async fn require_profile_pool(&self) -> AppResult<ProfilePool> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(ActiveProfile::lease)
            .ok_or(AppError::NoActiveProfile)
    }

    /// Like [`Self::require_profile_pool`], but refuses to hand out the
    /// lease when `expected` names a profile that is no longer the
    /// active one (issue #485).
    ///
    /// This is the only place that can close the write-to-the-wrong-profile
    /// window. A frontend guard ("is the profile still the one I captured?")
    /// necessarily runs *before* its `invoke` crosses the IPC boundary, so a
    /// `switch_profile` landing in between still redirects the write. Here the
    /// comparison and the lease happen under **one** acquisition of the same
    /// lock `switch_profile` takes to swap the pool, so there is no window at
    /// all: either the profile still matches and the lease pins it open, or the
    /// call fails.
    ///
    /// `expected: None` opts out and behaves exactly like
    /// [`Self::require_profile_pool`] — for callers with no profile in mind
    /// (a fresh user action against whatever is active right now).
    pub async fn require_profile_pool_for(&self, expected: Option<i64>) -> AppResult<ProfilePool> {
        let guard = self.profile.read().await;
        check_expected_profile(guard.as_ref().map(|p| p.profile_id), expected)?;
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

fn check_expected_profile(active: Option<i64>, expected: Option<i64>) -> AppResult<()> {
    match expected {
        // No expectation: act on whatever is active.
        None => Ok(()),
        Some(expected) if active == Some(expected) => Ok(()),
        Some(expected) => Err(AppError::ProfileChanged { expected, active }),
    }
}

#[cfg(test)]
mod expected_profile_tests {
    use super::*;

    #[test]
    fn no_expectation_always_passes() {
        assert!(check_expected_profile(Some(1), None).is_ok());
        assert!(check_expected_profile(None, None).is_ok());
    }

    #[test]
    fn matching_expectation_passes() {
        assert!(check_expected_profile(Some(7), Some(7)).is_ok());
    }

    #[test]
    fn switched_profile_is_refused() {
        let err = check_expected_profile(Some(2), Some(1)).unwrap_err();
        assert!(
            matches!(
                err,
                AppError::ProfileChanged {
                    expected: 1,
                    active: Some(2)
                }
            ),
            "unexpected error: {err}",
        );
    }

    #[test]
    fn deactivated_profile_is_refused() {
        // `deactivate_profile` leaves no active profile; a queued write
        // that expected one must not fall through to `NoActiveProfile`
        // silently succeeding somewhere else.
        let err = check_expected_profile(None, Some(1)).unwrap_err();
        assert!(
            matches!(
                err,
                AppError::ProfileChanged {
                    expected: 1,
                    active: None
                }
            ),
            "unexpected error: {err}",
        );
    }
}
