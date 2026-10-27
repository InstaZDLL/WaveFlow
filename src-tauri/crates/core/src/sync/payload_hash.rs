//! RFC-003 §2 / §4 helpers — payload-hash + HLC total-order
//! comparator + canonical serialisation.
//!
//! Three pure functions live here:
//!
//! - [`canonical_serialize`] / [`compute_payload_hash`] — RFC-003 §4
//!   "BLAKE3 over the entity's canonical wire form — every synced
//!   field plus `(hlc.wall, hlc.logical, origin_device_id)`,
//!   serialised in a deterministic shape (sorted JSON keys, lower-
//!   case hex for binary blobs)".
//! - [`hlc_strict_gt`] / [`HlcTriple`] — RFC-003 §2 total order on
//!   `(wall, logical, origin_device_id)`. Phase A's LWW gate.
//!
//! The module is intentionally storage-agnostic. The server binds
//! `[u8; 32]` straight into `BYTEA`; the desktop will bind into
//! `BLOB`; both round-trip the same canonical bytes for any logical
//! state. Phase A.4.1 lifts this out of `waveflow-server` into core
//! so the desktop emit path in A.4.2 can compute matching hashes
//! before the server bumps its pin in A.4.3.
//!
//! ## `canonical_serialize`
//!
//! Wraps the caller's entity fields under a top-level `"fields"` key
//! alongside `"hlc"` + `"origin_device_id"`. The wrapper guards
//! against a field name accidentally shadowing the HLC keys. Keys
//! are sorted recursively via [`BTreeMap`], so the resulting
//! `Vec<u8>` is byte-identical on every platform regardless of
//! whether `serde_json` was compiled with `preserve_order` (IndexMap
//! insertion order) or stays on the default `BTreeMap`.
//!
//! ## `compute_payload_hash`
//!
//! BLAKE3-256 over the canonical bytes. Output as `[u8; 32]` so the
//! caller can bind directly into the storage layer's binary blob
//! type without an extra hex encode / decode round-trip.
//!
//! ## `hlc_strict_gt`
//!
//! Tuple comparison uses Rust's derived lex ordering, which gives
//! `None < Some(any UUID)` automatically — that matches the
//! "legacy v1-derived row loses to any v2 op" intent from A.1.1's
//! header (legacy rows backfill with `origin_device_id = NULL`).
//! OR-Set / Fractional-Index semantics (which would extend the
//! tuple with `op_type`) land in Phase C, but they reuse the same
//! triple as the leading prefix.

use blake3::Hasher;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

use super::Hlc;

/// §2 total-order triple. Wraps the three components RFC-003 specifies
/// as the comparison key for Phase A's LWW rule (and as the leading
/// prefix of Phase C's OR-Set tuple).
///
/// `Hash` is derived so triples can serve as keys in dedup sets the
/// drain task may build to skip already-emitted ops in a single
/// batch. All fields (`i64`, `i32`, `Option<Uuid>`) already implement
/// `Hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HlcTriple {
    pub wall: i64,
    pub logical: i32,
    pub origin_device_id: Option<Uuid>,
}

impl HlcTriple {
    pub fn new(hlc: Hlc, origin_device_id: Option<Uuid>) -> Self {
        // RFC-003 §2 fixes the logical counter's legal range at
        // `0..=i32::MAX`. The desktop drain in A.4.2 (and the
        // server's existing A.1.1 bind-site guard) refuse values
        // outside that range at the wire boundary. Catch in-process
        // bugs here too: a negative `logical` round-trips through
        // canonical_serialize fine (it just hashes to a stable value)
        // but compares BELOW every legitimate triple under the
        // derived `Ord`, which would let LWW silently accept an
        // overwrite from a malformed op. `debug_assert!` keeps the
        // hot path zero-cost in release while surfacing the bug in
        // dev / CI.
        debug_assert!(
            hlc.logical >= 0,
            "HLC logical counter must be non-negative (0..=i32::MAX); got {}",
            hlc.logical
        );
        Self {
            wall: hlc.wall,
            logical: hlc.logical,
            origin_device_id,
        }
    }
}

/// `true` iff `incoming` strictly outranks `existing` under the §2
/// total order. The Phase A apply pipeline uses this to gate every
/// write: only newer ops mutate the row.
pub fn hlc_strict_gt(incoming: HlcTriple, existing: HlcTriple) -> bool {
    incoming > existing
}

/// Canonical bytes — entity fields + HLC + origin_device_id under a
/// deterministic wrapper. Sorted recursively so any two replicas
/// hashing the same logical state arrive at the same `[u8; 32]`.
///
/// Caller passes the SYNCED fields only — not server-derived columns
/// (created_at, updated_at, the row's BIGSERIAL id). Including those
/// would tie the hash to the apply order rather than the logical
/// state.
pub fn canonical_serialize(
    fields: &Map<String, Value>,
    hlc: Hlc,
    origin_device_id: Option<Uuid>,
) -> Vec<u8> {
    // Same contract as `HlcTriple::new` — RFC-003 §2 fixes the legal
    // range at `0..=i32::MAX`. A negative `logical` hashes to a
    // stable value, so a buggy caller would produce a self-consistent
    // payload that still fails LWW comparison at the apply site.
    // Catch it here too so the bug surfaces at the hash-compute call
    // rather than three steps later in the comparator.
    debug_assert!(
        hlc.logical >= 0,
        "HLC logical counter must be non-negative (0..=i32::MAX); got {}",
        hlc.logical
    );
    let wrapper = Value::Object({
        let mut wrapper = Map::new();
        wrapper.insert("fields".to_string(), Value::Object(fields.clone()));
        wrapper.insert(
            "hlc".to_string(),
            Value::Object({
                let mut hlc_map = Map::new();
                hlc_map.insert("logical".to_string(), Value::from(hlc.logical));
                hlc_map.insert("wall".to_string(), Value::from(hlc.wall));
                hlc_map
            }),
        );
        wrapper.insert(
            "origin_device_id".to_string(),
            match origin_device_id {
                Some(uuid) => Value::String(uuid.to_string()),
                None => Value::Null,
            },
        );
        wrapper
    });
    // Run the FULL tree (wrapper + nested entity fields) through
    // canonicalize so the byte form is deterministic regardless of
    // whether serde_json was compiled with `preserve_order`
    // (IndexMap insertion order) or stays on the default BTreeMap.
    // Without this top-level sort, the wrapper keys ("fields", "hlc",
    // "origin_device_id") + the hlc sub-map keys ("logical", "wall")
    // happened to land alphabetically by insertion-order accident —
    // a future edit that reordered the inserts would silently flap
    // every existing hash.
    //
    // `serde_json::to_vec` over a deterministic `Value` tree gives a
    // deterministic byte stream. The fail case is "Value contained a
    // float NaN" which never happens here — every input either came
    // through `Map<String, Value>` constructed from typed Rust values
    // or is the literal HLC pair / uuid string we just inserted.
    serde_json::to_vec(&canonicalize(&wrapper)).expect("canonical wrapper serialises")
}

/// BLAKE3-256 over the canonical form. Output sized for direct binary
/// blob bind (Postgres `BYTEA`, SQLite `BLOB`).
pub fn compute_payload_hash(
    fields: &Map<String, Value>,
    hlc: Hlc,
    origin_device_id: Option<Uuid>,
) -> [u8; 32] {
    let bytes = canonical_serialize(fields, hlc, origin_device_id);
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    *hasher.finalize().as_bytes()
}

/// Recursively reorder every JSON object's keys via `BTreeMap` so
/// the byte form is deterministic. Arrays stay in source order
/// (a sync field that's a list of strings preserves the desktop's
/// emission order — sorting it here would silently lose that
/// information).
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            let mut out = Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
