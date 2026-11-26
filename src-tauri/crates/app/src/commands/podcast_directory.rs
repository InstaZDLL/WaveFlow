//! The podcast directory: thin commands over [`crate::podcast::directory`].
//!
//! A show goes to the webview with its id in the directory and never with its
//! feed's address: subscribing asks by that id. Refusals are `podcast: <word>`,
//! as the other podcast commands' are, plus `rate_limited`, `directory_invalid`
//! and `no_feed`.

use serde::Serialize;
use tauri::{AppHandle, State};
use waveflow_core::podcast::address::{feed_key, parse_address};
use waveflow_core::podcast::directory::{country_code, search_term, DirectoryShow};

use crate::commands::podcast::subscribe_and_answer;
use crate::error::{AppError, AppResult};
use crate::podcast::directory;
use crate::podcast::fetch::Fetcher;
use crate::podcast::refresh::{Leases, PodcastError};
use crate::podcast::schedule::ProfileLeases;
use crate::podcast::store::{self, PodcastChannel};
use crate::state::AppState;

/// One show of a chart or a search, as the page draws it.
#[derive(Debug, Clone, Serialize)]
pub struct DirectoryShowRow {
    /// The directory's id, which every other directory command takes.
    pub id: u64,
    pub title: String,
    pub author: Option<String>,
    pub genre_id: Option<u32>,
    /// Whether `podcast_directory_artwork` may have a picture for it.
    pub has_artwork: bool,
    pub episode_count: Option<u32>,
    pub explicit: bool,
    /// The channel it already is in this profile, when the directory gave its
    /// feed and that feed is followed. A channel restored without its address
    /// is not counted: subscribing again is how it gets it back.
    pub channel_id: Option<i64>,
}

fn country(raw: &str) -> AppResult<String> {
    country_code(raw).ok_or_else(|| AppError::Other("podcast directory: invalid country".into()))
}

/// The rows of `shows`, each marked with the channel it already is.
async fn rows(
    leases: &ProfileLeases<'_>,
    shows: &[DirectoryShow],
) -> AppResult<Vec<DirectoryShowRow>> {
    let pool = leases.lease().await?;
    let mut rows = Vec::with_capacity(shows.len());
    for show in shows {
        let channel_id = match show.feed_url.as_deref().and_then(parse_address) {
            Some(url) => match store::find_by_key(&pool, &feed_key(&url)).await? {
                Some((id, true)) => Some(id),
                _ => None,
            },
            None => None,
        };
        rows.push(DirectoryShowRow {
            id: show.id,
            title: show.title.clone(),
            author: show.author.clone(),
            genre_id: show.genre_id,
            has_artwork: show.artwork_url.is_some(),
            episode_count: show.episode_count,
            explicit: show.explicit,
            channel_id,
        });
    }
    Ok(rows)
}

/// `country`'s chart of shows, narrowed to `genre` when there is one.
#[tauri::command]
pub async fn podcast_directory_top(
    state: State<'_, AppState>,
    country: String,
    genre: Option<u32>,
) -> AppResult<Vec<DirectoryShowRow>> {
    let country = self::country(&country)?;
    let (_, profile_id) = state.require_profile_snapshot().await?;
    let leases = ProfileLeases {
        state: &state,
        profile_id,
    };
    let fetcher = Fetcher::new().map_err(PodcastError::from)?;
    let shows = directory::shared()
        .chart(&leases, &fetcher, &country, genre)
        .await?;
    rows(&leases, &shows).await
}

/// The shows a search for `term` finds in `country`'s storefront. A term with
/// nothing in it asks nothing and finds nothing.
#[tauri::command]
pub async fn podcast_directory_search(
    state: State<'_, AppState>,
    country: String,
    term: String,
) -> AppResult<Vec<DirectoryShowRow>> {
    let country = self::country(&country)?;
    let Some(term) = search_term(&term) else {
        return Ok(Vec::new());
    };
    let (_, profile_id) = state.require_profile_snapshot().await?;
    let leases = ProfileLeases {
        state: &state,
        profile_id,
    };
    let fetcher = Fetcher::new().map_err(PodcastError::from)?;
    let shows = directory::shared()
        .search(&leases, &fetcher, &country, &term)
        .await?;
    rows(&leases, &shows).await
}

/// A show's picture as a `data:` address, `None` when there is none to show.
/// The webview loads nothing from the directory's hosts itself.
#[tauri::command]
pub async fn podcast_directory_artwork(
    state: State<'_, AppState>,
    id: u64,
) -> AppResult<Option<String>> {
    let (_, profile_id) = state.require_profile_snapshot().await?;
    let leases = ProfileLeases {
        state: &state,
        profile_id,
    };
    let fetcher = Fetcher::new().map_err(PodcastError::from)?;
    Ok(directory::shared()
        .picture(&leases, &fetcher, id)
        .await?
        .map(|picture| picture.to_string()))
}

/// Subscribes to the show `id`, by the feed the directory gives for it, on the
/// public internet only. Answers its channel, as `podcast_subscribe` does.
#[tauri::command]
pub async fn podcast_directory_subscribe(
    app: AppHandle,
    state: State<'_, AppState>,
    country: String,
    id: u64,
) -> AppResult<PodcastChannel> {
    let country = self::country(&country)?;
    let (_, profile_id) = state.require_profile_snapshot().await?;
    let leases = ProfileLeases {
        state: &state,
        profile_id,
    };
    let fetcher = Fetcher::new().map_err(PodcastError::from)?;
    let feed = directory::shared()
        .feed_of(&leases, &fetcher, &country, id)
        .await?;
    subscribe_and_answer(&app, &state, &leases, &fetcher, feed.as_str()).await
}
