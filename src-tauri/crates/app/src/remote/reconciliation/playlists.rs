//! Converting a playlist of server tracks into one of their linked local
//! copies.

use std::collections::HashSet;

use sqlx::{Row, SqliteConnection, SqlitePool};
use waveflow_core::repository::{
    playlist::PlaylistDraft,
    sqlite::playlist::{append_tracks_conn, insert_custom_conn},
};

use crate::error::{AppError, AppResult};

use super::{
    now_ms, playlist_link_freshness, PlaylistConversionItem, PlaylistConversionPreview,
    PlaylistConversionResult, PlaylistLinkProof, STATUS_CONFIRMED, STATUS_STALE,
};

/// Preview an explicit playlist conversion without mutating either source.
/// Only confirmed links are convertible; stale, missing and ambiguous pairs
/// remain visible in their original positions and block conversion.
pub async fn preview_playlist_conversion(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionPreview> {
    let mut conn = pool.acquire().await?;
    preview_playlist_conversion_on(&mut conn, direction, source_id).await
}

async fn preview_playlist_conversion_on(
    conn: &mut SqliteConnection,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionPreview> {
    let (source_name, mut items) = match direction {
        "local_to_server" => {
            let playlist_id = source_id
                .parse::<i64>()
                .map_err(|_| AppError::Other("invalid local playlist id".into()))?;
            let row = sqlx::query("SELECT name, is_smart FROM playlist WHERE id = ?")
                .bind(playlist_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| AppError::Other("local playlist not found".into()))?;
            if row.try_get::<i64, _>("is_smart")? != 0 {
                return Err(AppError::Other(
                    "smart playlists must be materialized locally before conversion".into(),
                ));
            }
            let rows = sqlx::query(
                "SELECT pt.position, t.id AS local_track_id, t.title, t.file_path,
                        t.is_available, l.remote_track_id, l.status,
                        l.verified_full_hash, rt.full_hash AS remote_full_hash,
                        rt.remote_id IS NOT NULL AS remote_visible
                   FROM playlist_track pt
                   JOIN track t ON t.id = pt.track_id
                   LEFT JOIN remote_track_link l ON l.local_track_id = t.id
                   LEFT JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                  WHERE pt.playlist_id = ?
                  ORDER BY pt.position, t.id",
            )
            .bind(playlist_id)
            .fetch_all(&mut *conn)
            .await?;
            let mut proofs = Vec::new();
            for row in &rows {
                if row.try_get::<i64, _>("is_available")? != 0 {
                    proofs.push(PlaylistLinkProof {
                        local_track_id: row.try_get("local_track_id")?,
                        file_path: row.try_get("file_path")?,
                        verified_full_hash: row.try_get("verified_full_hash")?,
                        remote_full_hash: row.try_get("remote_full_hash")?,
                    });
                }
            }
            let freshness = playlist_link_freshness(proofs).await?;
            let items = rows
                .into_iter()
                .map(|row| {
                    let local_track_id: i64 = row.try_get("local_track_id")?;
                    let remote_track_id: Option<String> = row.try_get("remote_track_id")?;
                    let link_status: Option<String> = row.try_get("status")?;
                    let local_available = row.try_get::<i64, _>("is_available")? != 0;
                    let remote_visible = row.try_get::<i64, _>("remote_visible")? != 0;
                    let confirmed_context = link_status.as_deref() == Some(STATUS_CONFIRMED)
                        && remote_track_id.is_some()
                        && local_available
                        && remote_visible;
                    let fresh = freshness.get(&local_track_id).copied().unwrap_or(false);
                    let status = if confirmed_context && fresh {
                        STATUS_CONFIRMED
                    } else if link_status.as_deref() == Some(STATUS_STALE)
                        || (confirmed_context && !fresh)
                    {
                        STATUS_STALE
                    } else {
                        "unlinked_or_ambiguous"
                    };
                    Ok(PlaylistConversionItem {
                        position: row.try_get("position")?,
                        title: row.try_get("title")?,
                        local_track_id: Some(local_track_id),
                        remote_track_id,
                        status: status.to_string(),
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            (row.try_get("name")?, items)
        }
        "server_to_local" => {
            let row = sqlx::query("SELECT name FROM remote_playlist WHERE remote_id = ?")
                .bind(source_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| AppError::Other("server playlist not found".into()))?;
            let rows = sqlx::query(
                "SELECT rpt.position, rpt.track_remote_id, rt.title,
                        l.local_track_id, l.status, l.verified_full_hash,
                        rt.full_hash AS remote_full_hash, t.file_path,
                        rt.remote_id IS NOT NULL AS remote_visible,
                        t.id IS NOT NULL AND t.is_available = 1 AS local_visible
                   FROM remote_playlist_track rpt
                   LEFT JOIN remote_track rt ON rt.remote_id = rpt.track_remote_id
                   LEFT JOIN remote_track_link l ON l.remote_track_id = rpt.track_remote_id
                   LEFT JOIN track t ON t.id = l.local_track_id
                  WHERE rpt.playlist_remote_id = ?
                  ORDER BY rpt.position",
            )
            .bind(source_id)
            .fetch_all(&mut *conn)
            .await?;
            let mut proofs = Vec::new();
            for row in &rows {
                if row.try_get::<i64, _>("local_visible")? != 0 {
                    proofs.push(PlaylistLinkProof {
                        local_track_id: row.try_get("local_track_id")?,
                        file_path: row.try_get("file_path")?,
                        verified_full_hash: row.try_get("verified_full_hash")?,
                        remote_full_hash: row.try_get("remote_full_hash")?,
                    });
                }
            }
            let freshness = playlist_link_freshness(proofs).await?;
            let mut seen_local = HashSet::new();
            let items = rows
                .into_iter()
                .map(|row| {
                    let local_track_id: Option<i64> = row.try_get("local_track_id")?;
                    let link_status: Option<String> = row.try_get("status")?;
                    let remote_visible = row.try_get::<i64, _>("remote_visible")? != 0;
                    let local_visible = row.try_get::<i64, _>("local_visible")? != 0;
                    let fresh = local_track_id
                        .and_then(|id| freshness.get(&id).copied())
                        .unwrap_or(false);
                    let confirmed_context = link_status.as_deref() == Some(STATUS_CONFIRMED)
                        && remote_visible
                        && local_visible;
                    let status = if let Some(local_track_id) =
                        local_track_id.filter(|_| confirmed_context && fresh)
                    {
                        if seen_local.insert(local_track_id) {
                            STATUS_CONFIRMED
                        } else {
                            "duplicate"
                        }
                    } else if link_status.as_deref() == Some(STATUS_STALE)
                        || (confirmed_context && !fresh)
                    {
                        STATUS_STALE
                    } else {
                        "unlinked_or_ambiguous"
                    };
                    let remote_track_id: String = row.try_get("track_remote_id")?;
                    let title = row
                        .try_get::<Option<String>, _>("title")?
                        .unwrap_or_else(|| remote_track_id.clone());
                    Ok(PlaylistConversionItem {
                        position: row.try_get("position")?,
                        title,
                        local_track_id,
                        remote_track_id: Some(remote_track_id),
                        status: status.to_string(),
                    })
                })
                .collect::<AppResult<Vec<_>>>()?;
            (row.try_get("name")?, items)
        }
        _ => {
            return Err(AppError::Other(
                "invalid playlist conversion direction".into(),
            ))
        }
    };

    let total_tracks = items.len();
    let convertible_tracks = items
        .iter()
        .filter(|item| item.status == STATUS_CONFIRMED)
        .count();
    let blocked_tracks = total_tracks.saturating_sub(convertible_tracks);
    // Keep the original order in the response even if future status
    // enrichment appends rows from another source.
    items.sort_by_key(|item| item.position);
    Ok(PlaylistConversionPreview {
        direction: direction.to_string(),
        source_id: source_id.to_string(),
        source_name,
        total_tracks,
        convertible_tracks,
        blocked_tracks,
        can_convert: blocked_tracks == 0,
        items,
    })
}

/// Execute a previously previewable playlist conversion. The preview is
/// rebuilt inside the same transaction, preventing a link becoming stale or
/// disappearing between confirmation and mutation.
pub async fn convert_playlist(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
) -> AppResult<PlaylistConversionResult> {
    convert_playlist_with_post_validation(pool, direction, source_id, || {}).await
}

pub(super) async fn convert_playlist_with_post_validation<F>(
    pool: &SqlitePool,
    direction: &str,
    source_id: &str,
    post_validation: F,
) -> AppResult<PlaylistConversionResult>
where
    F: FnOnce(),
{
    let mut tx = pool.begin().await?;
    let preview = preview_playlist_conversion_on(&mut tx, direction, source_id).await?;
    if !preview.can_convert {
        return Err(AppError::Other(format!(
            "playlist conversion blocked by {} unlinked, stale, ambiguous, or duplicate tracks",
            preview.blocked_tracks
        )));
    }
    post_validation();

    let destination_id = match direction {
        "local_to_server" => {
            let track_ids = preview
                .items
                .iter()
                .filter_map(|item| item.remote_track_id.clone())
                .collect::<Vec<_>>();
            crate::remote::write::create_playlist_in_tx(&mut tx, &preview.source_name, &track_ids)
                .await?
        }
        "server_to_local" => {
            let now = now_ms();
            let draft = PlaylistDraft {
                name: preview.source_name.clone(),
                description: Some("Materialized from WaveFlow Server".into()),
                color_id: "violet".into(),
                color_mode: "auto".into(),
                icon_id: "music".into(),
                now_ms: now,
            };
            let playlist_id = insert_custom_conn(&mut tx, &draft).await?;
            let track_ids = preview
                .items
                .iter()
                .filter_map(|item| item.local_track_id)
                .collect::<Vec<_>>();
            append_tracks_conn(&mut tx, playlist_id, &track_ids, now).await?;
            playlist_id.to_string()
        }
        _ => unreachable!("direction validated by preview"),
    };
    let final_preview = preview_playlist_conversion_on(&mut tx, direction, source_id).await?;
    if !final_preview.can_convert {
        return Err(AppError::Other(format!(
            "playlist conversion blocked by {} tracks that changed during conversion",
            final_preview.blocked_tracks
        )));
    }
    tx.commit().await?;
    Ok(PlaylistConversionResult {
        direction: direction.to_string(),
        destination_id,
        converted_tracks: preview.total_tracks,
    })
}
