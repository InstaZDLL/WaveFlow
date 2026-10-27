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
