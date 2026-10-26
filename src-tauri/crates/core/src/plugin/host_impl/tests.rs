use super::*;
use crate::plugin::manifest::{Permissions, PluginMetadata};

fn manifest_with_permissions(permissions: Permissions) -> Manifest {
    Manifest {
        schema_version: 1,
        plugin: PluginMetadata {
            id: "test".into(),
            name: "Test".into(),
            version: "1.0.0".into(),
            author: "x".into(),
            world: waveflow_plugin_sdk::worlds::SOURCE_V1.into(),
            description: None,
            description_i18n: None,
            homepage: None,
            license: None,
            provides: Vec::new(),
        },
        permissions,
        assets: vec![],
        options: vec![],
    }
}

#[test]
fn no_http_pattern_means_deny() {
    let m = manifest_with_permissions(Permissions::default());
    let p = HostPermissions::from_manifest(&m).expect("compile empty allowlist");
    assert!(!p.http_allowed("https://example.com/api"));
}

#[test]
fn http_allowlist_matches_glob_pattern() {
    let m = manifest_with_permissions(Permissions {
        http: vec!["https://*.radio-browser.info/**".into()],
        storage_read: false,
        storage_state: false,
        library_read_artists: false,
    });
    let p = HostPermissions::from_manifest(&m).expect("compile glob");
    assert!(p.http_allowed("https://de1.api.radio-browser.info/json/stations"));
    assert!(p.http_allowed("https://us.api.radio-browser.info/json/stations/byuuid/abc"));
    assert!(!p.http_allowed("https://malicious.example.com/api"));
    assert!(!p.http_allowed("http://de1.api.radio-browser.info/json/stations"));
}

#[test]
fn invalid_glob_pattern_surfaces_error() {
    let m = manifest_with_permissions(Permissions {
        http: vec!["[".into()],
        storage_read: false,
        storage_state: false,
        library_read_artists: false,
    });
    match HostPermissions::from_manifest(&m) {
        Ok(_) => panic!("expected glob compile error, got Ok"),
        Err(HostError::InvalidPattern { pattern, .. }) => assert_eq!(pattern, "["),
        Err(err) => panic!("expected InvalidPattern, got {err:?}"),
    }
}

#[test]
fn state_store_roundtrips_value() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StateStore::new(tmp.path().join("plugin-x"), STATE_QUOTA_BYTES);
    assert!(store.read("missing").expect("read").is_none());
    store.write("favourites", b"a;b;c").expect("write");
    let got = store.read("favourites").expect("read after write");
    assert_eq!(got.as_deref(), Some(b"a;b;c".as_slice()));
}

#[test]
fn state_store_rejects_quota_overflow() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
    store.write("a", &vec![0u8; 600]).expect("first write fits");
    match store.write("b", &vec![0u8; 600]) {
        Ok(()) => panic!("expected quota error"),
        Err(HostError::QuotaExceeded { would_use, cap }) => {
            assert_eq!(would_use, 1200);
            assert_eq!(cap, 1024);
        }
        Err(err) => panic!("expected QuotaExceeded, got {err:?}"),
    }
    // The rejected write must NOT have left a partial file.
    assert!(store.read("b").expect("read").is_none());
}

#[test]
fn state_store_overwrite_does_not_double_count() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
    store
        .write("one", &vec![0u8; 800])
        .expect("first write fits");
    // Overwriting `one` with another 800-byte payload should
    // leave usage at 800, not 1600 — the quota check must
    // exclude the file being replaced.
    store.write("one", &vec![1u8; 800]).expect("overwrite fits");
    assert_eq!(store.used_bytes().expect("usage"), 800);
}

#[test]
fn http_send_offline_supersedes_permission_denial() {
    // Locks the ORDERING INVARIANT in `Host::send`. CLAUDE.md
    // says every outbound HTTP path checks offline first; a
    // guest probing a non-allowlisted URL while the host is
    // offline must see the same empty 503 every other call
    // gets. The WIT contract pins permission ahead of the
    // network dispatch, not ahead of host-state short-circuits
    // — both gates still fire before any actual `builder.send()`.
    use crate::plugin::bindings::source::waveflow::host::http::{Host, Request};
    use crate::plugin::runtime::HostCtx;
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut ctx = HostCtx::stub(64 * 1024 * 1024, tmp.path().to_path_buf());
    // Empty allowlist (= deny everything) and offline.
    ctx.offline_probe = Arc::new(|| true);

    let req = Request {
        method: "GET".into(),
        url: "https://example.com/api".into(),
        headers: Vec::new(),
        body: None,
    };
    let result = ctx.send(req).expect("host call should not trap");
    match result {
        Ok(resp) => {
            assert_eq!(
                resp.status, 503,
                "offline must short-circuit before the allowlist check"
            );
        }
        Err(err) => panic!("expected empty 503 response, got Err({err})"),
    }
}

#[test]
fn http_send_short_circuits_when_offline() {
    // Build a HostCtx with an HTTP allowlist that authorises
    // example.com + an offline probe that says "yes, offline".
    // The send must NOT hit the network — it must return an
    // empty 503 response synthesised by the offline branch,
    // verifiable by `Vec::is_empty()` on the body without
    // depending on what `example.com` would actually serve.
    use crate::plugin::bindings::source::waveflow::host::http::{Host, Request};
    use crate::plugin::runtime::HostCtx;
    use std::sync::Arc;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut ctx = HostCtx::stub(64 * 1024 * 1024, tmp.path().to_path_buf());
    // Allow the request URL so we DON'T trip the permission
    // branch — the offline check is what we want to exercise.
    ctx.permissions = HostPermissions::from_manifest(&manifest_with_permissions(
        crate::plugin::manifest::Permissions {
            http: vec!["https://example.com/*".into()],
            storage_read: false,
            storage_state: false,
            library_read_artists: false,
        },
    ))
    .expect("compile allowlist");
    ctx.offline_probe = Arc::new(|| true);

    let req = Request {
        method: "GET".into(),
        url: "https://example.com/api".into(),
        headers: Vec::new(),
        body: None,
    };
    let result = ctx.send(req).expect("host call should not trap");
    match result {
        Ok(resp) => {
            assert_eq!(resp.status, 503);
            assert!(resp.headers.is_empty());
            assert!(resp.body.is_empty());
        }
        Err(err) => panic!("expected empty 503 response, got Err({err})"),
    }
}

#[test]
fn state_store_lock_file_excluded_from_quota() {
    // After a write, the on-disk dir contains both the value
    // file AND the `.write-lock` sentinel. The lock file must
    // not count against the quota or every subsequent write
    // would slowly run out of room over the lock-file's own
    // size (zero today but defensive — Windows occasionally
    // leaves a few bytes in similar handles).
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StateStore::new(tmp.path().join("plugin-x"), 1024);
    store.write("a", &vec![0u8; 1024]).expect("fills exactly");
    assert_eq!(store.used_bytes().expect("usage"), 1024);
    // Lock file is present on disk but excluded from the tally.
    assert!(tmp.path().join("plugin-x").join(".write-lock").exists());
}

#[test]
fn state_store_ignores_orphan_tmp_files() {
    // Simulates a host crash between `fs::write(tmp)` and
    // `fs::rename(tmp, path)`: the orphan `<hash>.tmp` sits in
    // the dir but must not consume the quota. Otherwise long-
    // lived installs would slowly lose available state to every
    // interrupted write that ever happened.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("plugin-x");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let store = StateStore::new(dir.clone(), 1024);

    // Synthesise an orphan tmp (any filename ending in `.tmp`).
    std::fs::write(dir.join("deadbeef.tmp"), vec![0u8; 900]).expect("write orphan");
    // Quota tally must still be zero — the orphan is invisible.
    assert_eq!(store.used_bytes().expect("usage"), 0);

    // A 1 KiB write must succeed; the orphan does NOT count
    // against the cap.
    store.write("a", &vec![1u8; 1024]).expect("fills quota");
    assert_eq!(store.used_bytes().expect("usage"), 1024);
}

#[test]
fn state_store_serialises_concurrent_writes() {
    // Two threads racing the same key. With the lock + atomic
    // rename, both writes succeed and the post-condition is
    // ONE of the two values — never a truncated mix. Without
    // the serialisation, the quota check + write could
    // interleave and leave the store over quota.
    use std::sync::Arc as StdArc;
    use std::thread;

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StdArc::new(StateStore::new(tmp.path().join("plugin-x"), 8192));

    let store_a = StdArc::clone(&store);
    let store_b = StdArc::clone(&store);
    let h_a = thread::spawn(move || {
        for _ in 0..10 {
            store_a.write("shared", &vec![b'A'; 1000]).expect("write A");
        }
    });
    let h_b = thread::spawn(move || {
        for _ in 0..10 {
            store_b.write("shared", &vec![b'B'; 1000]).expect("write B");
        }
    });
    h_a.join().expect("thread A");
    h_b.join().expect("thread B");

    // Whichever thread wrote last wins. The post-condition we
    // verify is that the result is one of the two pure values —
    // never a mix from a torn write.
    let got = store.read("shared").expect("read").expect("present");
    assert!(got.len() == 1000);
    let is_pure_a = got.iter().all(|&b| b == b'A');
    let is_pure_b = got.iter().all(|&b| b == b'B');
    assert!(
        is_pure_a || is_pure_b,
        "concurrent write produced a torn value"
    );
    // Quota was 8192 — 10 writes per side × 1000 bytes serially
    // unsupervised would balloon to 20 000 bytes. With the lock,
    // the dir holds exactly one 1000-byte file at any time.
    assert_eq!(store.used_bytes().expect("usage"), 1000);
}

#[test]
fn state_store_rejects_nul_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = StateStore::new(tmp.path().join("plugin-x"), STATE_QUOTA_BYTES);
    match store.read("bad\0key") {
        Ok(_) => panic!("expected InvalidStateKey"),
        Err(HostError::InvalidStateKey(_)) => {}
        Err(err) => panic!("expected InvalidStateKey, got {err:?}"),
    }
    match store.write("bad\0key", b"x") {
        Ok(()) => panic!("expected InvalidStateKey"),
        Err(HostError::InvalidStateKey(_)) => {}
        Err(err) => panic!("expected InvalidStateKey, got {err:?}"),
    }
}
