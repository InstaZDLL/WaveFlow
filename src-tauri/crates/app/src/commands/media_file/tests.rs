use super::human_bytes;

/// The sizes that actually reach a user: the cap, and a file just
/// over it. Both have to read like the numbers the rest of the app
/// shows, or the refusal is arithmetic homework.
#[test]
fn reads_like_the_rest_of_the_interface() {
    assert_eq!(human_bytes(256 * 1024 * 1024), "256 MB");
    assert_eq!(human_bytes(89_128_960), "85.0 MB");
    assert_eq!(human_bytes(64 * 1024 * 1024), "64.0 MB");
}

/// Below a kilobyte there is nothing to scale, and the loop must
/// stop at the last unit rather than running off the end of the
/// table on an absurd value.
#[test]
fn holds_at_both_ends() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1024), "1.0 KB");
    assert_eq!(human_bytes(9_007_199_254_740_992), "8192 TB");
}
