//! Public Rust service API for native frontends. Call from a Tokio worker;
//! no method in this module is intended to run on the GTK thread.
use crate::{
    audio::{AudioCmd, AudioEngine, PlayerState},
    error::{AppError, AppResult},
    host::{AppHandle, Event},
    paths::AppPaths,
    player_actions, queue,
    state::AppState,
};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
};
use waveflow_core::{
    domain::{playlist::Playlist, track::TrackRow},
    repository::{
        playlist::PlaylistRepository,
        sqlite::{SqlitePlaylistRepository, SqliteTrackRepository},
        track::{TrackListFilter, TrackRepository, TrackSort},
    },
};

pub struct Backend {
    state: Arc<AppState>,
    host: AppHandle,
    // Host keeps a weak reference to this service slot, not a cycle.
    // The outer Arc intentionally mirrors Tauri's `State<Arc<AudioEngine>>`
    // shape for the existing shared playback source.
    #[allow(clippy::redundant_allocation)]
    engine: Arc<Arc<AudioEngine>>,
}

#[derive(Clone, Debug)]
pub struct Playback {
    pub state: PlayerState,
    pub position_ms: u64,
    pub volume: f32,
    pub track_id: i64,
}

pub struct Artwork {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum TrackView {
    Library,
    Favorites,
    History,
    Playlist(i64),
}

impl Backend {
    pub async fn open(
        root: Option<PathBuf>,
    ) -> AppResult<(Self, tokio::sync::broadcast::Receiver<Event>)> {
        // Match the existing Tauri identifier, not the Flatpak application id.
        let root = match root {
            Some(root) => root,
            None => AppPaths::root_for_identifier("app.waveflow")?,
        };
        let state = AppState::open(root).await?;
        let host = AppHandle::new(state.clone());
        let events = host.subscribe();
        let engine_host = host.clone();
        let engine = tokio::task::spawn_blocking(move || AudioEngine::new(engine_host))
            .await
            .map_err(|e| AppError::Other(e.to_string()))?;
        let engine = Arc::new(engine);
        host.attach_engine(&engine);
        let pool = state.require_profile_pool().await?;
        if let Some(volume) = queue::read_player_volume(&pool).await {
            engine.shared().set_volume(volume);
        }
        drop(pool);
        Ok((
            Self {
                state,
                host,
                engine,
            },
            events,
        ))
    }

    pub fn playback(&self) -> Playback {
        let shared = self.engine.shared();
        Playback {
            state: shared.state(),
            position_ms: shared.current_position_ms(),
            volume: shared.volume(),
            track_id: shared.current_track_id.load(Ordering::Acquire),
        }
    }

    pub fn data_root(&self) -> &std::path::Path {
        &self.state.paths.root
    }

    pub async fn tracks(&self, view: &TrackView, search: &str) -> AppResult<Vec<TrackRow>> {
        let pool = self.state.require_profile_pool().await?;
        let repo = SqliteTrackRepository::new((*pool).clone());
        let mut rows = match view {
            TrackView::Library => {
                if let Some(plan) = waveflow_core::search::plan_search(search) {
                    return Ok(repo.search(&plan, 1000).await?);
                }
                repo.list(TrackListFilter::default(), TrackSort::default())
                    .await?
            }
            TrackView::Favorites => repo.list_liked().await?,
            TrackView::Playlist(id) => repo.list_in_playlist(*id).await?,
            TrackView::History => {
                let ids: Vec<i64> = sqlx::query_scalar(
                    "SELECT track_id FROM play_event WHERE track_id IS NOT NULL
                     GROUP BY track_id ORDER BY MAX(played_at) DESC LIMIT 100",
                )
                .fetch_all(&*pool)
                .await?;
                let mut rows = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(row) = repo.get(id).await? {
                        rows.push(row);
                    }
                }
                rows
            }
        };
        if !search.trim().is_empty() {
            let query = search.trim().to_lowercase();
            rows.retain(|t| {
                format!(
                    "{} {} {}",
                    t.title,
                    t.artist_name.as_deref().unwrap_or(""),
                    t.album_title.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&query)
            });
        }
        Ok(rows)
    }

    pub async fn playlists(&self) -> AppResult<Vec<Playlist>> {
        let pool = self.state.require_profile_pool().await?;
        Ok(SqlitePlaylistRepository::new((*pool).clone())
            .list_all_with_counts()
            .await?)
    }

    pub async fn active_profile_name(&self) -> AppResult<String> {
        let profile_id = self.state.require_profile_id().await?;
        sqlx::query_scalar("SELECT name FROM profile WHERE id = ?")
            .bind(profile_id)
            .fetch_one(&self.state.app_db)
            .await
            .map_err(Into::into)
    }

    pub async fn current_track(&self) -> AppResult<Option<TrackRow>> {
        let id = self.playback().track_id;
        let pool = self.state.require_profile_pool().await?;
        let repo = SqliteTrackRepository::new((*pool).clone());
        if id > 0 {
            return Ok(repo.get(id).await?);
        }
        match queue::restore_state(&pool).await? {
            Some((track, _)) => Ok(repo.get(track.id).await?),
            None => Ok(None),
        }
    }

    pub async fn artwork(&self, row: &TrackRow) -> AppResult<Option<Artwork>> {
        let (_, profile_id) = self.state.require_profile_snapshot().await?;
        let Some((hash, format)) = row.artwork_hash.as_ref().zip(row.artwork_format.as_ref())
        else {
            return Ok(None);
        };
        // Hashes and extensions come from the existing artwork pipeline.
        if !hash.chars().all(|c| c.is_ascii_hexdigit())
            || !format.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Ok(None);
        }
        let path = self
            .state
            .paths
            .profile_artwork_dir(profile_id)
            .join(format!("{hash}.{format}"));
        match tokio::fs::read(path).await {
            Ok(bytes) => tokio::task::spawn_blocking(move || {
                let image = image::load_from_memory(&bytes)
                    .map_err(|e| AppError::Other(e.to_string()))?
                    .thumbnail(128, 128)
                    .into_rgba8();
                Ok(Some(Artwork {
                    width: image.width(),
                    height: image.height(),
                    rgba: image.into_raw(),
                }))
            })
            .await
            .map_err(|e| AppError::Other(e.to_string()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn play(&self, ids: &[i64], index: usize) -> AppResult<()> {
        player_actions::replace_queue_and_play(&self.host, ids, index).await
    }
    pub async fn toggle(&self) -> AppResult<()> {
        match self.engine.shared().state() {
            PlayerState::Playing => self.engine.send(AudioCmd::Pause),
            _ => player_actions::play_and_wait(&self.host).await,
        }
    }
    pub async fn next(&self) {
        player_actions::next(&self.host, "gtk").await;
    }
    pub async fn previous(&self) {
        player_actions::previous(&self.host, "gtk").await;
    }
    pub fn seek(&self, ms: u64) -> AppResult<()> {
        self.engine.send(AudioCmd::Seek(ms))
    }

    pub async fn set_volume(&self, volume: f32) -> AppResult<()> {
        let volume = volume.clamp(0.0, 1.0);
        self.engine.shared().set_volume(volume);
        self.engine.send(AudioCmd::SetVolume(volume))?;
        let pool = self.state.require_profile_pool().await?;
        sqlx::query(
            "INSERT INTO profile_setting(key, value, value_type, updated_at)
             VALUES ('player.volume', ?, 'int', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(((volume * 100.0).round() as i64).to_string())
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(&*pool)
        .await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> AppResult<()> {
        let live = self.playback();
        self.engine
            .shared()
            .paused_output
            .store(true, Ordering::Release);
        self.engine.send(AudioCmd::Shutdown)?;
        if live.track_id > 0 {
            let pool = self.state.require_profile_pool().await?;
            queue::persist_resume_point(&pool, live.track_id, live.position_ms).await?;
        }
        Ok(())
    }
}
