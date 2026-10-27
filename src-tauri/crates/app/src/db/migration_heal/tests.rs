use super::*;

#[test]
fn lf_to_crlf_round_trips_through_normalize() {
    let lf = b"a\nb\nc\n";
    let crlf = lf_to_crlf(lf);
    assert_eq!(crlf, b"a\r\nb\r\nc\r\n");
    assert_eq!(normalize_to_lf(&crlf), lf);
}

#[test]
fn normalize_preserves_lone_cr() {
    // An old Mac classic line ending or a literal embedded \r in a
    // string should NOT be collapsed — only the CRLF pair.
    let mixed = b"a\rb\r\nc";
    assert_eq!(normalize_to_lf(mixed), b"a\rb\nc");
}

#[test]
fn normalize_is_idempotent_on_lf() {
    let lf = b"line1\nline2\n";
    assert_eq!(normalize_to_lf(lf), lf);
}

#[test]
fn lf_and_crlf_variants_hash_differently() {
    // Sanity: if these collided the whole heuristic would be a
    // no-op. SHA-384 over distinct inputs must give distinct
    // outputs (modulo cosmic-ray collisions).
    let lf = b"create table foo (id int);\n";
    let crlf = lf_to_crlf(lf);
    assert_ne!(sha384(lf), sha384(&crlf));
}
