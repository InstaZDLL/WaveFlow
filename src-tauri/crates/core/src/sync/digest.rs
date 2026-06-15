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
mod tests {
    use super::*;

    /// Two empty inputs hash identically (the empty BLAKE3 digest).
    #[test]
    fn empty_set_hashes_to_blake3_of_nothing() {
        let h1 = compute_set_hash(&[]);
        let h2 = compute_set_hash(&[]);
        assert_eq!(h1, h2);
        // Sanity: known empty BLAKE3 digest hex.
        assert_eq!(
            hex::encode(h1),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        );
    }

    /// Same members in same order produce the same hash.
    #[test]
    fn same_members_same_order_match() {
        let payload = [0x42u8; 32];
        let m = vec![("alpha", payload.as_slice()), ("beta", payload.as_slice())];
        let h1 = compute_set_hash(&m);
        let h2 = compute_set_hash(&m);
        assert_eq!(h1, h2);
    }

    /// Different orderings produce different hashes — the caller's
    /// sort-by-canonical-id is what guarantees cross-replica
    /// agreement.
    #[test]
    fn different_orderings_differ() {
        let p1 = [0x01u8; 32];
        let p2 = [0x02u8; 32];
        let ordered = compute_set_hash(&[("alpha", &p1[..]), ("beta", &p2[..])]);
        let reversed = compute_set_hash(&[("beta", &p2[..]), ("alpha", &p1[..])]);
        assert_ne!(ordered, reversed);
    }

    /// Length prefixes prevent the concatenation collision: the
    /// pairs `("a", "bc")` and `("ab", "c")` would produce the same
    /// raw byte stream without the `u32` length frames between
    /// them.
    #[test]
    fn length_prefixes_prevent_concatenation_collision() {
        let a = compute_set_hash(&[("a", b"bc")]);
        let b = compute_set_hash(&[("ab", b"c")]);
        assert_ne!(a, b);
    }

    /// Empty payload (e.g. `liked_track` canonical form is `{}`,
    /// but the row's `payload_hash` column is still a 32-byte
    /// BLAKE3 — this test covers the corner case where a row's
    /// hash was stored as an empty slice for whatever reason; the
    /// algorithm still terminates).
    #[test]
    fn empty_payload_bytes_terminate_cleanly() {
        let h = compute_set_hash(&[("x", b"")]);
        // Smoke: deterministic across calls.
        assert_eq!(h, compute_set_hash(&[("x", b"")]));
    }

    /// Changing one canonical_id byte changes the hash — i.e. the
    /// canonical_id contributes to the feed (not just the payload).
    #[test]
    fn canonical_id_change_changes_hash() {
        let p = [0xAAu8; 32];
        let a = compute_set_hash(&[("alpha", &p[..])]);
        let b = compute_set_hash(&[("alphz", &p[..])]);
        assert_ne!(a, b);
    }

    /// Changing one payload byte changes the hash.
    #[test]
    fn payload_change_changes_hash() {
        let mut p = [0xAAu8; 32];
        let a = compute_set_hash(&[("alpha", &p[..])]);
        p[0] = 0xBB;
        let b = compute_set_hash(&[("alpha", &p[..])]);
        assert_ne!(a, b);
    }

    /// Composite canonical_id with the `\u{1F}` track separator
    /// hashes deterministically — sanity that the multibyte unit
    /// separator passes through `as_bytes()` unchanged.
    #[test]
    fn composite_track_key_round_trips() {
        let p = [0u8; 32];
        let composite = "11111111-1111-1111-1111-111111111111\u{1F}/music/x.flac";
        let a = compute_set_hash(&[(composite, &p[..])]);
        let b = compute_set_hash(&[(composite, &p[..])]);
        assert_eq!(a, b);
    }
}
