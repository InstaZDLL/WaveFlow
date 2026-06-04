//! Share commands.
//!
//! Two unrelated surfaces share this module name (historical
//! accident — the PNG sink came first, public share links arrived
//! with Phase 1.g.3):
//!
//! - [`save_share_image`] — frontend Canvas renderer → file path.
//!   Used by Wrapped PNG export and Now-Playing card export so the
//!   IPC byte channel + `spawn_blocking` write aren't reimplemented
//!   per feature.
//! - [`share_link_mint`] / [`share_link_revoke`] / [`share_link_status`]
//!   — Phase 1.g.3-desktop wrappers around the waveflow-server
//!   canonical-share surface (PR #27). The React `ShareModal` calls
//!   these without ever knowing about the server's BIGSERIAL ids.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    server_client::{self, WaveflowServerClient},
    state::AppState,
    sync::canonical,
};

/// Persist a frontend-rendered PNG at the chosen path. The bytes flow
/// through the IPC channel as `Vec<u8>` (numeric JSON array on the
/// wire) rather than as a base64 data-URL because IPC strings are
/// UTF-16 in WebView2 and a 1080×1920 PNG roughly doubles in memory
/// after base64 — for clip-bound writes the binary detour is worth it.
///
/// File I/O runs on `spawn_blocking` so a slow disk (USB drive,
/// network share) can't stall the tokio runtime.
#[tauri::command]
pub async fn save_share_image(bytes: Vec<u8>, target_path: String) -> AppResult<()> {
    let target = std::path::PathBuf::from(target_path);
    tokio::task::spawn_blocking(move || std::fs::write(&target, bytes))
        .await
        .map_err(|e| AppError::Other(format!("share image task: {e}")))?
        .map_err(|e| AppError::Other(format!("share image write: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------
// Public share links (Phase 1.g.3-desktop)
// ---------------------------------------------------------------

/// Result of a successful mint. The opaque token + the full
/// shareable URL — the desktop is the natural place to combine
/// token + persisted `app.waveflow_web_url` into a clickable link,
/// since the React modal already mounts here.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareLink {
    /// 32-char opaque token vended by waveflow-server.
    pub token: String,
    /// `<web_origin>/p/<token>`.
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    /// `Some` when the link is currently active (last known state
    /// for this playlist on this device); `None` when no token was
    /// ever minted or a revoke has cleared it.
    pub link: Option<ShareLink>,
}

#[derive(Debug, Deserialize)]
struct MintResponse {
    token: String,
}

/// Mint (or echo back) the public share link for a playlist.
#[tauri::command]
pub async fn share_link_mint(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<ShareLink> {
    let client = require_server_client(&state).await?;
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;

    let (playlist_canonical, profile_canonical) =
        resolve_canonicals(&state, &pool, profile_id, playlist_id).await?;

    let resp = client
        .request(
            reqwest::Method::POST,
            &format!(
                "/api/v1/share/playlists/by-canonical/{}/{}",
                profile_canonical, playlist_canonical
            ),
        )
        .send()
        .await
        .map_err(|e| AppError::Other(format!("share mint request failed: {e}")))?;

    match resp.status() {
        reqwest::StatusCode::OK => {
            let body: MintResponse = resp
                .json()
                .await
                .map_err(|e| AppError::Other(format!("share mint decode failed: {e}")))?;
            let url = build_share_url(&state.app_db, &body.token).await?;
            write_cached_token(&pool, &playlist_canonical, Some(&body.token)).await?;
            Ok(ShareLink {
                token: body.token,
                url,
            })
        }
        reqwest::StatusCode::NOT_FOUND => Err(AppError::Other("playlist not found or not owned by the active profile".into())),
        other => Err(AppError::Other(format!(
            "share mint returned {other} ({})",
            resp.text().await.unwrap_or_default()
        ))),
    }
}

/// Revoke the public share link. Idempotent — calling on an already-
/// private playlist returns `Ok(())`. A foreign / unknown playlist
/// surfaces as `AppError::Other("playlist not found or not owned by the active profile".into())`.
#[tauri::command]
pub async fn share_link_revoke(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<()> {
    let client = require_server_client(&state).await?;
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;

    let (playlist_canonical, profile_canonical) =
        resolve_canonicals(&state, &pool, profile_id, playlist_id).await?;

    let resp = client
        .request(
            reqwest::Method::DELETE,
            &format!(
                "/api/v1/share/playlists/by-canonical/{}/{}",
                profile_canonical, playlist_canonical
            ),
        )
        .send()
        .await
        .map_err(|e| AppError::Other(format!("share revoke request failed: {e}")))?;

    match resp.status() {
        reqwest::StatusCode::NO_CONTENT => {
            write_cached_token(&pool, &playlist_canonical, None).await?;
            Ok(())
        }
        reqwest::StatusCode::NOT_FOUND => Err(AppError::Other("playlist not found or not owned by the active profile".into())),
        other => Err(AppError::Other(format!(
            "share revoke returned {other} ({})",
            resp.text().await.unwrap_or_default()
        ))),
    }
}

/// Local-only status read. Returns whatever `share.token.<canonical>`
/// last cached, with no network round-trip. The modal opens against
/// this for instant feedback; a subsequent mint / revoke updates the
/// cache and refreshes the UI.
#[tauri::command]
pub async fn share_link_status(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<ShareStatus> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    let playlist_canonical =
        canonical::canonical_for_local(&mut conn, canonical::ENTITY_PLAYLIST, playlist_id)
            .await?
            .ok_or(AppError::Other("playlist not found or not owned by the active profile".into()))?;
    drop(conn);

    let token = read_cached_token(&pool, &playlist_canonical).await?;
    let link = match token {
        Some(token) => Some(ShareLink {
            url: build_share_url(&state.app_db, &token).await?,
            token,
        }),
        None => None,
    };
    Ok(ShareStatus { link })
}

// ── Helpers ────────────────────────────────────────────────────

async fn require_server_client(state: &AppState) -> AppResult<WaveflowServerClient> {
    WaveflowServerClient::try_build(state)
        .await?
        .ok_or_else(|| {
            AppError::Other(
                "waveflow-server not configured for this profile (URL or JWT missing)".into(),
            )
        })
}

async fn resolve_canonicals(
    state: &AppState,
    pool: &SqlitePool,
    profile_id: i64,
    playlist_id: i64,
) -> AppResult<(String, String)> {
    let mut conn = pool.acquire().await?;
    let playlist_canonical =
        canonical::canonical_for_local(&mut conn, canonical::ENTITY_PLAYLIST, playlist_id)
            .await?
            .ok_or(AppError::Other("playlist not found or not owned by the active profile".into()))?;
    drop(conn);

    let profile_canonical = crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id)
        .await?
        .ok_or_else(|| {
            AppError::Other(
                "profile.canonical_id not backfilled yet — restart the app to seed it".into(),
            )
        })?;

    Ok((playlist_canonical, profile_canonical))
}

async fn build_share_url(app_db: &SqlitePool, token: &str) -> AppResult<String> {
    let origin = server_client::read_web_url(app_db).await?.ok_or_else(|| {
        AppError::Other(
            "web origin not configured (set app.waveflow_web_url in app_setting)".into(),
        )
    })?;
    let origin = origin.trim_end_matches('/');
    Ok(format!("{origin}/p/{token}"))
}

const CACHE_KEY_PREFIX: &str = "share.token.";

async fn read_cached_token(pool: &SqlitePool, playlist_canonical: &str) -> AppResult<Option<String>> {
    let key = format!("{CACHE_KEY_PREFIX}{playlist_canonical}");
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(&key)
            .fetch_optional(pool)
            .await?;
    Ok(value)
}

async fn write_cached_token(
    pool: &SqlitePool,
    playlist_canonical: &str,
    token: Option<&str>,
) -> AppResult<()> {
    let key = format!("{CACHE_KEY_PREFIX}{playlist_canonical}");
    let now = chrono::Utc::now().timestamp_millis();
    match token {
        Some(token) => {
            sqlx::query(
                "INSERT INTO profile_setting (key, value, value_type, updated_at)
                 VALUES (?, ?, 'string', ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            )
            .bind(&key)
            .bind(token)
            .bind(now)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM profile_setting WHERE key = ?")
                .bind(&key)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
