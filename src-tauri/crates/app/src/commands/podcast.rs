//! Podcasts: thin commands over [`crate::podcast`].
//!
//! A refusal reaches the frontend as `podcast: <word>`, the word from a fixed
//! list. Nothing here returns an address: an OPML export writes them to the file
//! the person chose, and nowhere else.

use std::io::Read;
use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, State};
use waveflow_core::artwork::metadata::existing_path;
use waveflow_core::podcast::opml::{read_opml, write_opml};
use waveflow_core::podcast::{FetchFailure, OpmlEntry, MAX_FEED_BYTES};

use crate::error::{AppError, AppResult};
use crate::podcast::download::{self, DownloadState, DownloadUpdate, Job};
use crate::podcast::fetch::Fetcher;
use crate::podcast::refresh::{
    self, Leases, OpmlImportReport, PodcastError, RefreshSummary, Subscribed,
};
use crate::podcast::schedule::{self, emit_updated, ProfileLeases};
use crate::podcast::store::{self, PodcastChannel, PodcastEpisode};
use crate::state::AppState;

/// The most rows one list call answers. The page is virtual and asks by page.
const MAX_EPISODE_PAGE: i64 = 500;

fn with_artwork_path(mut channel: PodcastChannel, state: &AppState) -> PodcastChannel {
    channel.artwork_path = channel
        .artwork_hash
        .as_deref()
        .and_then(|hash| existing_path(&state.paths.metadata_artwork_dir, hash));
    channel
}

/// Fetches the feed at `feed_url` and follows it. Refuses, and stores nothing,
/// if it is not a feed. Subscribing to the address a channel is stored under answers
/// that channel without contacting anybody.
#[tauri::command]
pub async fn podcast_subscribe(
    app: AppHandle,
    state: State<'_, AppState>,
    feed_url: String,
) -> AppResult<PodcastChannel> {
    let (_, profile_id) = state.require_profile_snapshot().await?;
    let leases = ProfileLeases {
        state: &state,
        profile_id,
    };
    let fetcher = Fetcher::new().map_err(PodcastError::from)?;
    let subscribed = refresh::subscribe(
        &leases,
        &fetcher,
        &feed_url,
        &state.paths.metadata_artwork_dir,
    )
    .await?;
    if matches!(subscribed, Subscribed::Added(_)) {
        emit_updated(&app, Some(subscribed.channel_id()));
    }
    let pool = leases.lease().await?;
    let channel = store::channel(&pool, subscribed.channel_id())
        .await?
        .ok_or_else(|| AppError::Other("podcast: channel_gone".into()))?;
    Ok(with_artwork_path(channel, &state))
}

/// Stops following a channel, forgets its episodes and removes the files they had
/// downloaded. Available with the switch off: leaving must not depend on having
/// opted in.
#[tauri::command]
pub async fn podcast_unsubscribe(
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: i64,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    for job in download::queue().cancel_channel(profile_id, channel_id) {
        download::emit(&app, job.episode_id, DownloadState::Cancelled);
    }
    let files = store::downloads_of_channel(&pool, channel_id).await?;
    if store::delete_channel(&pool, channel_id).await? {
        emit_updated(&app, Some(channel_id));
    }
    download::remove_files(state.paths.profile_podcast_download_dir(profile_id), files).await;
    Ok(())
}

/// Every channel of the active profile. Reads with the switch off too: what was
/// stored stays readable.
#[tauri::command]
pub async fn podcast_list_channels(state: State<'_, AppState>) -> AppResult<Vec<PodcastChannel>> {
    let pool = state.require_profile_pool().await?;
    Ok(store::list_channels(&pool)
        .await?
        .into_iter()
        .map(|channel| with_artwork_path(channel, &state))
        .collect())
}

/// A page of a channel's episodes, newest first.
#[tauri::command]
pub async fn podcast_list_episodes(
    state: State<'_, AppState>,
    channel_id: i64,
    limit: Option<i64>,
    offset: Option<i64>,
) -> AppResult<Vec<PodcastEpisode>> {
    let pool = state.require_profile_pool().await?;
    let limit = limit.unwrap_or(MAX_EPISODE_PAGE).clamp(1, MAX_EPISODE_PAGE);
    Ok(store::list_episodes(&pool, channel_id, limit, offset.unwrap_or(0).max(0)).await?)
}

/// Asks one channel, or all of them, for its feed. Refused with the switch off
/// or in offline mode, before anything is contacted.
#[tauri::command]
pub async fn podcast_refresh(app: AppHandle, channel_id: Option<i64>) -> AppResult<RefreshSummary> {
    let summary = schedule::run_refresh_pass(&app, channel_id).await?;
    match summary.outcome {
        refresh::PassOutcome::Disabled => Err(PodcastError::Disabled.into()),
        refresh::PassOutcome::Offline => Err(PodcastError::Offline.into()),
        _ => Ok(summary),
    }
}

/// Subscribes to every feed of an OPML file, one at a time.
#[tauri::command]
pub async fn podcast_import_opml(app: AppHandle, path: String) -> AppResult<OpmlImportReport> {
    let entries = tokio::task::spawn_blocking(move || read_opml_file(Path::new(&path)))
        .await
        .map_err(|err| AppError::Other(format!("opml read join: {err}")))??;
    Ok(schedule::run_import_pass(&app, entries).await?)
}

/// The list in the file at `path`, refused unless it is a regular file named
/// like a subscription list. The path comes from the webview, and this only
/// ever hands a file's outlines to the fetcher, but it has no business opening
/// a pipe or a device, which would block.
fn read_opml_file(path: &Path) -> AppResult<Vec<OpmlEntry>> {
    let named_like_a_list = |p: &Path| {
        p.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("opml") || extension.eq_ignore_ascii_case("xml")
            })
    };
    let refused = || AppError::from(PodcastError::Failed(FetchFailure::InvalidFeed));
    let target = std::fs::canonicalize(path)?;
    if !named_like_a_list(path)
        || !named_like_a_list(&target)
        || !std::fs::metadata(&target)?.is_file()
    {
        return Err(refused());
    }
    // Bounded by the read itself, not by the reported length.
    let mut bytes = Vec::new();
    std::fs::File::open(&target)?
        .take(MAX_FEED_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    read_opml(&bytes).ok_or_else(refused)
}

/// What an export wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OpmlExportReport {
    pub written: u32,
    /// Channels a restored archive left without an address, which cannot be
    /// written and are not lost: they are still in the profile.
    pub without_address: u32,
}

/// Writes every channel's address to the file at `path`. A deliberate act, and
/// the one place feed addresses leave the profile: the dialog that calls this
/// says so.
#[tauri::command]
pub async fn podcast_export_opml(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<OpmlExportReport> {
    let (document, report) = {
        let pool = state.require_profile_pool().await?;
        opml_export(&pool).await?
    };
    // Through a partial file renamed into place, on the blocking pool: the
    // destination is wherever the person picked, network shares included.
    tokio::task::spawn_blocking(move || {
        waveflow_core::atomic_write::write_bytes(Path::new(&path), document.as_bytes())
    })
    .await
    .map_err(|err| AppError::Other(format!("opml write join: {err}")))?
    .map_err(|err| AppError::Other(format!("opml write: {err}")))?;
    Ok(report)
}

/// The document an export writes, and what it says about it. Apart from the file: the
/// destination may be a share that takes its time, and a lease held across that would
/// make a profile switch wait it out.
pub(crate) async fn opml_export(
    pool: &sqlx::SqlitePool,
) -> Result<(String, OpmlExportReport), sqlx::Error> {
    let rows = store::export_rows(pool).await?;
    let without_address = store::count_without_address(pool).await?;
    let report = OpmlExportReport {
        written: rows.len() as u32,
        without_address: without_address as u32,
    };
    let document = write_opml(
        &rows
            .into_iter()
            .map(|(title, feed_url)| OpmlEntry {
                title: Some(title),
                feed_url,
            })
            .collect::<Vec<_>>(),
    );
    Ok((document, report))
}

/// Puts an episode in line to be kept on disk, one at a time. Answers once it is in
/// line: the event `podcast:download` says the rest. Refused with the switch off or
/// in offline mode, before anything is contacted. Asking for one already in line or
/// already on disk starts nothing.
#[tauri::command]
pub async fn podcast_download_episode(
    app: AppHandle,
    state: State<'_, AppState>,
    episode_id: i64,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    if !store::is_enabled(&pool).await? {
        return Err(PodcastError::Disabled.into());
    }
    if crate::offline::is_offline() {
        return Err(PodcastError::Offline.into());
    }
    let episode = store::episode_download(&pool, episode_id)
        .await?
        .ok_or_else(|| AppError::Other(format!("podcast: {}", download::EPISODE_GONE)))?;
    // A row restored from an archive until the feed is subscribed to again.
    if episode.enclosure_url.is_none() {
        return Err(PodcastError::Failed(FetchFailure::AddressRefused).into());
    }
    download::enqueue(
        &app,
        Job {
            profile_id,
            channel_id: episode.channel_id,
            episode_id,
        },
    );
    Ok(())
}

/// Takes an episode out of the line, or stops it where it is, and nothing is left on
/// the disk. Nothing happens for one that is neither.
#[tauri::command]
pub async fn podcast_cancel_download(
    app: AppHandle,
    state: State<'_, AppState>,
    episode_id: i64,
) -> AppResult<()> {
    let profile_id = state.require_profile_id().await?;
    if download::queue().cancel(profile_id, episode_id) {
        download::emit(&app, episode_id, DownloadState::Cancelled);
    }
    Ok(())
}

/// Removes an episode's file and keeps its row, so its position stays. Available
/// with the switch off and offline: what was kept stays removable.
#[tauri::command]
pub async fn podcast_delete_download(
    app: AppHandle,
    state: State<'_, AppState>,
    episode_id: i64,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    if download::queue().cancel(profile_id, episode_id) {
        download::emit(&app, episode_id, DownloadState::Cancelled);
    }
    let dir = state.paths.profile_podcast_download_dir(profile_id);
    if download::delete_download(&pool, &dir, episode_id).await? {
        emit_updated(&app, None);
    }
    Ok(())
}

/// What the kept episodes weigh and what they may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PodcastDownloadUsage {
    pub used_bytes: u64,
    pub quota_bytes: u64,
}

#[tauri::command]
pub async fn podcast_download_usage(state: State<'_, AppState>) -> AppResult<PodcastDownloadUsage> {
    let pool = state.require_profile_pool().await?;
    Ok(PodcastDownloadUsage {
        used_bytes: store::download_used_bytes(&pool, None).await?,
        quota_bytes: store::download_quota_bytes(&pool).await?,
    })
}

/// The downloads under way for the active profile, for a page opened meanwhile: no
/// event replays for it. Numbers come with the next `podcast:download`.
#[tauri::command]
pub async fn podcast_download_queue(state: State<'_, AppState>) -> AppResult<Vec<DownloadUpdate>> {
    let profile_id = state.require_profile_id().await?;
    let (running, waiting) = download::queue().in_flight(profile_id);
    Ok(running
        .into_iter()
        .map(|id| DownloadUpdate::at(id, DownloadState::Running))
        .chain(
            waiting
                .into_iter()
                .map(|id| DownloadUpdate::at(id, DownloadState::Queued)),
        )
        .collect())
}
