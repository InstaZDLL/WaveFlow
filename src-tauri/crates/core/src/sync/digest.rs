//! Set-hash computation for the `GET /api/v1/sync/digest` endpoint
//! (RFC-003 §4).
//!
//! Two replicas — the desktop and `waveflow-server` — must arrive at
//! the same `[u8; 32]` when they compute the digest over the same
//! materialised set, so the BLAKE3 feed has to be bit-identical on
//! both sides. The server already lives in
//! `waveflow-server/src/db.rs::digest_read::compute_set_hash`; this
//! module is the desktop's mirror, held in `waveflow-core` so a
//! future server-side cleanup can switch to importing it instead of
//! keeping a parallel impl.
//!
//! ## Feed shape
//!
//! For every member, ordered by ascending `canonical_id`:
//!
//! 1. `canonical_id.len() as u32` (little-endian, 4 bytes)
//! 2. `canonical_id` UTF-8 bytes
//! 3. `payload_bytes.len() as u32` (little-endian, 4 bytes)
//! 4. `payload_bytes` (raw BLAKE3-256, 32 bytes when present)
//!
//! Length prefixes are required: without them, two distinct
//! (canonical_id, payload_hash) pairs whose concatenations alias
//! would collide on the set hash. The corner case is rare in
//! practice (UUID canonical ids are fixed-width) but the file-path
//! composite key the `track` entity uses (`<lib_canonical>\u{1F}<file_path>`)
//! has variable length, which makes the prefix non-negotiable.
//!
//! ## Caller responsibilities
//!
//! - Members MUST be sorted by `canonical_id` ASC before hashing.
//!   The helper does not sort — sorting at the SQL layer is cheap
//!   (`ORDER BY canonical_id`) and lets the caller produce the
//!   sorted set with the same query that filters
//!   `payload_hash IS NOT NULL`.
//! - The `payload_bytes` slice is whatever the server stores in
//!   the row's `payload_hash` column — typically `[u8; 32]` from
//!   [`crate::sync::payload_hash::compute_payload_hash`]. The
//!   helper does not assume a fixed length; an empty slice falls
//!   through cleanly (still gets a `0u32` length prefix).
//!
//! ## Why the server hex-decodes and we do not
//!
//! The server feeds `hex::decode(&m.payload_hash)` because its
//! `DigestMember.payload_hash` is the JSON-serialised hex string.
//! The desktop's local read fetches the BLOB directly from SQLite,
//! so the bytes are already raw. The wire shape (hex) is only used
//! at the HTTP boundary; the hash feed shape (raw bytes) is the
//! protocol.

use blake3::Hasher;

/// Compute the BLAKE3-256 `set_hash` over a sorted list of members.
///
/// `members` MUST be sorted by `canonical_id` ASC. The helper does
/// not validate the order — feeding an unsorted slice produces a
/// hash that, by RFC-003 §4, no other compliant replica will match.
pub fn compute_set_hash(members: &[(&str, &[u8])]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    for (canonical_id, payload_bytes) in members {
        let id_bytes = canonical_id.as_bytes();
        // RFC-003 §4 frames each field with a `u32` little-endian
        // length prefix. Realistic inputs are bounded by OS
        // PATH_MAX (`canonical_id` peaks ~33 KB on Windows extended,
        // ~4 KB on Linux) and BLAKE3-256 (`payload_bytes` always 32
        // bytes), both several orders of magnitude below `u32::MAX`
        // (~4.3 GB). The `as u32` cast would silently truncate if
        // those invariants ever broke — and since the server uses
        // the identical cast (`waveflow-server/src/db.rs::compute_set_hash`),
        // truncation on either side stays cross-replica-consistent
        // by accident rather than design. `debug_assert!` documents
        // the contract + traps the regression in tests/dev without
        // forcing every caller to thread a `Result`.
        debug_assert!(
            id_bytes.len() <= u32::MAX as usize,
            "canonical_id length {} exceeds u32::MAX — RFC-003 §4 length prefix would truncate",
            id_bytes.len(),
        );
        debug_assert!(
            payload_bytes.len() <= u32::MAX as usize,
            "payload_bytes length {} exceeds u32::MAX — RFC-003 §4 length prefix would truncate",
            payload_bytes.len(),
        );
        hasher.update(&(id_bytes.len() as u32).to_le_bytes());
        hasher.update(id_bytes);
        hasher.update(&(payload_bytes.len() as u32).to_le_bytes());
        hasher.update(payload_bytes);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests;
