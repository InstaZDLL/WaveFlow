use super::*;
use std::fs;

fn touch(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"x").unwrap();
}

#[test]
fn prefers_existing_local_over_cache() {
    let local = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    touch(local.path(), "abc.png");
    touch(cache.path(), "def.jpg"); // cache also present

    let got = resolve_local_or_cached_path(
        local.path(),
        Some("abc"),
        Some("png"),
        cache.path(),
        Some("def"),
    )
    .unwrap();
    assert_eq!(got, local.path().join("abc.png").to_string_lossy());
}

#[test]
fn falls_back_to_cache_when_local_file_missing() {
    let local = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // Local hash/format point at a file that was wiped; only the cache exists.
    touch(cache.path(), "def.jpg");

    let got = resolve_local_or_cached_path(
        local.path(),
        Some("abc"),
        Some("png"),
        cache.path(),
        Some("def"),
    )
    .unwrap();
    assert_eq!(got, cache.path().join("def.jpg").to_string_lossy());
}

#[test]
fn incomplete_local_skips_to_cache() {
    let local = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    touch(cache.path(), "def.jpg");

    // hash without format → local candidate is not even attempted.
    let got =
        resolve_local_or_cached_path(local.path(), Some("abc"), None, cache.path(), Some("def"));
    assert_eq!(got.unwrap(), cache.path().join("def.jpg").to_string_lossy());
}

#[test]
fn none_when_nothing_resolves() {
    let local = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Local file missing, no cache hash at all.
    assert!(resolve_local_or_cached_path(
        local.path(),
        Some("abc"),
        Some("png"),
        cache.path(),
        None,
    )
    .is_none());

    // Cache hash given but the file doesn't exist on disk either.
    assert!(
        resolve_local_or_cached_path(local.path(), None, None, cache.path(), Some("ghost"),)
            .is_none()
    );
}
