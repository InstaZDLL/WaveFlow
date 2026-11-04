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
/// if it is not a feed. Subscribing to an address already followed answers the
/// channel it belongs to without contacting anybody.
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

/// Stops following a channel and forgets its episodes. Available with the switch
/// off: leaving must not depend on having opted in.
#[tauri::command]
pub async fn podcast_unsubscribe(
    app: AppHandle,
    state: State<'_, AppState>,
    channel_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    if store::delete_channel(&pool, channel_id).await? {
        emit_updated(&app, Some(channel_id));
    }
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
    let pool = state.require_profile_pool().await?;
    let rows = store::export_rows(&pool).await?;
    let without_address = store::count_without_address(&pool).await?;
    let written = rows.len();
    let document = write_opml(
        &rows
            .into_iter()
            .map(|(title, feed_url)| OpmlEntry {
                title: Some(title),
                feed_url,
            })
            .collect::<Vec<_>>(),
    );
    // Through a partial file renamed into place, on the blocking pool: the
    // destination is wherever the person picked, network shares included.
    tokio::task::spawn_blocking(move || {
        waveflow_core::atomic_write::write_bytes(Path::new(&path), document.as_bytes())
    })
    .await
    .map_err(|err| AppError::Other(format!("opml write join: {err}")))?
    .map_err(|err| AppError::Other(format!("opml write: {err}")))?;
    Ok(OpmlExportReport {
        written: written as u32,
        without_address: without_address as u32,
    })
}
