//! Conservative local/server reconciliation (server RFC-004, M5).
//!
//! The server publishes a plain full-file BLAKE3 digest. The local
//! `track.file_hash` is deliberately a different, partial scan digest, so this
//! module first joins on byte size and only then reads the few possible local
//! matches in full. A link is automatic only when the exact digest identifies
//! one local row and one remote row. Every multiplicity stays a candidate for
//! a human decision.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use sqlx::{Row, SqlitePool};

use crate::error::{AppError, AppResult};

const STATUS_CONFIRMED: &str = "confirmed";
const STATUS_STALE: &str = "stale";

#[derive(Debug, Clone)]
struct LocalTrack {
    id: i64,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    file_path: String,
    size: i64,
}

#[derive(Debug, Clone)]
struct HashedLocalTrack {
    track: LocalTrack,
    full_hash: String,
}

#[derive(Debug, Clone)]
struct RemoteTrack {
    id: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    size: i64,
    full_hash: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalMatchCandidate {
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub file_path: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteMatchCandidate {
    pub track_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MatchCandidateGroup {
    pub full_hash: String,
    pub local_tracks: Vec<LocalMatchCandidate>,
    pub remote_tracks: Vec<RemoteMatchCandidate>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub hashed_local_tracks: usize,
    pub unreadable_local_tracks: usize,
    pub auto_linked: usize,
    pub verified_links: usize,
    pub stale_links: usize,
    pub rejected_pairs: usize,
    pub candidates: Vec<MatchCandidateGroup>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReconciliationLink {
    pub local_track_id: i64,
    pub remote_track_id: String,
    pub local_title: String,
    pub remote_title: Option<String>,
    pub method: String,
    pub verified_full_hash: Option<String>,
    pub status: String,
    pub playback_preference: String,
    pub confirmed_at: i64,
    pub verified_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferredLocalPlayback {
    pub track_id: i64,
    pub path: PathBuf,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
struct ExistingLink {
    local_track_id: i64,
    remote_track_id: String,
    verified_full_hash: Option<String>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn valid_full_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Discover exact matches, persist only the unique ones and return every
/// ambiguous group. Files that cannot be read are reported but never linked.
pub async fn discover(pool: &SqlitePool) -> AppResult<ReconciliationReport> {
    let remote_tracks = load_remote_tracks(pool).await?;
    if remote_tracks.is_empty() {
        return Ok(ReconciliationReport {
            hashed_local_tracks: 0,
            unreadable_local_tracks: 0,
            auto_linked: 0,
            verified_links: 0,
            stale_links: 0,
            rejected_pairs: 0,
            candidates: Vec::new(),
        });
    }

    let local_tracks = load_local_candidates(pool).await?;
    let (hashed_local_tracks, unreadable_local_tracks) =
        tokio::task::spawn_blocking(move || hash_local_tracks(local_tracks))
            .await
            .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))?;

    reconcile(
        pool,
        hashed_local_tracks,
        unreadable_local_tracks,
        remote_tracks,
    )
    .await
}

async fn load_remote_tracks(pool: &SqlitePool) -> AppResult<Vec<RemoteTrack>> {
    let rows = sqlx::query(
        "SELECT remote_id, title, artist, album, size, full_hash
           FROM remote_track
          WHERE size IS NOT NULL AND size >= 0 AND full_hash IS NOT NULL
          ORDER BY remote_id",
    )
    .fetch_all(pool)
    .await?;

    let mut tracks = Vec::with_capacity(rows.len());
    for row in rows {
        let full_hash: String = row.try_get("full_hash")?;
        if !valid_full_hash(&full_hash) {
            continue;
        }
        tracks.push(RemoteTrack {
            id: row.try_get("remote_id")?,
            title: row.try_get("title")?,
            artist: row.try_get("artist")?,
            album: row.try_get("album")?,
            size: row.try_get("size")?,
            full_hash: full_hash.to_ascii_lowercase(),
        });
    }
    Ok(tracks)
}

async fn load_local_candidates(pool: &SqlitePool) -> AppResult<Vec<LocalTrack>> {
    let rows = sqlx::query(
        "SELECT t.id, t.title, t.file_path, t.file_size, al.title AS album,
                (SELECT GROUP_CONCAT(name, ', ') FROM (
                    SELECT ar.name
                      FROM track_artist ta
                      JOIN artist ar ON ar.id = ta.artist_id
                     WHERE ta.track_id = t.id
                     ORDER BY ta.position
                )) AS artist
           FROM track t
           LEFT JOIN album al ON al.id = t.album_id
          WHERE t.is_available = 1
            AND (EXISTS (SELECT 1 FROM remote_track rt
                          WHERE rt.size = t.file_size AND rt.size >= 0
                            AND rt.full_hash IS NOT NULL)
                 OR EXISTS (SELECT 1 FROM remote_track_link l
                             WHERE l.local_track_id = t.id))
          ORDER BY t.id",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(LocalTrack {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                artist: row.try_get("artist")?,
                album: row.try_get("album")?,
                file_path: row.try_get("file_path")?,
                size: row.try_get("file_size")?,
            })
        })
        .collect()
}

fn hash_local_tracks(tracks: Vec<LocalTrack>) -> (Vec<HashedLocalTrack>, usize) {
    let mut hashed = Vec::with_capacity(tracks.len());
    let mut unreadable = 0;
    for track in tracks {
        match waveflow_core::scanner::hash_file_full(Path::new(&track.file_path)) {
            Ok(full_hash) => hashed.push(HashedLocalTrack { track, full_hash }),
            Err(err) => {
                unreadable += 1;
                tracing::warn!(
                    local_track_id = track.id,
                    error = %err,
                    "reconciliation full-content hash failed; excluding track"
                );
            }
        }
    }
    (hashed, unreadable)
}

async fn reconcile(
    pool: &SqlitePool,
    local_tracks: Vec<HashedLocalTrack>,
    unreadable_local_tracks: usize,
    remote_tracks: Vec<RemoteTrack>,
) -> AppResult<ReconciliationReport> {
    let hashed_local_count = local_tracks.len();
    let mut local_by_hash: HashMap<String, Vec<HashedLocalTrack>> = HashMap::new();
    let mut local_by_id = HashMap::new();
    for local in local_tracks {
        local_by_id.insert(local.track.id, local.clone());
        local_by_hash
            .entry(local.full_hash.clone())
            .or_default()
            .push(local);
    }

    let mut remote_by_hash: HashMap<String, Vec<RemoteTrack>> = HashMap::new();
    let mut remote_by_id = HashMap::new();
    for remote in remote_tracks {
        remote_by_id.insert(remote.id.clone(), remote.clone());
        remote_by_hash
            .entry(remote.full_hash.clone())
            .or_default()
            .push(remote);
    }

    let mut tx = pool.begin().await?;
    let link_rows = sqlx::query(
        "SELECT local_track_id, remote_track_id, verified_full_hash
           FROM remote_track_link",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut links = Vec::with_capacity(link_rows.len());
    for row in link_rows {
        links.push(ExistingLink {
            local_track_id: row.try_get("local_track_id")?,
            remote_track_id: row.try_get("remote_track_id")?,
            verified_full_hash: row.try_get("verified_full_hash")?,
        });
    }

    let rejection_rows = sqlx::query(
        "SELECT local_track_id, remote_track_id, proof
           FROM remote_track_match_rejection
          WHERE proof_kind = 'exact_full_hash'",
    )
    .fetch_all(&mut *tx)
    .await?;
    let rejections: HashSet<(i64, String, String)> = rejection_rows
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get("local_track_id")?,
                row.try_get("remote_track_id")?,
                row.try_get("proof")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?;

    let now = now_ms();
    let mut verified_links = 0;
    let mut stale_links = 0;
    let mut linked_local: HashMap<i64, String> = HashMap::new();
    let mut linked_remote: HashMap<String, i64> = HashMap::new();

    for link in &links {
        linked_local.insert(link.local_track_id, link.remote_track_id.clone());
        linked_remote.insert(link.remote_track_id.clone(), link.local_track_id);

        let Some(local) = local_by_id.get(&link.local_track_id) else {
            // An unavailable local file keeps its link; availability is not
            // evidence that the identity changed.
            continue;
        };
        let Some(remote) = remote_by_id.get(&link.remote_track_id) else {
            // The remote cache is disposable and may be between refills.
            continue;
        };
        let matches = link.verified_full_hash.as_deref() == Some(local.full_hash.as_str())
            && local.full_hash == remote.full_hash;
        let status = if matches {
            verified_links += 1;
            STATUS_CONFIRMED
        } else {
            stale_links += 1;
            STATUS_STALE
        };
        sqlx::query(
            "UPDATE remote_track_link SET status = ?, verified_at = ?
              WHERE local_track_id = ?",
        )
        .bind(status)
        .bind(now)
        .bind(link.local_track_id)
        .execute(&mut *tx)
        .await?;
    }

    let mut auto_linked = 0;
    for (hash, locals) in &local_by_hash {
        let Some(remotes) = remote_by_hash.get(hash) else {
            continue;
        };
        if locals.len() != 1 || remotes.len() != 1 {
            continue;
        }
        let local = &locals[0];
        let remote = &remotes[0];
        if linked_local.contains_key(&local.track.id)
            || linked_remote.contains_key(&remote.id)
            || rejections.contains(&(local.track.id, remote.id.clone(), hash.clone()))
        {
            continue;
        }

        sqlx::query(
            "INSERT INTO remote_track_link
                (local_track_id, remote_track_id, method, verified_full_hash,
                 status, playback_preference, confirmed_at, verified_at)
             VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', ?, ?)",
        )
        .bind(local.track.id)
        .bind(&remote.id)
        .bind(hash)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        linked_local.insert(local.track.id, remote.id.clone());
        linked_remote.insert(remote.id.clone(), local.track.id);
        auto_linked += 1;
    }

    let mut hashes: Vec<String> = local_by_hash
        .keys()
        .filter(|hash| remote_by_hash.contains_key(*hash))
        .cloned()
        .collect();
    hashes.sort();

    let mut candidates = Vec::new();
    let mut rejected_pairs = 0;
    for hash in hashes {
        let locals = &local_by_hash[&hash];
        let remotes = &remote_by_hash[&hash];
        let mut candidate_locals = Vec::new();
        let mut candidate_remotes = Vec::new();
        let mut has_unrejected_pair = false;

        for local in locals {
            if linked_local.contains_key(&local.track.id) {
                continue;
            }
            candidate_locals.push(local_candidate(local));
            for remote in remotes {
                if linked_remote.contains_key(&remote.id) {
                    continue;
                }
                if rejections.contains(&(local.track.id, remote.id.clone(), hash.clone())) {
                    rejected_pairs += 1;
                } else {
                    has_unrejected_pair = true;
                }
            }
        }
        for remote in remotes {
            if !linked_remote.contains_key(&remote.id) {
                candidate_remotes.push(remote_candidate(remote));
            }
        }

        if has_unrejected_pair && !candidate_locals.is_empty() && !candidate_remotes.is_empty() {
            candidates.push(MatchCandidateGroup {
                full_hash: hash,
                local_tracks: candidate_locals,
                remote_tracks: candidate_remotes,
            });
        }
    }

    tx.commit().await?;
    Ok(ReconciliationReport {
        hashed_local_tracks: hashed_local_count,
        unreadable_local_tracks,
        auto_linked,
        verified_links,
        stale_links,
        rejected_pairs,
        candidates,
    })
}

fn local_candidate(local: &HashedLocalTrack) -> LocalMatchCandidate {
    LocalMatchCandidate {
        track_id: local.track.id,
        title: local.track.title.clone(),
        artist: local.track.artist.clone(),
        album: local.track.album.clone(),
        file_path: local.track.file_path.clone(),
        size: local.track.size,
    }
}

fn remote_candidate(remote: &RemoteTrack) -> RemoteMatchCandidate {
    RemoteMatchCandidate {
        track_id: remote.id.clone(),
        title: remote.title.clone(),
        artist: remote.artist.clone(),
        album: remote.album.clone(),
        size: remote.size,
    }
}

/// Confirm one exact pair from an ambiguous group. The local file is re-read
/// immediately before the transaction so a stale UI cannot confirm old bytes.
pub async fn confirm_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT t.file_path, t.file_size, rt.size AS remote_size, rt.full_hash
           FROM track t
           JOIN remote_track rt ON rt.remote_id = ?
          WHERE t.id = ? AND t.is_available = 1",
    )
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("local or remote track is unavailable".into()))?;

    let file_path: String = row.try_get("file_path")?;
    let local_size: i64 = row.try_get("file_size")?;
    let remote_size: i64 = row.try_get("remote_size")?;
    let remote_hash: String = row.try_get("full_hash")?;
    if local_size != remote_size || !valid_full_hash(&remote_hash) {
        return Err(AppError::Other(
            "tracks do not share a valid exact-content candidate".into(),
        ));
    }

    let local_hash = tokio::task::spawn_blocking(move || {
        waveflow_core::scanner::hash_file_full(Path::new(&file_path))
    })
    .await
    .map_err(|err| AppError::Other(format!("reconciliation hash task failed: {err}")))??;
    if !local_hash.eq_ignore_ascii_case(&remote_hash) {
        return Err(AppError::Other("track contents no longer match".into()));
    }

    let normalized_hash = remote_hash.to_ascii_lowercase();
    let now = now_ms();
    let mut tx = pool.begin().await?;
    let conflict: Option<(i64, String)> = sqlx::query_as(
        "SELECT local_track_id, remote_track_id FROM remote_track_link
          WHERE (local_track_id = ? AND remote_track_id != ?)
             OR (remote_track_id = ? AND local_track_id != ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?;
    if conflict.is_some() {
        return Err(AppError::Other(
            "one of these tracks is already linked elsewhere".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO remote_track_link
            (local_track_id, remote_track_id, method, verified_full_hash,
             status, playback_preference, confirmed_at, verified_at)
         VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', ?, ?)
         ON CONFLICT(local_track_id) DO UPDATE SET
             remote_track_id = excluded.remote_track_id,
             method = excluded.method,
             verified_full_hash = excluded.verified_full_hash,
             status = excluded.status,
             verified_at = excluded.verified_at",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM remote_track_match_rejection
          WHERE local_track_id = ? AND remote_track_id = ?
            AND proof_kind = 'exact_full_hash' AND proof = ?",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Hide an exact candidate until its proof changes.
pub async fn reject_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT rt.full_hash,
                EXISTS(SELECT 1 FROM track t WHERE t.id = ?) AS local_exists,
                EXISTS(SELECT 1 FROM remote_track_link l
                        WHERE l.local_track_id = ? OR l.remote_track_id = ?) AS linked
           FROM remote_track rt WHERE rt.remote_id = ?",
    )
    .bind(local_track_id)
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("remote track is unavailable".into()))?;
    let local_exists = row.try_get::<i64, _>("local_exists")? != 0;
    let linked = row.try_get::<i64, _>("linked")? != 0;
    let proof: String = row.try_get("full_hash")?;
    if !local_exists || !valid_full_hash(&proof) {
        return Err(AppError::Other("candidate is unavailable".into()));
    }
    if linked {
        return Err(AppError::Other("linked tracks cannot be rejected".into()));
    }

    sqlx::query(
        "INSERT OR IGNORE INTO remote_track_match_rejection
            (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
         VALUES (?, ?, 'exact_full_hash', ?, ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(proof.to_ascii_lowercase())
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn links(pool: &SqlitePool) -> AppResult<Vec<ReconciliationLink>> {
    let rows = sqlx::query(
        "SELECT l.local_track_id, l.remote_track_id, t.title AS local_title,
                rt.title AS remote_title, l.method, l.verified_full_hash,
                l.status, l.playback_preference, l.confirmed_at, l.verified_at
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
           LEFT JOIN remote_track rt ON rt.remote_id = l.remote_track_id
          ORDER BY lower(t.title), l.local_track_id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ReconciliationLink {
                local_track_id: row.try_get("local_track_id")?,
                remote_track_id: row.try_get("remote_track_id")?,
                local_title: row.try_get("local_title")?,
                remote_title: row.try_get("remote_title")?,
                method: row.try_get("method")?,
                verified_full_hash: row.try_get("verified_full_hash")?,
                status: row.try_get("status")?,
                playback_preference: row.try_get("playback_preference")?,
                confirmed_at: row.try_get("confirmed_at")?,
                verified_at: row.try_get("verified_at")?,
            })
        })
        .collect()
}

pub async fn set_playback_preference(
    pool: &SqlitePool,
    local_track_id: i64,
    preference: &str,
) -> AppResult<()> {
    if !matches!(preference, "local_first" | "server_first") {
        return Err(AppError::Other("invalid playback preference".into()));
    }
    let result = sqlx::query(
        "UPDATE remote_track_link SET playback_preference = ?
          WHERE local_track_id = ?",
    )
    .bind(preference)
    .bind(local_track_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::Other("reconciliation link not found".into()));
    }
    Ok(())
}

/// Resolve a remote track to the confirmed, available local file selected by
/// the user's playback preference. A missing file is treated like no local
/// candidate so callers can immediately fall back to the server.
pub async fn preferred_local_playback(
    pool: &SqlitePool,
    remote_track_id: &str,
) -> AppResult<Option<PreferredLocalPlayback>> {
    let row = sqlx::query(
        "SELECT t.id, t.file_path, t.duration_ms
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
          WHERE l.remote_track_id = ?
            AND l.status = 'confirmed'
            AND l.playback_preference = 'local_first'
            AND t.is_available = 1
          LIMIT 1",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let path = PathBuf::from(row.try_get::<String, _>("file_path")?);
    if !path.is_file() {
        return Ok(None);
    }
    let duration_ms = row.try_get::<i64, _>("duration_ms")?.max(0) as u64;
    Ok(Some(PreferredLocalPlayback {
        track_id: row.try_get("id")?,
        path,
        duration_ms,
    }))
}

pub async fn remove_link(pool: &SqlitePool, local_track_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    // Capture the link's matching evidence before deleting it and record a
    // rejection for the same pair — otherwise the next `discover` would just
    // auto-recreate the exact link the user manually removed. Same proof
    // representation as `reject_exact`, so `reconcile` honours it.
    if let Some(row) = sqlx::query(
        "SELECT remote_track_id, method, verified_full_hash
           FROM remote_track_link WHERE local_track_id = ?",
    )
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let remote_track_id: String = row.try_get("remote_track_id")?;
        let method: String = row.try_get("method")?;
        let verified_full_hash: Option<String> = row.try_get("verified_full_hash")?;
        // Only an exact-hash link carries a full-hash proof to suppress.
        if method == "exact_full_hash" {
            if let Some(proof) = verified_full_hash.filter(|value| valid_full_hash(value)) {
                sqlx::query(
                    "INSERT OR IGNORE INTO remote_track_match_rejection
                        (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
                     VALUES (?, ?, 'exact_full_hash', ?, ?)",
                )
                .bind(local_track_id)
                .bind(&remote_track_id)
                .bind(proof.to_ascii_lowercase())
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    sqlx::query("DELETE FROM remote_track_link WHERE local_track_id = ?")
        .bind(local_track_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::fs;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::raw_sql(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE album (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
             CREATE TABLE artist (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER REFERENCES album(id),
                 file_path TEXT NOT NULL,
                 file_size INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 duration_ms INTEGER NOT NULL DEFAULT 0,
                 is_available INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE track_artist (
                 track_id INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
                 artist_id INTEGER NOT NULL REFERENCES artist(id),
                 position INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE remote_track (
                 remote_id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 artist TEXT,
                 album TEXT,
                 size INTEGER,
                 full_hash TEXT
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../../migrations/profile/20260817100000_remote_track_reconciliation.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_local(pool: &SqlitePool, id: i64, path: &Path, title: &str) {
        sqlx::query(
            "INSERT INTO track (id, file_path, file_size, title, is_available)
             VALUES (?, ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(path.to_string_lossy().as_ref())
        .bind(fs::metadata(path).unwrap().len() as i64)
        .bind(title)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_remote(pool: &SqlitePool, id: &str, bytes: &[u8], title: &str) {
        let hash = blake3::hash(bytes).to_hex().to_string();
        sqlx::query(
            "INSERT INTO remote_track (remote_id, title, size, full_hash)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(title)
        .bind(bytes.len() as i64)
        .bind(hash)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unique_exact_content_is_linked_automatically() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.flac");
        fs::write(&path, b"identical audio bytes").unwrap();
        insert_local(&pool, 1, &path, "Local").await;
        insert_remote(&pool, "remote-1", b"identical audio bytes", "Remote").await;

        let report = discover(&pool).await.unwrap();
        assert_eq!(report.auto_linked, 1);
        assert!(report.candidates.is_empty());
        let links = links(&pool).await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].local_track_id, 1);
        assert_eq!(links[0].remote_track_id, "remote-1");
        assert_eq!(links[0].playback_preference, "local_first");
    }

    #[tokio::test]
    async fn equal_size_with_different_content_never_links() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.mp3");
        fs::write(&path, b"aaaa").unwrap();
        insert_local(&pool, 1, &path, "Local").await;
        insert_remote(&pool, "remote-1", b"bbbb", "Remote").await;

        let report = discover(&pool).await.unwrap();
        assert_eq!(report.hashed_local_tracks, 1);
        assert_eq!(report.auto_linked, 0);
        assert!(report.candidates.is_empty());
        assert!(links(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn duplicate_content_requires_confirmation() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();
        insert_local(&pool, 1, &first, "First").await;
        insert_local(&pool, 2, &second, "Second").await;
        insert_remote(&pool, "remote-1", b"same", "Remote").await;

        let report = discover(&pool).await.unwrap();
        assert_eq!(report.auto_linked, 0);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].local_tracks.len(), 2);

        confirm_exact(&pool, 2, "remote-1").await.unwrap();
        let links = links(&pool).await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].local_track_id, 2);
    }

    #[tokio::test]
    async fn duplicate_remote_copies_never_auto_link() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.flac");
        fs::write(&path, b"same").unwrap();
        insert_local(&pool, 1, &path, "Local").await;
        insert_remote(&pool, "remote-1", b"same", "First remote").await;
        insert_remote(&pool, "remote-2", b"same", "Second remote").await;

        let report = discover(&pool).await.unwrap();
        assert_eq!(report.auto_linked, 0);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].remote_tracks.len(), 2);
        assert!(links(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejection_hides_the_same_proof() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.aac");
        let second = dir.path().join("second.aac");
        fs::write(&first, b"same").unwrap();
        fs::write(&second, b"same").unwrap();
        insert_local(&pool, 1, &first, "First").await;
        insert_local(&pool, 2, &second, "Second").await;
        insert_remote(&pool, "remote-1", b"same", "Remote").await;

        reject_exact(&pool, 1, "remote-1").await.unwrap();
        reject_exact(&pool, 2, "remote-1").await.unwrap();
        let report = discover(&pool).await.unwrap();
        assert_eq!(report.rejected_pairs, 2);
        assert!(report.candidates.is_empty());
        assert!(links(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn changed_bytes_mark_a_confirmed_link_stale() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.ogg");
        fs::write(&path, b"aaaa").unwrap();
        insert_local(&pool, 1, &path, "Local").await;
        insert_remote(&pool, "remote-1", b"aaaa", "Remote").await;
        assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

        fs::write(&path, b"bbbb").unwrap();
        let report = discover(&pool).await.unwrap();
        assert_eq!(report.stale_links, 1);
        assert_eq!(links(&pool).await.unwrap()[0].status, "stale");
    }

    #[tokio::test]
    async fn a_path_move_keeps_and_reverifies_the_link() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("before.flac");
        let moved = dir.path().join("after.flac");
        fs::write(&first, b"same bytes").unwrap();
        insert_local(&pool, 1, &first, "Local").await;
        insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;
        assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

        fs::rename(&first, &moved).unwrap();
        sqlx::query("UPDATE track SET file_path = ? WHERE id = 1")
            .bind(moved.to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();
        let report = discover(&pool).await.unwrap();
        assert_eq!(report.verified_links, 1);
        assert_eq!(links(&pool).await.unwrap()[0].status, "confirmed");
    }

    #[tokio::test]
    async fn playback_uses_only_confirmed_available_local_first_links() {
        let pool = pool().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.flac");
        fs::write(&path, b"same bytes").unwrap();
        insert_local(&pool, 1, &path, "Local").await;
        sqlx::query("UPDATE track SET duration_ms = 12_345 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        insert_remote(&pool, "remote-1", b"same bytes", "Remote").await;
        assert_eq!(discover(&pool).await.unwrap().auto_linked, 1);

        let selected = preferred_local_playback(&pool, "remote-1")
            .await
            .unwrap()
            .expect("confirmed local-first link selected");
        assert_eq!(selected.track_id, 1);
        assert_eq!(selected.path, path);
        assert_eq!(selected.duration_ms, 12_345);

        set_playback_preference(&pool, 1, "server_first")
            .await
            .unwrap();
        assert!(preferred_local_playback(&pool, "remote-1")
            .await
            .unwrap()
            .is_none());

        set_playback_preference(&pool, 1, "local_first")
            .await
            .unwrap();
        sqlx::query("UPDATE track SET is_available = 0 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(preferred_local_playback(&pool, "remote-1")
            .await
            .unwrap()
            .is_none());

        sqlx::query("UPDATE track SET is_available = 1 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        fs::remove_file(&path).unwrap();
        assert!(preferred_local_playback(&pool, "remote-1")
            .await
            .unwrap()
            .is_none());

        fs::write(&path, b"same bytes").unwrap();
        sqlx::query("UPDATE remote_track_link SET status = 'stale' WHERE local_track_id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(preferred_local_playback(&pool, "remote-1")
            .await
            .unwrap()
            .is_none());
    }
}
