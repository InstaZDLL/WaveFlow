//! HTTP client for `GET /api/v1/sync/digest`.
//!
//! Returns the server's `DigestResponse` deserialised into local
//! wire types. The types mirror the server's
//! `waveflow_server::sync::{DigestResponse, DigestMember, MaxHlc}`
//! shape (same JSON keys, same `skip_serializing_if = Option::is_none`
//! on `max_hlc.origin_device_id`). Putting them in
//! `waveflow-core` would let us share the deserialise types
//! cross-repo, but the server's copies derive `utoipa::ToSchema`
//! and the desktop never needs the OpenAPI surface, so private
//! deserialise structs here keeps `waveflow-core` schema-free.
//!
//! ## Scope discipline
//!
//! - Profile-scoped entities (`library`, `playlist`, `track`)
//!   REQUIRE `profile_canonical_id`. The server returns 400
//!   without it.
//! - User-scoped entities (`liked_track`, `track_rating`) REJECT
//!   `profile_canonical_id`. The server returns 400 if it's
//!   present.
//!
//! We mirror that gate here so a typo at the call site fails
//! locally before the HTTP round-trip.

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::server_client::WaveflowServerClient;

/// One `(canonical_id, payload_hash)` pair from the server response.
/// `payload_hash` is hex-encoded BLAKE3-256 — the diff layer
/// decodes once before feeding the local set-hash comparison.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteMember {
    pub canonical_id: String,
    pub payload_hash: String,
}

/// The `max_hlc` field of the server response. `Option<Uuid>` is
/// `None` when no row carries an origin_device_id (every materialised
/// row predates v2 — Lamport-only era).
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteMaxHlc {
    pub wall: i64,
    pub logical: i32,
    #[serde(default)]
    pub origin_device_id: Option<Uuid>,
}

/// Top-level response. Mirrors `waveflow_server::sync::DigestResponse`.
/// `entity` isn't part of the wire shape (the server doesn't echo
/// the query param back); the diff layer carries it alongside.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteDigest {
    pub set_hash: String,
    pub version: i64,
    #[serde(default)]
    pub max_hlc: Option<RemoteMaxHlc>,
    pub members: Vec<RemoteMember>,
}

/// Fetch the server digest for `entity`.
///
/// Profile-scoped entities (`library` / `playlist` / `track`)
/// require `profile_canonical_id`; user-scoped ones (`liked_track`
/// / `track_rating`) require it absent. Mismatched pairs short-
/// circuit with a local error rather than wasting a 400 round-trip.
pub async fn fetch_remote_digest(
    client: &WaveflowServerClient,
    entity: &str,
    profile_canonical_id: Option<&str>,
) -> AppResult<RemoteDigest> {
    validate_scope(entity, profile_canonical_id)?;

    let mut query: Vec<(&str, &str)> = vec![("entity", entity)];
    if let Some(canon) = profile_canonical_id {
        query.push(("profile_canonical_id", canon));
    }

    let response = client
        .request(reqwest::Method::GET, "/api/v1/sync/digest")
        .query(&query)
        .send()
        .await
        .map_err(|err| AppError::Other(format!("sync digest GET: {err}")))?;

    let status = response.status();
    match status {
        StatusCode::OK => {
            let digest: RemoteDigest = response.json().await.map_err(|err| {
                AppError::Other(format!("sync digest deserialise: {err}"))
            })?;
            Ok(digest)
        }
        StatusCode::NOT_FOUND => Err(AppError::Other(format!(
            "sync digest GET {entity}: profile_canonical_id not visible to this user",
        ))),
        StatusCode::UNAUTHORIZED => Err(AppError::Other(
            "sync digest GET: unauthorized — JWT expired or revoked".into(),
        )),
        StatusCode::BAD_REQUEST => {
            // Server's body is plain text for the 400 path; surface
            // it verbatim so a wrong-scope call (caught locally now,
            // but defence-in-depth) yields a readable error.
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".into());
            Err(AppError::Other(format!(
                "sync digest GET {entity}: bad request — {body}",
            )))
        }
        other => {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".into());
            Err(AppError::Other(format!(
                "sync digest GET {entity}: unexpected status {other} — {body}",
            )))
        }
    }
}

fn validate_scope(entity: &str, profile_canonical_id: Option<&str>) -> AppResult<()> {
    match entity {
        "library" | "playlist" | "track" => {
            if profile_canonical_id.is_none() {
                return Err(AppError::Other(format!(
                    "sync digest {entity}: profile_canonical_id is required",
                )));
            }
        }
        "liked_track" | "track_rating" => {
            if profile_canonical_id.is_some() {
                return Err(AppError::Other(format!(
                    "sync digest {entity}: profile_canonical_id must be omitted",
                )));
            }
        }
        other => {
            return Err(AppError::Other(format!(
                "sync digest: unknown entity '{other}'",
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_scope_requires_canonical_for_profile_entities() {
        for e in ["library", "playlist", "track"] {
            assert!(validate_scope(e, None).is_err(), "{e} without canonical");
            assert!(validate_scope(e, Some("abc")).is_ok(), "{e} with canonical");
        }
    }

    #[test]
    fn validate_scope_rejects_canonical_for_user_entities() {
        for e in ["liked_track", "track_rating"] {
            assert!(
                validate_scope(e, Some("abc")).is_err(),
                "{e} with canonical",
            );
            assert!(validate_scope(e, None).is_ok(), "{e} without canonical");
        }
    }

    #[test]
    fn validate_scope_rejects_unknown_entity() {
        assert!(validate_scope("playlist_track", None).is_err());
        assert!(validate_scope("profile", Some("abc")).is_err());
    }

    #[test]
    fn remote_digest_deserialises_full_shape() {
        let body = serde_json::json!({
            "set_hash": "deadbeef",
            "version": 42,
            "max_hlc": {
                "wall": 1_700_000_000_000_i64,
                "logical": 3,
                "origin_device_id": "11111111-1111-1111-1111-111111111111"
            },
            "members": [
                { "canonical_id": "aaa", "payload_hash": "00aa" },
                { "canonical_id": "bbb", "payload_hash": "11bb" }
            ]
        });
        let parsed: RemoteDigest = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.set_hash, "deadbeef");
        assert_eq!(parsed.version, 42);
        let max = parsed.max_hlc.unwrap();
        assert_eq!(max.wall, 1_700_000_000_000);
        assert_eq!(max.logical, 3);
        assert!(max.origin_device_id.is_some());
        assert_eq!(parsed.members.len(), 2);
    }

    #[test]
    fn remote_digest_deserialises_with_omitted_max_hlc_and_no_origin() {
        // Server's `max_hlc.origin_device_id` is
        // `skip_serializing_if = Option::is_none`, and the whole
        // `max_hlc` is `Option<MaxHlc>` (None when set is empty).
        // Both shapes must round-trip through `serde(default)`.
        let body = serde_json::json!({
            "set_hash": "abcd",
            "version": 0,
            "members": []
        });
        let parsed: RemoteDigest = serde_json::from_value(body).unwrap();
        assert!(parsed.max_hlc.is_none());

        let body2 = serde_json::json!({
            "set_hash": "abcd",
            "version": 1,
            "max_hlc": { "wall": 10, "logical": 0 },
            "members": [{ "canonical_id": "x", "payload_hash": "00" }]
        });
        let parsed2: RemoteDigest = serde_json::from_value(body2).unwrap();
        let max = parsed2.max_hlc.unwrap();
        assert_eq!(max.wall, 10);
        assert!(max.origin_device_id.is_none());
    }
}
