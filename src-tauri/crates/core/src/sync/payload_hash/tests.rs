use super::*;
use serde_json::json;

fn hlc(wall: i64, logical: i32) -> Hlc {
    Hlc { wall, logical }
}

fn fields(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => panic!("test fields must be an object"),
    }
}

#[test]
fn canonical_serialize_is_deterministic_across_key_order() {
    // Same logical state expressed with keys in different order
    // must hash identically — that's the whole point of the
    // canonical form.
    let a = fields(json!({ "name": "A", "color": "red", "icon": "music" }));
    let b = fields(json!({ "icon": "music", "color": "red", "name": "A" }));

    let bytes_a = canonical_serialize(&a, hlc(1, 0), None);
    let bytes_b = canonical_serialize(&b, hlc(1, 0), None);

    assert_eq!(bytes_a, bytes_b);
    assert_eq!(
        compute_payload_hash(&a, hlc(1, 0), None),
        compute_payload_hash(&b, hlc(1, 0), None)
    );
}

#[test]
fn canonical_serialize_changes_with_field_value() {
    let a = fields(json!({ "name": "A" }));
    let b = fields(json!({ "name": "B" }));
    assert_ne!(
        compute_payload_hash(&a, hlc(1, 0), None),
        compute_payload_hash(&b, hlc(1, 0), None),
    );
}

#[test]
fn canonical_serialize_changes_with_hlc() {
    let f = fields(json!({ "name": "A" }));
    assert_ne!(
        compute_payload_hash(&f, hlc(1, 0), None),
        compute_payload_hash(&f, hlc(1, 1), None),
    );
    assert_ne!(
        compute_payload_hash(&f, hlc(1, 0), None),
        compute_payload_hash(&f, hlc(2, 0), None),
    );
}

#[test]
fn canonical_serialize_changes_with_origin_device_id() {
    let f = fields(json!({ "name": "A" }));
    let uuid_a = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let uuid_b = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    assert_ne!(
        compute_payload_hash(&f, hlc(1, 0), Some(uuid_a)),
        compute_payload_hash(&f, hlc(1, 0), Some(uuid_b)),
    );
    assert_ne!(
        compute_payload_hash(&f, hlc(1, 0), Some(uuid_a)),
        compute_payload_hash(&f, hlc(1, 0), None),
    );
}

#[test]
fn canonical_serialize_array_order_is_preserved() {
    // Arrays MUST keep their source order — a multi-artist tag
    // [Tyler, Earl] is distinct from [Earl, Tyler]. Hashing them
    // identically would let the apply pipeline silently swap
    // primary / secondary artist on a re-emit.
    let a = fields(json!({ "artists": ["Tyler", "Earl"] }));
    let b = fields(json!({ "artists": ["Earl", "Tyler"] }));
    assert_ne!(
        compute_payload_hash(&a, hlc(1, 0), None),
        compute_payload_hash(&b, hlc(1, 0), None),
    );
}

#[test]
fn canonical_serialize_nested_objects_are_sorted() {
    // The recursive sort must reach nested objects too — a
    // playlist that ships `{ snapshots: { id1: {...}, id2: {...} } }`
    // shouldn't flap its hash based on the desktop's serde_json
    // emission order.
    let a = fields(json!({ "outer": { "z": 1, "a": 2 } }));
    let b = fields(json!({ "outer": { "a": 2, "z": 1 } }));
    assert_eq!(
        compute_payload_hash(&a, hlc(1, 0), None),
        compute_payload_hash(&b, hlc(1, 0), None),
    );
}

#[test]
fn canonical_serialize_top_level_keys_are_sorted() {
    // The wrapper itself + the hlc sub-map MUST be sorted by
    // canonicalize, not by insertion order. Verify by inspecting
    // the raw byte form: keys must appear in lexicographic order
    // regardless of how they were inserted in canonical_serialize.
    let f = fields(json!({ "name": "A" }));
    let bytes = canonical_serialize(&f, hlc(7, 3), None);
    let text = std::str::from_utf8(&bytes).unwrap();
    // "fields" < "hlc" < "origin_device_id" lex-wise.
    let p_fields = text.find("\"fields\"").unwrap();
    let p_hlc = text.find("\"hlc\"").unwrap();
    let p_origin = text.find("\"origin_device_id\"").unwrap();
    assert!(p_fields < p_hlc);
    assert!(p_hlc < p_origin);
    // "logical" < "wall" lex-wise.
    let p_logical = text.find("\"logical\"").unwrap();
    let p_wall = text.find("\"wall\"").unwrap();
    assert!(p_logical < p_wall);
}

#[test]
fn hlc_strict_gt_compares_wall_first() {
    let later = HlcTriple {
        wall: 2,
        logical: 0,
        origin_device_id: None,
    };
    let earlier = HlcTriple {
        wall: 1,
        logical: 99,
        origin_device_id: None,
    };
    assert!(hlc_strict_gt(later, earlier));
    assert!(!hlc_strict_gt(earlier, later));
}

#[test]
fn hlc_strict_gt_tiebreaks_on_logical() {
    let later = HlcTriple {
        wall: 1,
        logical: 2,
        origin_device_id: None,
    };
    let earlier = HlcTriple {
        wall: 1,
        logical: 1,
        origin_device_id: None,
    };
    assert!(hlc_strict_gt(later, earlier));
}

#[test]
fn hlc_strict_gt_tiebreaks_on_origin_device_id() {
    // Same (wall, logical) — origin_device_id breaks the tie.
    let uuid_low = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let uuid_high = Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
    let higher = HlcTriple {
        wall: 1,
        logical: 1,
        origin_device_id: Some(uuid_high),
    };
    let lower = HlcTriple {
        wall: 1,
        logical: 1,
        origin_device_id: Some(uuid_low),
    };
    assert!(hlc_strict_gt(higher, lower));
    assert!(!hlc_strict_gt(lower, higher));
}

#[test]
fn hlc_strict_gt_none_loses_to_some() {
    // Legacy backfilled rows carry `origin_device_id = None`. A
    // v2 op with any UUID must strictly outrank them on the
    // tiebreak — matches the A.1.1 backfill semantics.
    let v2 = HlcTriple {
        wall: 1,
        logical: 1,
        origin_device_id: Some(Uuid::nil()),
    };
    let legacy = HlcTriple {
        wall: 1,
        logical: 1,
        origin_device_id: None,
    };
    assert!(hlc_strict_gt(v2, legacy));
    assert!(!hlc_strict_gt(legacy, v2));
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "HLC logical counter must be non-negative")]
fn hlc_triple_new_rejects_negative_logical_in_debug() {
    // RFC-003 §2 invariant — the desktop drain (A.4.2) + server
    // bind site (A.1.1) refuse `logical < 0` at the wire boundary.
    // Catch the in-process variant here so an integration bug
    // can't ship a self-consistent-but-wrong hash. Release builds
    // strip the assert; the wire guards still hold.
    let _ = HlcTriple::new(
        Hlc {
            wall: 1,
            logical: -1,
        },
        None,
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "HLC logical counter must be non-negative")]
fn canonical_serialize_rejects_negative_logical_in_debug() {
    let f = fields(json!({ "name": "A" }));
    let _ = canonical_serialize(
        &f,
        Hlc {
            wall: 1,
            logical: -1,
        },
        None,
    );
}

#[test]
fn hlc_strict_gt_rejects_equal_triple() {
    // RFC-003 §2: idempotency — the apply pipeline treats equal
    // tuples as a no-op replay, not an overwrite.
    let same = HlcTriple {
        wall: 5,
        logical: 7,
        origin_device_id: Some(Uuid::nil()),
    };
    assert!(!hlc_strict_gt(same, same));
}
