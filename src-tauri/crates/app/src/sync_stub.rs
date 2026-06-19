//! Sync module stub — replaces `crate::sync` when the `sync_v1`
//! Cargo feature is OFF.
//!
//! Why a stub instead of compile-flag wrapping every call site? The
//! sync emit path is interleaved with the CRUD transactions in
//! `commands/{library,playlist,track,scan,duplicates}.rs` — ~70 call
//! sites in total. Wrapping each in `#[cfg(feature = "sync_v1")]`
//! would mean an audit pass on every future CRUD change. The stub
//! pattern is: public API surface identical to the real module, all
//! functions short-circuit to "sync disabled" responses, and the
//! existing call sites compile unchanged.
//!
//! ## Short-circuit semantics
//!
//! The real `enqueue_op_in_tx` returns `Ok(None)` when sync gates
//! refuse (no JWT, `SyncMode::Local`, offline). Every emit call site
//! already handles the None branch:
//!
//! ```ignore
//! let stamp = crate::sync::hooks::enqueue_op_in_tx(...).await?;
//! if let Some(stamp) = stamp {
//!     // payload_hash stamp + digest bump — skipped when None
//! }
//! ```
//!
//! The stub returns `Ok(None)` unconditionally, which means the
//! existing skip path fires on every call. Net effect: CRUD writes
//! land normally; nothing is enqueued, nothing is stamped, nothing
//! is hashed. The `sync_op_queue` table stays empty.
//!
//! ## What's missing from the binary
//!
//! - HTTP client (`server_client`) — feature-gated alongside `sync`.
//! - WS subscriber (`tokio-tungstenite` upgrade + reconnect loop).
//! - Drain task + backfill orchestrator (digest diff, LWW merge).
//! - Payload hash builder (`waveflow_core::sync::payload_hash`).
//! - All `commands/{sync,share,server_auth,loopback}` Tauri handlers.
//!
//! ## When re-enabled (1.6.0)
//!
//! `cargo build --features sync_v1` resolves `mod sync` to the real
//! module (`src/sync/`). This stub stays in tree as the source-of-
//! truth for "what the public API surface looks like" — drift in
//! either direction breaks the build.

// Every field on every stub struct is "dead" in the sync-off build —
// callers construct the structs (the literal-init call sites in
// library / playlist / scan must still compile) but nothing reads
// them back because the stub fns short-circuit before any field
// access. Same goes for the wake() method on SubscribeHandle. The
// allow keeps the build noise-free without leaking the suppression
// into the real module under `--features sync_v1`.
#![allow(dead_code)]

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::error::AppResult;

/// `sync::canonical` stub. The real module manages the
/// `sync_id_map` table that pairs local row ids with UUID canonical
/// ids; with sync off there's no need for the mapping so every
/// function short-circuits to a benign default. Existing canonical_id
/// columns on the entity tables (populated by the
/// `trg_*_set_canonical_id_on_insert` migrations) keep working
/// independently — the stub doesn't touch them.
pub mod canonical {
    use super::*;

    pub const ENTITY_PLAYLIST: &str = "playlist";
    pub const ENTITY_LIBRARY: &str = "library";
    pub const ENTITY_LIKED_TRACK: &str = "liked_track";
    pub const ENTITY_TRACK_RATING: &str = "track_rating";

    /// Real impl reads `sync_id_map`. Stub returns `None` so any
    /// caller that branches on "no mapping yet" picks the legacy
    /// path — typically the entity write proceeds without a sync
    /// emit, which is the desired behaviour.
    pub async fn canonical_for_local(
        _conn: &mut SqliteConnection,
        _entity: &str,
        _local_id: i64,
    ) -> AppResult<Option<String>> {
        Ok(None)
    }

    /// Real impl plants a mapping row + returns the canonical id.
    /// Stub returns an empty string — callers feed it into the
    /// `PendingOpDraft.entity_id` field which the stub's
    /// `enqueue_op_in_tx` drops on the floor.
    pub async fn ensure_local_playlist(
        _conn: &mut SqliteConnection,
        _local_id: i64,
    ) -> AppResult<String> {
        Ok(String::new())
    }

    pub async fn ensure_local_library(
        _conn: &mut SqliteConnection,
        _local_id: i64,
    ) -> AppResult<String> {
        Ok(String::new())
    }

    /// Real impl SELECTs `track.file_hash`. Stub returns None so
    /// emit paths that key liked / rating ops on the hash skip
    /// silently when sync is off.
    pub async fn file_hash_for_local_track(
        _conn: &mut SqliteConnection,
        _local_id: i64,
    ) -> AppResult<Option<String>> {
        Ok(None)
    }

    pub async fn drop_mapping(
        _conn: &mut SqliteConnection,
        _entity: &str,
        _canonical: &str,
    ) -> AppResult<()> {
        Ok(())
    }
}

/// `sync::hooks` stub. Matches the real module's re-export of
/// `PendingOpDraft` from `sync::queue` so the call sites that
/// construct it via `&crate::sync::hooks::PendingOpDraft { … }`
/// compile unchanged.
pub mod hooks {
    use super::*;
    use serde_json::Value as JsonValue;

    /// Shape-only mirror of `sync::queue::PendingOpDraft`. Fields
    /// are public + identical so the literal-construction call
    /// sites compile unchanged. Stub's `enqueue_op_in_tx` ignores
    /// the value.
    #[derive(Debug, Clone)]
    pub struct PendingOpDraft {
        pub entity: String,
        pub entity_id: String,
        pub field: Option<String>,
        pub op: String,
        pub payload: Option<JsonValue>,
    }

    /// Shape-only mirror of the real `EnqueuedStamp`. Returned by
    /// `enqueue_op_in_tx` only inside the `Some(_)` arm — the stub
    /// never constructs one (returns `None` unconditionally), so the
    /// struct exists purely as a type tag for the call sites that
    /// destructure `if let Some(stamp) = …`.
    #[derive(Debug, Clone, Copy)]
    pub struct EnqueuedStamp {
        pub hlc_wall: i64,
        pub hlc_logical: i32,
        pub origin_device_id: Option<Uuid>,
    }

    /// The pivot. Real impl gates on JWT presence + sync mode then
    /// writes `sync_op_queue`. Stub returns `Ok(None)` — every
    /// caller's `if let Some(stamp)` block short-circuits, so no
    /// payload_hash is computed and no digest version bumps.
    pub async fn enqueue_op_in_tx(
        _conn: &mut SqliteConnection,
        _draft: &PendingOpDraft,
    ) -> AppResult<Option<EnqueuedStamp>> {
        Ok(None)
    }
}

/// `sync::payload` stub. Real module computes BLAKE3 payload_hash
/// over canonical-field JSON; stub no-ops. Submodules mirror the
/// per-entity stamp helpers.
pub mod payload {
    use super::*;

    /// No-op — the real impl bumps `metadata_digest_version` so the
    /// digest endpoint observes a state change. With sync off no
    /// digest endpoint reads the table.
    pub async fn bump_digest_in_tx(
        _conn: &mut SqliteConnection,
        _entity: &str,
    ) -> AppResult<()> {
        Ok(())
    }

    pub mod library {
        use super::super::*;
        use crate::sync::hooks::EnqueuedStamp;

        /// Real impl SELECTs the row + builds a canonical-fields map.
        /// Stub returns `None` so any caller that uses the returned
        /// fields downstream (typically just `stamp_in_tx`) skips.
        /// Combined with `enqueue_op_in_tx` returning `None` upstream,
        /// the whole emit chain short-circuits without writing
        /// anything to the row.
        pub async fn fields_from_row(
            _conn: &mut SqliteConnection,
            _local_id: i64,
        ) -> AppResult<Option<Map<String, Value>>> {
            Ok(None)
        }

        pub async fn stamp_in_tx(
            _conn: &mut SqliteConnection,
            _local_id: i64,
            _fields: Map<String, Value>,
            _stamp: EnqueuedStamp,
        ) -> AppResult<()> {
            Ok(())
        }
    }

    pub mod playlist {
        use super::super::*;
        use crate::sync::hooks::EnqueuedStamp;

        pub async fn fields_from_row(
            _conn: &mut SqliteConnection,
            _local_id: i64,
        ) -> AppResult<Option<Map<String, Value>>> {
            Ok(None)
        }

        pub async fn stamp_in_tx(
            _conn: &mut SqliteConnection,
            _local_id: i64,
            _fields: Map<String, Value>,
            _stamp: EnqueuedStamp,
        ) -> AppResult<()> {
            Ok(())
        }
    }

    pub mod liked_track {
        use super::super::*;
        use crate::sync::hooks::EnqueuedStamp;

        pub async fn stamp_in_tx(
            _conn: &mut SqliteConnection,
            _track_id: i64,
            _stamp: EnqueuedStamp,
        ) -> AppResult<()> {
            Ok(())
        }

        pub async fn bump_for_delete_in_tx(_conn: &mut SqliteConnection) -> AppResult<()> {
            Ok(())
        }
    }

    pub mod track_rating {
        use super::super::*;
        use crate::sync::hooks::EnqueuedStamp;

        pub async fn stamp_set_in_tx(
            _conn: &mut SqliteConnection,
            _track_id: i64,
            _rating: i64,
            _stamp: EnqueuedStamp,
        ) -> AppResult<()> {
            Ok(())
        }

        pub async fn stamp_delete_in_tx(
            _conn: &mut SqliteConnection,
            _track_id: i64,
            _stamp: EnqueuedStamp,
        ) -> AppResult<()> {
            Ok(())
        }
    }
}

/// `sync::track_emit` stub. The real module builds the server's
/// `apply::track::insert` wire payload + enqueues track ops on
/// scanner / duplicates / library-folder-removal paths. Stub no-ops
/// — the scanner still writes local rows; nothing leaves the device.
pub mod track_emit {
    use super::*;

    /// Same field layout as the real `TrackInsertWire` so the
    /// scanner literal-construction sites compile unchanged. Stub
    /// doesn't read any of it (`emit_*` no-op).
    #[derive(Debug, Clone)]
    pub struct TrackInsertWire<'a> {
        pub file_hash: &'a str,
        pub title: &'a str,
        pub file_size: i64,
        pub file_modified: i64,
        pub duration_ms: i64,
        pub track_number: Option<i64>,
        pub disc_number: Option<i64>,
        pub year: Option<i64>,
        pub bitrate: Option<i64>,
        pub sample_rate: Option<i64>,
        pub channels: Option<i64>,
        pub bit_depth: Option<i64>,
        pub codec: Option<&'a str>,
        pub musical_key: Option<&'a str>,
        pub added_at: i64,
        pub album_title: Option<&'a str>,
        pub album_artist_name: Option<&'a str>,
        pub is_compilation: bool,
        pub artists: &'a [String],
    }

    pub async fn emit_track_insert_in_tx(
        _conn: &mut SqliteConnection,
        _library_id: i64,
        _track_id: i64,
        _file_path: &str,
        _wire: &TrackInsertWire<'_>,
    ) -> AppResult<()> {
        Ok(())
    }

    pub async fn emit_track_delete_in_tx(
        _conn: &mut SqliteConnection,
        _library_id: i64,
        _file_path: &str,
    ) -> AppResult<()> {
        Ok(())
    }
}

/// `sync::track_snapshots` stub. Real impl resolves track ids to
/// `{title, artist?, duration_ms?}` triples folded into outbound
/// playlist + tracks ops so the server can render public share
/// previews offline. Stub returns an empty JSON object — the playlist
/// emit paths still feed it as `snapshots:` in the payload, the stub
/// `enqueue_op_in_tx` drops the whole payload anyway.
pub mod track_snapshots {
    use super::*;

    pub async fn build_snapshots(
        _conn: &mut SqliteConnection,
        _track_ids: &[i64],
    ) -> AppResult<Value> {
        Ok(Value::Object(Map::new()))
    }
}

/// `sync::drain` stub. Real `DrainHandle` wraps a `tokio::sync::Notify`
/// that the drain task awaits on; stub keeps the same surface so
/// `AppState.drain.notify()` call sites compile, but the wake signal
/// has no listener (no task spawned in the stub world).
pub mod drain {
    /// Empty stub — kept as a unit-struct-with-default so the
    /// `Arc<DrainHandle>` field in `AppState` is constructible the
    /// same way.
    #[derive(Default)]
    pub struct DrainHandle;

    impl DrainHandle {
        /// No-op when sync is off. Kept as a method so existing call
        /// sites (`state.drain.notify()` in every CRUD command)
        /// compile unchanged.
        pub fn notify(&self) {}
    }
}

/// `sync::ws` stub. Same shape as `drain` — `wake()` no-op, struct
/// kept for the `AppState.ws` field.
pub mod ws {
    #[derive(Default)]
    pub struct SubscribeHandle;

    impl SubscribeHandle {
        pub fn wake(&self) {}
    }
}
