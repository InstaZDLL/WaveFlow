//! The automatic refresh: every six hours, while the app runs and the switch is
//! on, and never during startup.
//!
//! The six hours count from the last check, stored per profile, and not from the
//! launch, so someone whose sessions are shorter than that still gets their
//! feeds checked. One that is overdue runs [`OVERDUE_GRACE`] after the loop looks,
//! so launching the app contacts nobody.
//!
//! With the switch off the loop waits on a [`Notify`], so no timer runs. It wakes
//! when `podcasts.enabled` is written and when another profile becomes active,
//! whose switch and last check are its own.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::sync::Notify;

use super::fetch::Fetcher;
use super::refresh::{
    self, Leases, OpmlImportReport, PassObserver, PassOutcome, PodcastError, RefreshSummary,
};
use super::store;
use crate::commands::profile::PROFILE_SETTING_CHANGED;
use crate::state::{AppState, ProfilePool};

/// How often channels are asked for their feed.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// The shortest wait before a check, however overdue. Also what stops a pass that
/// could not stamp the profile, in offline mode, from running again at once.
pub(crate) const OVERDUE_GRACE: Duration = Duration::from_secs(5 * 60);

/// One way for everything that can change the answer to "should this run".
static WAKE: Notify = Notify::const_new();

/// One pass at a time, manual or automatic: two would ask the same hosts for
/// the same feeds and race each other's writes.
static PASS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears [`PASS_RUNNING`] on every exit path.
struct PassGuard;

impl PassGuard {
    fn take() -> Option<Self> {
        (!PASS_RUNNING.swap(true, Ordering::SeqCst)).then_some(Self)
    }
}

impl Drop for PassGuard {
    fn drop(&mut self) {
        PASS_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Another profile became active: its switch and its channels are not this one's.
pub(crate) fn profile_changed() {
    WAKE.notify_one();
}

/// The profile a piece of work began for, leased again each time it is needed.
pub(crate) struct ProfileLeases<'a> {
    pub state: &'a AppState,
    pub profile_id: i64,
}

impl Leases for ProfileLeases<'_> {
    type Lease = ProfilePool;

    async fn lease(&self) -> Result<ProfilePool, PodcastError> {
        self.state
            .require_profile_pool_for(Some(self.profile_id))
            .await
            .map_err(|_| PodcastError::ProfileChanged)
    }
}

/// Reports a pass to the status bar and the page.
struct TaskObserver<'a> {
    app: &'a AppHandle,
    task: Option<crate::tasks::TaskHandle>,
}

impl TaskObserver<'_> {
    fn start(app: &AppHandle) -> TaskObserver<'_> {
        TaskObserver {
            app,
            task: crate::tasks::start(
                app,
                crate::tasks::TaskKind::PodcastRefresh,
                0,
                crate::tasks::Cancellation::Flag,
            ),
        }
    }
}

impl PassObserver for TaskObserver<'_> {
    fn cancelled(&self) -> bool {
        self.task.as_ref().is_some_and(|task| task.is_cancelling())
    }

    fn progress(&self, done: u64, total: u64) {
        if let Some(task) = self.task.as_ref() {
            task.progress(done, total);
        }
    }

    fn channel_changed(&self, channel_id: i64) {
        emit_updated(self.app, Some(channel_id));
    }
}

/// See [`super::PODCAST_UPDATED`].
pub(crate) fn emit_updated(app: &AppHandle, channel_id: Option<i64>) {
    let _ = app.emit(
        super::PODCAST_UPDATED,
        serde_json::json!({ "channel_id": channel_id }),
    );
}

/// A refresh pass over the active profile, under its status-bar row.
pub(crate) async fn run_refresh_pass(
    app: &AppHandle,
    only: Option<i64>,
) -> Result<RefreshSummary, PodcastError> {
    let Some(_running) = PassGuard::take() else {
        return Ok(RefreshSummary::ended(PassOutcome::AlreadyRunning));
    };
    let state = app.state::<AppState>();
    let (_, profile_id) = state
        .require_profile_snapshot()
        .await
        .map_err(PodcastError::Other)?;
    let fetcher = Fetcher::new()?;
    let observer = TaskObserver::start(app);
    let summary = refresh::refresh_pass(
        &ProfileLeases {
            state: &state,
            profile_id,
        },
        &fetcher,
        only,
        &state.paths.metadata_artwork_dir,
        &observer,
    )
    .await?;
    tracing::info!(
        refreshed = summary.refreshed,
        unchanged = summary.unchanged,
        failed = summary.failed,
        "podcast refresh pass finished"
    );
    Ok(summary)
}

/// An OPML list, subscribed to feed by feed, under the same row and the same
/// one-at-a-time rule as a refresh.
pub(crate) async fn run_import_pass(
    app: &AppHandle,
    entries: Vec<waveflow_core::podcast::OpmlEntry>,
) -> Result<OpmlImportReport, PodcastError> {
    let Some(_running) = PassGuard::take() else {
        return Ok(OpmlImportReport {
            outcome: PassOutcome::AlreadyRunning,
            added: 0,
            already: 0,
            failed: Vec::new(),
            total: entries.len() as u32,
        });
    };
    let state = app.state::<AppState>();
    let (_, profile_id) = state
        .require_profile_snapshot()
        .await
        .map_err(PodcastError::Other)?;
    let fetcher = Fetcher::new()?;
    let observer = TaskObserver::start(app);
    refresh::import_pass(
        &ProfileLeases {
            state: &state,
            profile_id,
        },
        &fetcher,
        entries,
        &state.paths.metadata_artwork_dir,
        &observer,
    )
    .await
}

/// Starts the timer. Costs nothing until the switch is on.
pub(crate) fn spawn(app: AppHandle) {
    app.listen(PROFILE_SETTING_CHANGED, |event| {
        let written = serde_json::from_str::<serde_json::Value>(event.payload())
            .ok()
            .and_then(|payload| payload.get("key")?.as_str().map(str::to_owned));
        if written.as_deref() == Some(store::ENABLED_KEY) {
            WAKE.notify_one();
        }
    });
    tauri::async_runtime::spawn(async move {
        run_schedule(
            &WAKE,
            || {
                let app = app.clone();
                async move { read_schedule(&app).await }
            },
            || {
                let app = app.clone();
                async move {
                    // Nobody to ask, and no row to flash in the status bar every
                    // five minutes for as long as offline mode lasts.
                    if crate::offline::is_offline() {
                        return;
                    }
                    if let Err(err) = run_refresh_pass(&app, None).await {
                        let err = crate::error::AppError::from(err);
                        tracing::warn!(%err, "podcast refresh pass stopped");
                    }
                }
            },
        )
        .await;
    });
}

/// What the loop has to go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Schedule {
    Off,
    On {
        /// `None` when this profile has never been checked.
        since_last_check: Option<Duration>,
    },
}

async fn read_schedule(app: &AppHandle) -> Schedule {
    let state = app.state::<AppState>();
    let Ok(pool) = state.require_profile_pool().await else {
        return Schedule::Off;
    };
    if !store::is_enabled(&pool).await.unwrap_or(false) {
        return Schedule::Off;
    }
    let last = store::last_check_at(&pool).await.unwrap_or(None);
    let now = chrono::Utc::now().timestamp_millis();
    Schedule::On {
        // A stamp from the future is a clock that moved back, and reads as now.
        since_last_check: last
            .map(|at| Duration::from_millis(now.saturating_sub(at).max(0) as u64)),
    }
}

/// How long to wait before the next check.
fn wait_before_check(since_last_check: Option<Duration>) -> Duration {
    since_last_check
        .map_or(OVERDUE_GRACE, |since| {
            REFRESH_INTERVAL.saturating_sub(since)
        })
        .clamp(OVERDUE_GRACE, REFRESH_INTERVAL)
}

/// The loop itself, with what it asks and what it runs passed in so a test can
/// run it on a clock it controls.
///
/// Everything is read again when the wait ends: the switch may have been turned off,
/// or a check run, by a path that did not wake this, and the answer must be the one
/// that holds now.
pub(crate) async fn run_schedule<S, SF, R, RF>(wake: &Notify, read: S, refresh: R)
where
    S: Fn() -> SF,
    SF: Future<Output = Schedule>,
    R: Fn() -> RF,
    RF: Future<Output = ()>,
{
    loop {
        let Schedule::On { since_last_check } = read().await else {
            wake.notified().await;
            continue;
        };
        tokio::select! {
            _ = tokio::time::sleep(wait_before_check(since_last_check)) => {}
            _ = wake.notified() => continue,
        }
        // A pass run by hand during the wait stamped the profile without waking this,
        // and asking every host again now would be asking for nothing.
        if let Schedule::On { since_last_check } = read().await {
            if wait_before_check(since_last_check) <= OVERDUE_GRACE {
                refresh().await;
            }
        }
    }
}
