//! Episodes kept on disk, one at a time, under a quota.
//!
//! The file lands in a folder under the cache root
//! ([`profile_podcast_download_dir`]), never a library folder, so neither the
//! scanner nor the watcher sees it. Nothing a feed says names a path: the file is
//! called by the episode's id and a suffix chosen from its first bytes, and a body
//! that is not audio gets nothing written at all.
//!
//! The enclosure is opened by [`Fetcher`], so each hop is judged before it is
//! contacted, against the address the channel was typed with. A failure is one word,
//! and no address reaches the log, an event or an error.
//!
//! **Everything is counted while the body is read.** `Content-Length` is only what
//! lets a file known to be too big be refused before its bytes come, and a host
//! that lies gains nothing, since both ceilings are applied to what really arrives.
//!
//! **A download holds no database lease across the network**, or a slow host would
//! make a profile switch wait out its drain timeout. The pool is leased again for
//! each check, pinned to the profile the download began for, which stops the
//! download once that profile is no longer active. The lease is held across the
//! rename and the row that records it, so a switch cannot pass between the two.
//!
//! **Bytes reach their final name in one rename**, after they are on the disk, and
//! the row comes last. A crash can therefore leave a partial file, or a whole file
//! without a row, and never a row promising bytes that were not written; [`sweep`]
//! removes what is left at the next launch and on each profile switch.
//!
//! [`profile_podcast_download_dir`]: crate::paths::AppPaths::profile_podcast_download_dir

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Notify};
use url::Url;
use waveflow_core::podcast::address::parse_address;
use waveflow_core::podcast::{sniff_audio, FetchFailure};

use super::fetch::{FetchError, Fetcher, Origin};
use super::refresh::{with_busy_retry, Leases, PodcastError, SCAN_WAIT_POLL};
use super::schedule::{emit_updated, ProfileLeases};
use super::store;
use crate::state::AppState;

/// Emitted as a download moves, at most about four times a second.
pub const PODCAST_DOWNLOAD: &str = "podcast:download";

/// The most one episode may weigh. Far above any real episode, and what stops a
/// body that never ends well before it reaches the quota.
const MAX_EPISODE_BYTES: u64 = 1024 * 1024 * 1024;

/// What [`sniff_audio`] wants to see before it can tell.
const SNIFF_BYTES: usize = 12;

/// The suffix of a file being written.
const PARTIAL_SUFFIX: &str = "part";

/// The word for a file or a row that could not be written. Not one a feed can
/// cause, so it is not a `FetchFailure`.
const WRITE_FAILED: &str = "write_failed";

/// The word for an episode no longer there to download.
pub(crate) const EPISODE_GONE: &str = "episode_gone";

/// Attempts at the rename. On Windows an indexer or an antivirus commonly opens a
/// file the moment it is closed, and the rename fails for as long as it looks.
const MAX_RENAME_ATTEMPTS: u32 = 5;

/// The clocks of a download. A parameter so a test can run them short.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pace {
    /// The largest file taken.
    pub max_bytes: u64,
    /// Opening the enclosure, every hop included.
    pub open_deadline: Duration,
    /// The whole body. A slow line and a long episode have to fit in it.
    pub body_deadline: Duration,
    /// A body must bring [`Self::min_bytes_per_window`] in each of these. A
    /// deadline on the whole body cannot be short enough to catch a host that
    /// sends a byte now and then, and it would hold up the queue for hours.
    pub window: Duration,
    pub min_bytes_per_window: u64,
    /// How often the event goes out while bytes arrive.
    pub progress_every: Duration,
    /// How often the switch, offline mode and the profile are looked at again.
    pub gate_every: Duration,
}

impl Pace {
    pub(crate) const fn standard() -> Self {
        Self {
            max_bytes: MAX_EPISODE_BYTES,
            open_deadline: Duration::from_secs(60),
            body_deadline: Duration::from_secs(6 * 60 * 60),
            window: Duration::from_secs(60),
            min_bytes_per_window: 64 * 1024,
            progress_every: Duration::from_millis(250),
            gate_every: Duration::from_secs(1),
        }
    }
}

/// Where a download stands. Mirrors `PodcastDownloadState` in `podcast.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// The payload of [`PODCAST_DOWNLOAD`]. Holds no address and no path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadUpdate {
    pub episode_id: i64,
    pub state: DownloadState,
    pub received_bytes: u64,
    /// What the host declared, which may be untrue. `None` when it declared nothing.
    pub total_bytes: Option<u64>,
    /// One word, with [`DownloadState::Failed`] only.
    pub error_word: Option<String>,
}

impl DownloadUpdate {
    pub(crate) fn at(episode_id: i64, state: DownloadState) -> Self {
        Self {
            episode_id,
            state,
            received_bytes: 0,
            total_bytes: None,
            error_word: None,
        }
    }
}

/// What a download reports along the way, without knowing about Tauri.
pub(crate) trait DownloadObserver: Sync {
    fn update(&self, update: DownloadUpdate);
    /// The episode's title, once it has been read.
    fn titled(&self, _title: &str) {}
}

/// How a download ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ended {
    /// The file is in place and the row says so. `None` when it already was, and
    /// nothing changed.
    Stored {
        size: Option<u64>,
    },
    /// By the user, or because its profile is no longer the active one.
    Cancelled,
    Failed(String),
}

/// Why a download stopped short, kept apart from the word so nothing can be
/// logged but the word.
#[derive(Debug)]
pub(super) enum Stop {
    Cancelled,
    ProfileChanged,
    Word(String),
}

impl From<FetchFailure> for Stop {
    fn from(failure: FetchFailure) -> Self {
        Self::Word(failure.code())
    }
}

impl From<FetchError> for Stop {
    fn from(err: FetchError) -> Self {
        match err {
            FetchError::Offline => Self::Word("offline".to_owned()),
            FetchError::Failed(failure) => failure.into(),
        }
    }
}

impl From<PodcastError> for Stop {
    fn from(err: PodcastError) -> Self {
        match err {
            PodcastError::Disabled => Self::Word("disabled".to_owned()),
            PodcastError::Offline => Self::Word("offline".to_owned()),
            PodcastError::Failed(failure) => failure.into(),
            PodcastError::ProfileChanged => Self::ProfileChanged,
            PodcastError::EpisodeGone => Self::Word(EPISODE_GONE.to_owned()),
            PodcastError::NoSource => Self::Word("no_source".to_owned()),
            PodcastError::Database(err) => err.into(),
            PodcastError::Other(_) => Self::Word(WRITE_FAILED.to_owned()),
        }
    }
}

impl From<sqlx::Error> for Stop {
    fn from(err: sqlx::Error) -> Self {
        tracing::warn!(%err, "podcast download: the database refused");
        Self::Word(WRITE_FAILED.to_owned())
    }
}

impl From<std::io::Error> for Stop {
    fn from(err: std::io::Error) -> Self {
        // The kind and not the error: its text may name the file.
        tracing::warn!(kind = ?err.kind(), "podcast download: the disk refused");
        Self::Word(WRITE_FAILED.to_owned())
    }
}

/// A file's name in the managed folder.
fn file_name(episode_id: i64, suffix: &str) -> String {
    format!("{episode_id}.{suffix}")
}

/// The episode a name in the managed folder belongs to: `<id>.<suffix>`, digits and
/// letters only. So no separator, no `..`, no absolute path, no drive prefix, no
/// `wfnet://` locator and no alternate data stream: `download_file` may have come
/// out of an archive somebody made, and what passes here gets opened and removed.
fn episode_of(name: &str) -> Option<i64> {
    let (id, suffix) = name.split_once('.')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

/// The file `name` in `dir`, when it is this episode's and is there.
///
/// `download_file` may have come out of an archive somebody made, so it is hostile
/// here: it has to read `<episode id>.<letters and digits>`, the one shape a download
/// writes. That leaves no separator, no `..`, no absolute path, no drive prefix, no
/// `wfnet://` locator and no alternate data stream, on any system, no other
/// episode's file and no partial one. Whatever opens or removes a stored name goes
/// through here; anything else is no file of ours, and nothing touches it.
pub(crate) fn stored_file(dir: &Path, episode_id: i64, name: &str) -> Option<PathBuf> {
    let (id, suffix) = name.split_once('.')?;
    if id != episode_id.to_string()
        || suffix.is_empty()
        || suffix == PARTIAL_SUFFIX
        || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let path = dir.join(name);
    path.is_file().then_some(path)
}

/// Removes the partial file however the download ends, a panic included.
struct Partial {
    path: PathBuf,
}

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// On the blocking pool, since a file being looked at by something else is waited
/// for, and with the bytes already flushed.
async fn rename_into_place(from: PathBuf, to: PathBuf) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut attempt = 1;
        loop {
            match std::fs::rename(&from, &to) {
                Err(err) if is_held_by_another(&err) && attempt < MAX_RENAME_ATTEMPTS => {
                    std::thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
                    attempt += 1;
                }
                outcome => return outcome,
            }
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Refused because something else has the file open. `ERROR_SHARING_VIOLATION` has
/// no `ErrorKind` of its own, so it is named by number.
fn is_held_by_another(err: &std::io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    err.kind() == std::io::ErrorKind::PermissionDenied
        || (cfg!(windows) && err.raw_os_error() == Some(ERROR_SHARING_VIOLATION))
}

/// Throws away what is at `path`, which may not have been created.
async fn remove_quietly(path: &Path) {
    let path = path.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
}

/// Answers once `cancel` is true, and never when nobody is left to set it.
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    if cancel.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The switch, offline mode and the profile, as they are now.
async fn gate<L: Leases>(leases: &L, fetcher: &Fetcher) -> Result<(), Stop> {
    if fetcher.is_offline() {
        return Err(PodcastError::Offline.into());
    }
    let pool = leases.lease().await?;
    if !store::is_enabled(&pool).await? {
        return Err(PodcastError::Disabled.into());
    }
    Ok(())
}

/// Fetches one episode into `dir`.
///
/// `cancel` turning true stops it wherever it is, the wait for a host included.
pub(crate) async fn download_episode<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    dir: &Path,
    episode_id: i64,
    pace: Pace,
    cancel: watch::Receiver<bool>,
    observer: &impl DownloadObserver,
) -> Ended {
    match fetch_and_store(leases, fetcher, dir, episode_id, pace, cancel, observer).await {
        Ok(size) => Ended::Stored { size },
        Err(Stop::Cancelled | Stop::ProfileChanged) => Ended::Cancelled,
        Err(Stop::Word(word)) => {
            tracing::info!(episode_id, code = word.as_str(), "podcast download failed");
            Ended::Failed(word)
        }
    }
}

async fn fetch_and_store<L: Leases>(
    leases: &L,
    fetcher: &Fetcher,
    dir: &Path,
    episode_id: i64,
    pace: Pace,
    cancel: watch::Receiver<bool>,
    observer: &impl DownloadObserver,
) -> Result<Option<u64>, Stop> {
    let (source, quota, used) = {
        gate(leases, fetcher).await?;
        let pool = leases.lease().await?;
        let source = store::episode_download(&pool, episode_id)
            .await?
            .ok_or_else(|| Stop::Word(EPISODE_GONE.to_owned()))?;
        let quota = store::download_quota_bytes(&pool).await?;
        let used = store::download_used_bytes(&pool, Some(episode_id)).await?;
        (source, quota, used)
    };
    observer.titled(&source.title);
    if source
        .download_file
        .as_deref()
        .and_then(|name| stored_file(dir, episode_id, name))
        .is_some()
    {
        return Ok(None);
    }
    // At the quota already, so nobody needs contacting to find out.
    if used >= quota {
        return Err(FetchFailure::QuotaExceeded.into());
    }
    let enclosure = source
        .enclosure_url
        .as_deref()
        .ok_or(PodcastError::NoSource)?;
    let enclosure = parse_address(enclosure).ok_or(FetchFailure::AddressRefused)?;
    // A channel without an address is not one somebody typed here, so it reaches
    // nothing local. A restored one has no enclosure either, until it is subscribed
    // to again.
    let origin = source
        .feed_url
        .as_deref()
        .and_then(parse_address)
        .map_or(Origin::public(), |feed| Origin::of(&feed));

    observer.update(DownloadUpdate::at(episode_id, DownloadState::Running));
    let body = Body {
        leases,
        fetcher,
        dir,
        episode_id,
        pace,
        room: quota.saturating_sub(used),
        observer,
    };
    let written = body.read(&enclosure, origin, cancel.clone()).await?;
    finish(leases, dir, episode_id, written, cancel)
        .await
        .map(Some)
}

/// A body read to its end, flushed to the disk under its partial name.
struct Written {
    partial: Partial,
    final_name: String,
    size: u64,
}

struct Body<'a, L, O> {
    leases: &'a L,
    fetcher: &'a Fetcher,
    dir: &'a Path,
    episode_id: i64,
    pace: Pace,
    /// What the quota has left for this file.
    room: u64,
    observer: &'a O,
}

impl<L: Leases, O: DownloadObserver> Body<'_, L, O> {
    async fn read(
        &self,
        enclosure: &Url,
        origin: Origin,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Written, Stop> {
        let mut response = tokio::select! {
            biased;
            () = cancelled(&mut cancel) => return Err(Stop::Cancelled),
            opened = tokio::time::timeout(
                self.pace.open_deadline,
                self.fetcher.open(enclosure, origin, HeaderMap::new()),
            ) => opened.unwrap_or(Err(FetchFailure::Unreached.into()))?,
        };
        // `open` also answers a 304, which nothing asked for here.
        if !response.status().is_success() {
            return Err(FetchFailure::Status(response.status().as_u16()).into());
        }
        let declared = response.content_length();
        self.check_room(declared.unwrap_or(0))?;

        let started = Instant::now();
        let mut window_started = started;
        let mut window_bytes = 0u64;
        let mut last_gate = started;
        let mut last_reported = 0u64;
        let mut received = 0u64;
        let mut head: Vec<u8> = Vec::with_capacity(SNIFF_BYTES);
        // The file before its guard, so it is closed by the time the guard removes it.
        let mut output: Option<(tokio::fs::File, Partial, String)> = None;
        let mut ticks = tokio::time::interval(self.pace.progress_every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                () = cancelled(&mut cancel) => return Err(Stop::Cancelled),
                _ = ticks.tick() => {
                    let now = Instant::now();
                    if now.duration_since(last_gate) >= self.pace.gate_every {
                        last_gate = now;
                        gate(self.leases, self.fetcher).await?;
                    }
                    if now.duration_since(started) >= self.pace.body_deadline {
                        return Err(FetchFailure::Unreached.into());
                    }
                    if now.duration_since(window_started) >= self.pace.window {
                        if window_bytes < self.pace.min_bytes_per_window {
                            return Err(FetchFailure::Unreached.into());
                        }
                        window_started = now;
                        window_bytes = 0;
                    }
                    if received != last_reported {
                        last_reported = received;
                        self.observer.update(DownloadUpdate {
                            received_bytes: received,
                            total_bytes: declared,
                            ..DownloadUpdate::at(self.episode_id, DownloadState::Running)
                        });
                    }
                }
                chunk = response.chunk() => {
                    let Some(chunk) = chunk.map_err(|_| FetchFailure::Unreached)? else {
                        break;
                    };
                    received = received.saturating_add(chunk.len() as u64);
                    window_bytes = window_bytes.saturating_add(chunk.len() as u64);
                    self.check_room(received)?;
                    match output.as_mut() {
                        Some((file, ..)) => file.write_all(&chunk).await?,
                        None => {
                            head.extend_from_slice(&chunk);
                            if head.len() >= SNIFF_BYTES {
                                output = Some(self.open_output(&head).await?);
                            }
                        }
                    }
                }
            }
        }
        // A body too short to have been judged is no episode, whatever it starts with.
        let Some((mut file, partial, final_name)) = output else {
            return Err(FetchFailure::NotAudio.into());
        };
        // A declared length is a promise about the end of the body, and a body cut
        // short by a connection that closed cleanly is not an episode.
        if declared.is_some_and(|declared| declared != received) {
            return Err(FetchFailure::Unreached.into());
        }
        // On the disk before anything gives it its name: whatever finds the name
        // must find the bytes. Closed too, since Windows refuses to rename an open file.
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        Ok(Written {
            partial,
            final_name,
            size: received,
        })
    }

    /// Both ceilings against `bytes`, which is what has arrived, or a declared length
    /// that would have passed one anyway.
    fn check_room(&self, bytes: u64) -> Result<(), FetchFailure> {
        if bytes > self.pace.max_bytes {
            return Err(FetchFailure::TooLarge);
        }
        if bytes > self.room {
            return Err(FetchFailure::QuotaExceeded);
        }
        Ok(())
    }

    /// Judges the first bytes and only then creates anything. What is not audio
    /// leaves nothing on the disk, not even a partial file.
    async fn open_output(&self, head: &[u8]) -> Result<(tokio::fs::File, Partial, String), Stop> {
        let (suffix, _) = sniff_audio(head).ok_or(FetchFailure::NotAudio)?;
        tokio::fs::create_dir_all(self.dir).await?;
        let partial = Partial {
            path: self.dir.join(file_name(self.episode_id, PARTIAL_SUFFIX)),
        };
        let mut file = tokio::fs::File::create(&partial.path).await?;
        file.write_all(head).await?;
        Ok((file, partial, file_name(self.episode_id, suffix)))
    }
}

/// Gives the file its name and the row its record, under one lease, parked behind
/// a running scan as any background writer is.
async fn finish<L: Leases>(
    leases: &L,
    dir: &Path,
    episode_id: i64,
    written: Written,
    mut cancel: watch::Receiver<bool>,
) -> Result<u64, Stop> {
    while crate::commands::scan::scan_in_flight() {
        tokio::select! {
            biased;
            () = cancelled(&mut cancel) => return Err(Stop::Cancelled),
            _ = tokio::time::sleep(SCAN_WAIT_POLL) => {}
        }
    }
    let Written {
        partial,
        final_name,
        size,
    } = written;
    let pool = leases.lease().await?;
    // The last moment a cancel can still leave nothing behind.
    if *cancel.borrow() {
        return Err(Stop::Cancelled);
    }
    let target = dir.join(&final_name);
    rename_into_place(partial.path.clone(), target.clone()).await?;
    // Renamed, so the guard's remove finds nothing.
    drop(partial);
    record_in_place(&pool, &target, episode_id, &final_name, size, &cancel).await
}

/// Writes the row of a file now under its name, and takes the file back when it must
/// not stay.
///
/// `cancel` is read again once the row is written. A delete sets it before it reads
/// the row, so one that landed after the check before the rename is seen here, and
/// one that lands later finds the row and removes the file itself. Either way, what
/// the user removed does not come back.
pub(super) async fn record_in_place(
    pool: &sqlx::SqlitePool,
    target: &Path,
    episode_id: i64,
    file: &str,
    size: u64,
    cancel: &watch::Receiver<bool>,
) -> Result<u64, Stop> {
    match with_busy_retry(|| store::record_download(pool, episode_id, file, size)).await {
        Ok(true) if *cancel.borrow() => {
            remove_quietly(target).await;
            with_busy_retry(|| store::clear_download(pool, episode_id)).await?;
            Err(Stop::Cancelled)
        }
        Ok(true) => Ok(size),
        Ok(false) => {
            remove_quietly(target).await;
            Err(Stop::Word(EPISODE_GONE.to_owned()))
        }
        Err(err) => {
            remove_quietly(target).await;
            Err(err.into())
        }
    }
}

// ── the queue ───────────────────────────────────────────────────────

/// One episode asked for. The profile is part of what it is: episode ids are only
/// unique inside one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Job {
    pub profile_id: i64,
    pub channel_id: i64,
    pub episode_id: i64,
}

struct Running {
    job: Job,
    cancel: watch::Sender<bool>,
}

struct QueueState {
    waiting: VecDeque<Job>,
    running: Option<Running>,
    /// A worker is draining the queue, so nobody needs to start one.
    worker: bool,
}

/// What asking for a job did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pushed {
    /// It was already asked for.
    Already,
    /// In line, with a worker on it.
    Queued,
    /// In line, and the caller is to start the worker.
    QueuedStartWorker,
}

/// The downloads asked for, one running and the others in line.
pub(crate) struct Queue {
    state: Mutex<QueueState>,
}

impl Queue {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                waiting: VecDeque::new(),
                running: None,
                worker: false,
            }),
        }
    }

    fn with<T>(&self, change: impl FnOnce(&mut QueueState) -> T) -> T {
        // Plain data with no invariant spanning a panic, so carrying on is better
        // than taking every later download down with it.
        change(&mut self.state.lock().unwrap_or_else(|err| err.into_inner()))
    }

    pub(crate) fn push(&self, job: Job) -> Pushed {
        self.with(|state| {
            let same = |other: &Job| {
                other.profile_id == job.profile_id && other.episode_id == job.episode_id
            };
            if state.waiting.iter().any(same)
                || state.running.as_ref().is_some_and(|r| same(&r.job))
            {
                return Pushed::Already;
            }
            state.waiting.push_back(job);
            if state.worker {
                Pushed::Queued
            } else {
                state.worker = true;
                Pushed::QueuedStartWorker
            }
        })
    }

    /// The next job and what tells it to stop, or `None`, which retires the worker.
    fn next(&self) -> Option<(Job, watch::Sender<bool>)> {
        self.with(|state| {
            state.running = None;
            let Some(job) = state.waiting.pop_front() else {
                state.worker = false;
                return None;
            };
            let (cancel, _) = watch::channel(false);
            state.running = Some(Running {
                job,
                cancel: cancel.clone(),
            });
            Some((job, cancel))
        })
    }

    /// Removes one job from the line, or tells it to stop when it is running. True
    /// when it was taken out of the line, so nothing will report it but the caller.
    pub(crate) fn cancel(&self, profile_id: i64, episode_id: i64) -> bool {
        self.with(|state| {
            let before = state.waiting.len();
            state
                .waiting
                .retain(|job| job.profile_id != profile_id || job.episode_id != episode_id);
            if state.waiting.len() != before {
                return true;
            }
            if let Some(running) = state.running.as_ref() {
                if running.job.profile_id == profile_id && running.job.episode_id == episode_id {
                    running.cancel.send_replace(true);
                }
            }
            false
        })
    }

    /// [`Self::cancel`] for every episode of a channel: the jobs taken out of the
    /// line, which only the caller will report.
    pub(crate) fn cancel_channel(&self, profile_id: i64, channel_id: i64) -> Vec<Job> {
        self.with(|state| {
            let of_channel =
                |job: &Job| job.profile_id == profile_id && job.channel_id == channel_id;
            let taken: Vec<Job> = state.waiting.iter().copied().filter(of_channel).collect();
            state.waiting.retain(|job| !of_channel(job));
            if let Some(running) = state.running.as_ref().filter(|r| of_channel(&r.job)) {
                running.cancel.send_replace(true);
            }
            taken
        })
    }

    /// The episode running and those in line for a profile, which is what a page
    /// opened meanwhile has to go on: no event replays for it.
    pub(crate) fn in_flight(&self, profile_id: i64) -> (Option<i64>, Vec<i64>) {
        self.with(|state| {
            (
                state
                    .running
                    .as_ref()
                    .filter(|r| r.job.profile_id == profile_id)
                    .map(|r| r.job.episode_id),
                state
                    .waiting
                    .iter()
                    .filter(|job| job.profile_id == profile_id)
                    .map(|job| job.episode_id)
                    .collect(),
            )
        })
    }

    /// Runs the jobs one at a time until the line is empty.
    pub(crate) async fn drain<F, Fut>(&self, run: F)
    where
        F: Fn(Job, watch::Sender<bool>) -> Fut,
        Fut: Future<Output = ()>,
    {
        /// So a panic in a download does not leave a worker nobody would start again.
        struct Retire<'a>(&'a Queue);
        impl Drop for Retire<'_> {
            fn drop(&mut self) {
                self.0.with(|state| {
                    state.running = None;
                    state.worker = false;
                });
            }
        }
        let retire = Retire(self);
        while let Some((job, cancel)) = self.next() {
            run(job, cancel).await;
        }
        // `next` has retired this worker, and another may have started since, whose
        // flag is not this one's to clear.
        std::mem::forget(retire);
    }
}

static QUEUE: Queue = Queue::new();

pub(crate) fn queue() -> &'static Queue {
    &QUEUE
}

// ── what is left after a crash ──────────────────────────────────────

/// What a sweep did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Swept {
    pub partial_files: usize,
    pub orphan_files: usize,
    pub orphan_rows: usize,
}

/// Removes partial files and files no row names, and clears rows whose file is gone.
///
/// `writing` says whether an episode is being fetched right now, whose file is
/// legitimately not recorded yet. A name in the folder that is not ours is left
/// alone, since nothing here knows it.
///
/// The order is what keeps a download finishing during a sweep from passing for an
/// orphan. It records its row once the file has its name, and stops `writing` after
/// that, so: the folder is listed, what is `writing` is left out of it, and only then
/// are the rows read. A file listed whose download has already stopped `writing` has
/// its row among them. `writing` is asked again before each removal, for a download
/// that began meanwhile, and each row's file is looked for last.
///
/// A row is cleared only when the folder can be listed. `check_rows` is false when
/// this session fell back to the default location because the caches' drive was
/// unplugged at launch, and a folder that cannot be listed covers one that goes away
/// afterwards: clearing the rows would lose every download by the time the drive
/// returns, since the next sweep would then find files no row names. A missing folder
/// proves nothing either, and neither does the root above it, which can be an empty
/// mount point; the profile's folders are even made again on it at each switch. A
/// folder removed by hand keeps its rows until a download makes it again, and the
/// episodes stream meanwhile.
pub(crate) async fn sweep(
    pool: &sqlx::SqlitePool,
    dir: &Path,
    writing: impl Fn(i64) -> bool + Send + 'static,
    check_rows: bool,
) -> Result<Swept, sqlx::Error> {
    let on_blocking_pool = |err| sqlx::Error::Io(std::io::Error::other(err));
    let folder = dir.to_path_buf();
    let listed: Vec<String> = tokio::task::spawn_blocking(move || {
        std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| episode_of(name).is_some())
                    .collect()
            })
            .unwrap_or_default()
    })
    .await
    .map_err(on_blocking_pool)?;
    let listed: Vec<String> = listed
        .into_iter()
        .filter(|name| !episode_of(name).is_some_and(&writing))
        .collect();

    let rows = store::all_downloads(pool).await?;
    let recorded: HashSet<String> = rows.iter().map(|(_, name)| name.clone()).collect();
    let folder = dir.to_path_buf();
    let (partial_files, orphan_files, gone) = tokio::task::spawn_blocking(move || {
        let (mut partials, mut orphans) = (0, 0);
        for name in listed {
            let partial = name.ends_with(&format!(".{PARTIAL_SUFFIX}"));
            if (!partial && recorded.contains(&name)) || episode_of(&name).is_some_and(&writing) {
                continue;
            }
            if std::fs::remove_file(folder.join(&name)).is_ok() {
                if partial {
                    partials += 1;
                } else {
                    orphans += 1;
                }
            }
        }
        let check_rows = check_rows && std::fs::read_dir(&folder).is_ok();
        let gone: Vec<i64> = rows
            .iter()
            .filter(|(id, name)| check_rows && stored_file(&folder, *id, name).is_none())
            .map(|(episode_id, _)| *episode_id)
            .collect();
        (partials, orphans, gone)
    })
    .await
    .map_err(on_blocking_pool)?;

    for episode_id in &gone {
        with_busy_retry(|| store::clear_download(pool, *episode_id)).await?;
    }
    Ok(Swept {
        partial_files,
        orphan_files,
        orphan_rows: gone.len(),
    })
}

static SWEEP_WAKE: Notify = Notify::const_new();

/// The profile whose view is on screen. Episode ids are only unique inside a profile,
/// so an event about another profile's job would land on an episode of this one's.
static ACTIVE_PROFILE: AtomicI64 = AtomicI64::new(0);

/// Another profile became active: what a crash left in its folder has not been
/// looked at yet, and its view hears nothing more of the last one's jobs.
pub(crate) fn profile_changed(profile_id: i64) {
    ACTIVE_PROFILE.store(profile_id, Ordering::SeqCst);
    SWEEP_WAKE.notify_one();
}

pub(super) fn is_active(profile_id: i64) -> bool {
    ACTIVE_PROFILE.load(Ordering::SeqCst) == profile_id
}

/// Sweeps the active profile at launch, and each profile as it becomes active.
///
/// Reads and removes local files only, so it contacts nobody and needs no switch: a
/// download kept with podcasts turned off stays, and what a crash left does not.
pub(crate) fn spawn_sweeper(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            sweep_active_profile(&app).await;
            SWEEP_WAKE.notified().await;
        }
    });
}

async fn sweep_active_profile(app: &AppHandle) {
    // Before the lease: a scan may last an hour, and a lease held through it would
    // make a profile switch wait out its drain timeout.
    while crate::commands::scan::scan_in_flight() {
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
    let state = app.state::<AppState>();
    let Ok((pool, profile_id)) = state.require_profile_snapshot().await else {
        return;
    };
    let dir = state.paths.profile_podcast_download_dir(profile_id);
    let check_rows = state.cache_root_fallback.is_none();
    let writing = move |episode_id| queue().in_flight(profile_id).0 == Some(episode_id);
    match sweep(&pool, &dir, writing, check_rows).await {
        Ok(swept) if swept != Swept::default() => tracing::info!(
            profile_id,
            partial_files = swept.partial_files,
            orphan_files = swept.orphan_files,
            orphan_rows = swept.orphan_rows,
            "podcast downloads: cleaned what a stop left behind"
        ),
        Ok(_) => {}
        Err(err) => tracing::warn!(profile_id, %err, "podcast downloads: sweep failed"),
    }
}

// ── the app's side ──────────────────────────────────────────────────

/// Reports one download to the page and the status bar.
struct AppObserver<'a> {
    app: &'a AppHandle,
    profile_id: i64,
    task: Option<crate::tasks::TaskHandle>,
}

impl DownloadObserver for AppObserver<'_> {
    fn update(&self, update: DownloadUpdate) {
        if let Some(task) = self.task.as_ref() {
            task.progress(update.received_bytes, update.total_bytes.unwrap_or(0));
        }
        if is_active(self.profile_id) {
            let _ = self.app.emit(PODCAST_DOWNLOAD, update);
        }
    }

    fn titled(&self, title: &str) {
        if let Some(task) = self.task.as_ref() {
            if !title.is_empty() {
                task.detail(title);
            }
        }
    }
}

/// See [`PODCAST_DOWNLOAD`]. Says nothing about a profile that has been left: a
/// command may still be on its way when the switch lands.
pub(crate) fn emit(app: &AppHandle, profile_id: i64, episode_id: i64, state: DownloadState) {
    if is_active(profile_id) {
        let _ = app.emit(PODCAST_DOWNLOAD, DownloadUpdate::at(episode_id, state));
    }
}

/// Puts an episode in line and starts the worker when none is running.
pub(crate) fn enqueue(app: &AppHandle, job: Job) {
    let pushed = queue().push(job);
    if pushed == Pushed::Already {
        return;
    }
    emit(app, job.profile_id, job.episode_id, DownloadState::Queued);
    if pushed == Pushed::QueuedStartWorker {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            queue()
                .drain(|job, cancel| run_job(&app, job, cancel))
                .await;
        });
    }
}

async fn run_job(app: &AppHandle, job: Job, cancel: watch::Sender<bool>) {
    let state = app.state::<AppState>();
    let stop = cancel.clone();
    let task = crate::tasks::start(
        app,
        crate::tasks::TaskKind::PodcastDownload,
        0,
        crate::tasks::cancel_fn(move || {
            stop.send_replace(true);
        }),
    );
    let observer = AppObserver {
        app,
        profile_id: job.profile_id,
        task,
    };
    let dir = state.paths.profile_podcast_download_dir(job.profile_id);
    let leases = ProfileLeases {
        state: &state,
        profile_id: job.profile_id,
    };
    let ended = match Fetcher::new() {
        Ok(fetcher) => {
            download_episode(
                &leases,
                &fetcher,
                &dir,
                job.episode_id,
                Pace::standard(),
                cancel.subscribe(),
                &observer,
            )
            .await
        }
        Err(_) => Ended::Failed(FetchFailure::Unreached.code()),
    };
    let last = match &ended {
        Ended::Stored { size } => {
            if size.is_some() && is_active(job.profile_id) {
                emit_updated(app, Some(job.channel_id));
            }
            DownloadUpdate {
                received_bytes: size.unwrap_or(0),
                total_bytes: *size,
                ..DownloadUpdate::at(job.episode_id, DownloadState::Done)
            }
        }
        Ended::Cancelled => DownloadUpdate::at(job.episode_id, DownloadState::Cancelled),
        Ended::Failed(word) => DownloadUpdate {
            error_word: Some(word.clone()),
            ..DownloadUpdate::at(job.episode_id, DownloadState::Failed)
        },
    };
    if is_active(job.profile_id) {
        let _ = app.emit(PODCAST_DOWNLOAD, last);
    }
}

/// Removes the files a channel held, after its rows have gone. One that refuses
/// stays until the next sweep, which finds a file no row names.
pub(crate) async fn remove_files(dir: PathBuf, files: Vec<(i64, String)>) {
    let _ = tokio::task::spawn_blocking(move || {
        for (episode_id, name) in files {
            if let Some(path) = stored_file(&dir, episode_id, &name) {
                if let Err(err) = std::fs::remove_file(path) {
                    tracing::debug!(kind = ?err.kind(), "podcast download: a file stayed");
                }
            }
        }
    })
    .await;
}

/// Removes one episode's file, then what its row says of it: the row stays, since
/// its position is the user's. A file that refuses to go keeps its record, or it
/// would hold disk space nothing offers to give back.
pub(crate) async fn delete_download(
    pool: &sqlx::SqlitePool,
    dir: &Path,
    episode_id: i64,
) -> Result<bool, PodcastError> {
    let Some(name) = store::download_file_of(pool, episode_id).await? else {
        return Ok(false);
    };
    if let Some(path) = stored_file(dir, episode_id, &name) {
        tokio::task::spawn_blocking(move || std::fs::remove_file(path))
            .await
            .map_err(|err| {
                PodcastError::Other(crate::error::AppError::Other(format!(
                    "podcast delete join: {err}"
                )))
            })?
            .or_else(|err| match err.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(err),
            })
            .map_err(|err| PodcastError::Other(err.into()))?;
    }
    with_busy_retry(|| store::clear_download(pool, episode_id)).await?;
    Ok(true)
}
