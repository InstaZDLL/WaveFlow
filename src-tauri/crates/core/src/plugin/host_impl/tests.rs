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

/// Allowlist built from `patterns` alone.
fn http_permissions(patterns: &[&str]) -> HostPermissions {
    let m = manifest_with_permissions(Permissions {
        http: patterns.iter().map(|p| (*p).to_string()).collect(),
        storage_read: false,
        storage_state: false,
        library_read_artists: false,
    });
    HostPermissions::from_manifest(&m).expect("compile allowlist")
}

const RADIO: &str = "https://*.radio-browser.info/**";

#[test]
fn http_allowlist_wildcard_cannot_cross_into_path_or_query() {
    // The old whole-string glob let `*` run over `/` and `?`, so
    // the host it "matched" sat in the path or the query.
    let p = http_permissions(&[RADIO]);
    assert!(!p.http_allowed("https://evil.com/?.radio-browser.info/a"));
    assert!(!p.http_allowed("https://evil.com/.radio-browser.info/a"));
    assert!(!p.http_allowed("https://evil.com#.radio-browser.info/a"));
    assert!(!p.http_allowed("https://evil.com/x?y=.radio-browser.info/"));
}

#[test]
fn http_allowlist_judges_the_host_reqwest_connects_to() {
    // The parser reads `\` as `/`: this connects to 127.0.0.1.
    let p = http_permissions(&[RADIO]);
    assert!(!p.http_allowed(r"https://127.0.0.1\@x.radio-browser.info/a"));
    assert!(!p.http_allowed(r"https://127.0.0.1\x.radio-browser.info/a"));
    // Spelled with a plain slash, the same host is still refused.
    assert!(!p.http_allowed("https://127.0.0.1/x.radio-browser.info/a"));
}

#[test]
fn http_allowlist_refuses_shorthand_ipv4() {
    // Even against a pattern that names the address, a shorthand
    // spelling says one thing and connects to another.
    let p = http_permissions(&["https://127.0.0.1/**", RADIO]);
    assert!(p.http_allowed("https://127.0.0.1/a"));
    for url in [
        "https://2130706433/a",
        "https://0x7f000001/a",
        "https://0177.0.0.1/a",
        "https://127.1/a",
        "https://0x7f000001/x.radio-browser.info/a",
    ] {
        assert!(!p.http_allowed(url), "{url} must be refused");
    }
}

#[test]
fn http_allowlist_refuses_user_info() {
    let p = http_permissions(&[RADIO]);
    assert!(!p.http_allowed("https://user@de1.api.radio-browser.info/json"));
    assert!(!p.http_allowed("https://user:pw@de1.api.radio-browser.info/json"));
    assert!(!p.http_allowed("https://:pw@de1.api.radio-browser.info/json"));
    // The host is what follows the `@`.
    assert!(!p.http_allowed("https://de1.api.radio-browser.info@evil.com/json"));
}

#[test]
fn http_allowlist_refuses_a_url_written_otherwise_than_read() {
    let p = http_permissions(&[RADIO]);
    // The parser skips the third slash and reads a host there.
    assert!(!p.http_allowed("https:///x.radio-browser.info"));
    assert!(!p.http_allowed("https:///de1.api.radio-browser.info/json"));
    assert!(!p.http_allowed(r"https:\\de1.api.radio-browser.info/json"));
    // Whitespace the parser strips.
    assert!(!p.http_allowed(" https://de1.api.radio-browser.info/json"));
    assert!(!p.http_allowed("https://de1.api.radio-\tbrowser.info/json"));
    // Percent-encoding in the host.
    assert!(!p.http_allowed("https://de1.api.radio-browser.%69nfo/json"));
    // Unparsable.
    assert!(!p.http_allowed("not a url"));
    assert!(!p.http_allowed("https://"));
}

#[test]
fn http_allowlist_refuses_another_scheme() {
    let p = http_permissions(&[RADIO]);
    assert!(!p.http_allowed("http://de1.api.radio-browser.info/json/stations"));
    assert!(!p.http_allowed("ftp://de1.api.radio-browser.info/json/stations"));
    assert!(!p.http_allowed("wss://de1.api.radio-browser.info/json/stations"));
}

#[test]
fn http_allowlist_wildcard_host_covers_whole_labels_only() {
    let p = http_permissions(&[RADIO]);
    assert!(p.http_allowed("https://a.b.api.radio-browser.info/json"));
    assert!(p.http_allowed("https://DE1.API.Radio-Browser.info/json"));
    // The suffix alone, a look-alike, an empty label, a trailing dot.
    assert!(!p.http_allowed("https://radio-browser.info/json"));
    assert!(!p.http_allowed("https://evilradio-browser.info/json"));
    assert!(!p.http_allowed("https://radio-browser.info.evil.com/json"));
    assert!(!p.http_allowed("https://a..radio-browser.info/json"));
    assert!(!p.http_allowed("https://de1.api.radio-browser.info./json"));
}

#[test]
fn http_allowlist_checks_the_port() {
    let p = http_permissions(&[RADIO, "http://localhost:8080/**"]);
    assert!(!p.http_allowed("https://de1.api.radio-browser.info:8443/json"));
    // The default port written out is refused: it is not how the
    // URL parses, and no plugin needs it.
    assert!(!p.http_allowed("https://de1.api.radio-browser.info:443/json"));
    assert!(p.http_allowed("http://localhost:8080/x"));
    assert!(!p.http_allowed("http://localhost/x"));
    assert!(!p.http_allowed("http://localhost:8081/x"));
}

#[test]
fn http_allowlist_path_is_matched_after_dot_segments() {
    let p = http_permissions(&["https://music.163.com/api/**", "https://example.com/a/*"]);
    assert!(p.http_allowed("https://music.163.com/api/search/get?s=x"));
    assert!(!p.http_allowed("https://music.163.com/api/../weapi/x"));
    assert!(!p.http_allowed("https://music.163.com/api/%2e%2e/weapi/x"));
    assert!(!p.http_allowed("https://music.163.com/apix/y"));
    // A single `*` stays inside one segment.
    assert!(p.http_allowed("https://example.com/a/b"));
    assert!(!p.http_allowed("https://example.com/a/b/c"));
}

#[test]
fn http_allowlist_keeps_every_published_pattern_working() {
    // Every `permissions.http` pattern a published plugin declares
    // (the registry, the bundled Web Radio, the private ones), with
    // a request of the kind it makes.
    let cases: &[(&str, &[&str])] = &[
        (
            "https://*.api.radio-browser.info/**",
            &[
                "https://all.api.radio-browser.info/json/servers",
                "https://de1.api.radio-browser.info/json/stations/search?name=x&limit=50",
            ],
        ),
        (
            "https://radio-browser.info/**",
            &["https://radio-browser.info/", "https://radio-browser.info"],
        ),
        (
            "https://musicbrainz.org/**",
            &["https://musicbrainz.org/ws/2/release-group?artist=abc&fmt=json"],
        ),
        (
            "https://amp-api.music.apple.com/**",
            &["https://amp-api.music.apple.com/v1/catalog/us/songs/1/lyrics?l=en"],
        ),
        (
            "https://music.apple.com/**",
            &["https://music.apple.com/us/browse"],
        ),
        (
            "https://itunes.apple.com/**",
            &["https://itunes.apple.com/search?term=a+b&entity=album"],
        ),
        (
            "https://mvod.itunes.apple.com/**",
            &["https://mvod.itunes.apple.com/itunes-assets/x/y.m3u8"],
        ),
        ("https://lrc.red/**", &["https://lrc.red/api/search?q=x"]),
        (
            "https://lyrics.kugou.com/**",
            &["https://lyrics.kugou.com/download?ver=1&id=1&accesskey=k&fmt=krc"],
        ),
        (
            "https://music.163.com/api/**",
            &["https://music.163.com/api/song/lyric?id=1&lv=1&kv=1&tv=-1"],
        ),
        (
            "https://u.y.qq.com/cgi-bin/**",
            &["https://u.y.qq.com/cgi-bin/musicu.fcg?data=%7B%7D"],
        ),
        (
            "https://c.y.qq.com/soso/fcgi-bin/**",
            &["https://c.y.qq.com/soso/fcgi-bin/client_search_cp?w=x"],
        ),
        (
            "https://api.tidal.com/**",
            &["https://api.tidal.com/v1/search/albums?query=x&countryCode=US"],
        ),
        (
            "https://open.spotify.com/**",
            &["https://open.spotify.com/get_access_token?reason=init"],
        ),
        (
            "https://clienttoken.spotify.com/**",
            &["https://clienttoken.spotify.com/v1/clienttoken"],
        ),
        (
            "https://api-partner.spotify.com/**",
            &["https://api-partner.spotify.com/pathfinder/v1/query"],
        ),
        (
            "https://gew1-spclient.spotify.com/**",
            &["https://gew1-spclient.spotify.com/canvaz-cache/v0/canvases"],
        ),
        (
            "https://code.thetadev.de/**",
            &["https://code.thetadev.de/ThetaDev/spotify-secrets/raw/branch/main/x.json"],
        ),
    ];
    for (pattern, urls) in cases {
        let p = http_permissions(&[pattern]);
        for url in *urls {
            assert!(p.http_allowed(url), "{pattern} must allow {url}");
        }
    }
    // And a path a narrower pattern does not cover stays refused.
    let p = http_permissions(&["https://u.y.qq.com/cgi-bin/**"]);
    assert!(!p.http_allowed("https://u.y.qq.com/other"));
}

#[test]
fn http_allowlist_returns_the_url_it_judged() {
    let p = http_permissions(&[RADIO]);
    let url = p
        .allowed_http_url("https://DE1.api.radio-browser.info/json/a/../stations")
        .expect("allowed");
    assert_eq!(url.host_str(), Some("de1.api.radio-browser.info"));
    assert_eq!(url.path(), "/json/stations");
}

#[test]
fn http_allowlist_refuses_unreadable_patterns() {
    for pattern in [
        "https://*evil.com/**",
        "https://api.*.com/**",
        "https://*.*.example.com/**",
        "https://*.127.0.0.1/**",
        "ftp://example.com/**",
        "example.com/**",
        "https://example.com:abc/**",
        "https://user@example.com/**",
        r"https://example.com\@x/**",
        "https:///path/**",
        "https://example.com/a#b",
        "https://example.com/search?q=*",
        "https://example.com/[",
    ] {
        let m = manifest_with_permissions(Permissions {
            http: vec![pattern.into()],
            storage_read: false,
            storage_state: false,
            library_read_artists: false,
        });
        assert!(
            matches!(
                HostPermissions::from_manifest(&m),
                Err(HostError::InvalidPattern { .. })
            ),
            "{pattern} must be refused"
        );
    }
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
