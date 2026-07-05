//! HTTP client for `GET /api/v1/sync/entity` (RFC-003 Phase B.2,
//! server PR #66).
//!
//! Sibling of [`super::client`] (`/sync/digest`): where digest hands
//! back a hashed snapshot of an entity set, this one fetches the
//! FULL canonical-fields state of a single materialised row keyed
//! on its canonical id. The backfill orchestrator hits it to
//! resolve `missing_locally` (server has, desktop doesn't) and
//! `divergent` (same canonical, different hash → fetch remote,
//! compare §2 tuples, apply the LWW winner).
//!
//! ## Scope discipline (mirrors digest)
//!
//! - Profile-scoped (`library` / `playlist` / `track`) require
//!   `profile_canonical_id`. Server returns 400 without it.
//! - User-scoped (`liked_track` / `track_rating`) reject it.
//! - The `profile` entity is intentionally NOT supported by the
//!   server endpoint (the canonical_id used to address it IS the
//!   per-tenant scope identifier). We mirror that exclusion here
//!   so a typo at the call site fails locally.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::server_client::WaveflowServerClient;

/// Single-row state returned by `GET /api/v1/sync/entity`. Mirror
/// of the server's `waveflow_server::sync::EntityFetchResponse`.
/// `fields` is whatever `apply::*::canonical_fields` fed to
/// `compute_payload_hash` when the row was stamped — recomputing
/// against `(fields, hlc, origin_device_id)` MUST land on
/// `payload_hash`, which the backfill orchestrator can assert
/// defensively.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteEntityRow {
    pub entity: String,
    pub canonical_id: String,
    /// Hex-encoded BLAKE3-256.
    pub payload_hash: String,
    pub hlc: RemoteEntityHlc,
    #[serde(default)]
    pub origin_device_id: Option<Uuid>,
    pub fields: Map<String, Value>,
    /// Track-only — the desktop maps to its local `library.id`
    /// via `canonical::ENTITY_LIBRARY`. `None` for non-track
    /// entities (server omits the field via `skip_serializing_if`).
    #[serde(default)]
    pub library_canonical_id: Option<String>,
    /// Track-only — the second half of the composite canonical
    /// (`<lib_canonical>\u{1F}<file_path>`). `None` for non-track.
    #[serde(default)]
    pub file_path: Option<String>,
}

/// HLC pair on the wire. Matches the server's `Hlc` shape; kept
/// local to avoid pulling utoipa schema types into the desktop.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteEntityHlc {
    pub wall: i64,
    pub logical: i32,
}

/// Fetch a single materialised row for `(entity, canonical_id)`.
/// `Ok(None)` covers the server-side 404 path (row absent or
/// `payload_hash IS NULL`); other failures (auth, scope, network)
/// surface as `Err`.
///
/// Per the server's scope rules:
/// - `library` / `playlist` / `track`: `profile_canonical_id`
///   required (the active profile's UUID from `app.db`).
/// - `liked_track` / `track_rating`: `profile_canonical_id` MUST
///   be `None`; the server rejects it otherwise.
pub async fn fetch_remote_entity(
    client: &WaveflowServerClient,
    entity: &str,
    canonical_id: &str,
    profile_canonical_id: Option<&str>,
) -> AppResult<Option<RemoteEntityRow>> {
    validate_scope(entity, profile_canonical_id)?;

    let mut query: Vec<(&str, &str)> = vec![("entity", entity), ("canonical_id", canonical_id)];
    if let Some(canon) = profile_canonical_id {
        query.push(("profile_canonical_id", canon));
    }

    let response = client
        .request(reqwest::Method::GET, "/api/v1/sync/entity")
        .query(&query)
        .send()
        .await
        .map_err(|err| AppError::Other(format!("sync entity GET: {err}")))?;

    let status = response.status();
    match status {
        StatusCode::OK => {
            let row: RemoteEntityRow = response
                .json()
                .await
                .map_err(|err| AppError::Other(format!("sync entity deserialise: {err}")))?;
            Ok(Some(row))
        }
        StatusCode::NOT_FOUND => Ok(None),
        StatusCode::UNAUTHORIZED => Err(AppError::Other(
            "sync entity GET: unauthorized — JWT expired or revoked".into(),
        )),
        StatusCode::BAD_REQUEST => {
            let body = response.text().await.unwrap_or_else(|_| "<no body>".into());
            Err(AppError::Other(format!(
                "sync entity GET {entity}: bad request — {body}",
            )))
        }
        other => {
            let body = response.text().await.unwrap_or_else(|_| "<no body>".into());
            Err(AppError::Other(format!(
                "sync entity GET {entity}: unexpected status {other} — {body}",
            )))
        }
    }
}

fn validate_scope(entity: &str, profile_canonical_id: Option<&str>) -> AppResult<()> {
    match entity {
        "library" | "playlist" | "track" => {
            if profile_canonical_id.is_none() {
                return Err(AppError::Other(format!(
                    "sync entity {entity}: profile_canonical_id is required",
                )));
            }
        }
        "liked_track" | "track_rating" => {
            if profile_canonical_id.is_some() {
                return Err(AppError::Other(format!(
                    "sync entity {entity}: profile_canonical_id must be omitted",
                )));
            }
        }
        // `profile` deliberately rejected — server PR #66 excludes
        // it by design (canonical IS the scope identifier).
        other => {
            return Err(AppError::Other(format!(
                "sync entity: unknown / unsupported entity '{other}'",
            )))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_requires_canonical_for_profile_entities() {
        for e in ["library", "playlist", "track"] {
            assert!(validate_scope(e, None).is_err(), "{e} without canonical");
            assert!(validate_scope(e, Some("abc")).is_ok(), "{e} with canonical");
        }
    }

    #[test]
    fn scope_rejects_canonical_for_user_entities() {
        for e in ["liked_track", "track_rating"] {
            assert!(
                validate_scope(e, Some("abc")).is_err(),
                "{e} with canonical",
            );
            assert!(validate_scope(e, None).is_ok(), "{e} without canonical");
        }
    }

    #[test]
    fn scope_rejects_profile_entity_and_unknown() {
        // Server excludes `profile` from /entity by design.
        assert!(validate_scope("profile", Some("abc")).is_err());
        assert!(validate_scope("playlist_track", None).is_err());
    }

    #[test]
    fn remote_entity_row_deserialises_library_shape() {
        let body = serde_json::json!({
            "entity": "library",
            "canonical_id": "lib-uuid",
            "payload_hash": "deadbeef",
            "hlc": { "wall": 100, "logical": 1 },
            "origin_device_id": "11111111-1111-1111-1111-111111111111",
            "fields": {
                "name": "Bandes-son",
                "description": "best",
                "color_id": "azure",
                "icon_id": "library",
            }
        });
        let parsed: RemoteEntityRow = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.entity, "library");
        assert_eq!(parsed.canonical_id, "lib-uuid");
        assert_eq!(parsed.hlc.wall, 100);
        assert_eq!(parsed.hlc.logical, 1);
        assert!(parsed.origin_device_id.is_some());
        assert!(parsed.library_canonical_id.is_none());
        assert!(parsed.file_path.is_none());
        assert_eq!(parsed.fields["name"], "Bandes-son");
    }

    #[test]
    fn remote_entity_row_deserialises_track_shape_with_aux_fields() {
        // Track responses carry library_canonical_id + file_path
        // as top-level convenience fields alongside `fields`.
        let body = serde_json::json!({
            "entity": "track",
            "canonical_id": "lib-uuid\u{001F}/m/a.flac",
            "payload_hash": "abcd",
            "hlc": { "wall": 1, "logical": 0 },
            "fields": { "title": "X", "file_hash": "h" },
            "library_canonical_id": "lib-uuid",
            "file_path": "/m/a.flac",
        });
        let parsed: RemoteEntityRow = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.library_canonical_id.as_deref(), Some("lib-uuid"));
        assert_eq!(parsed.file_path.as_deref(), Some("/m/a.flac"));
        // origin_device_id omitted by server when None — deserialise as None.
        assert!(parsed.origin_device_id.is_none());
    }
}
