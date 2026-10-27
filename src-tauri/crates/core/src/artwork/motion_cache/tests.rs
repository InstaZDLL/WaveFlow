use super::*;

#[test]
fn rejects_internal_and_non_https_urls() {
    assert!(is_safe_motion_url(
        "https://mvod.itunes.apple.com/itunes-assets/x.mp4"
    ));
    assert!(is_safe_motion_url("https://example.com:443/a.mp4"));
    // scheme
    assert!(!is_safe_motion_url("http://example.com/a.mp4"));
    assert!(!is_safe_motion_url("file:///etc/passwd"));
    // localhost / loopback
    assert!(!is_safe_motion_url("https://localhost/a.mp4"));
    assert!(!is_safe_motion_url("https://127.0.0.1/a.mp4"));
    assert!(!is_safe_motion_url("https://[::1]:8443/a.mp4"));
    // Trailing FQDN dot must not bypass the loopback checks — these
    // resolve to the same targets as above.
    assert!(!is_safe_motion_url("https://localhost./a.mp4"));
    assert!(!is_safe_motion_url("https://127.0.0.1./a.mp4"));
    // private / link-local / cloud-metadata
    assert!(!is_safe_motion_url("https://10.0.0.5/a.mp4"));
    assert!(!is_safe_motion_url("https://192.168.1.1/a.mp4"));
    assert!(!is_safe_motion_url("https://172.16.0.1/a.mp4"));
    assert!(!is_safe_motion_url(
        "https://169.254.169.254/latest/meta-data"
    ));
    assert!(!is_safe_motion_url("https://[fd00::1]/a.mp4"));
    // IPv4-mapped (`::ffff:a.b.c.d`) + IPv4-compatible (`::a.b.c.d`) IPv6
    // must not slip past the v6 branch.
    assert!(!is_safe_motion_url("https://[::ffff:127.0.0.1]/a.mp4"));
    assert!(!is_safe_motion_url("https://[::ffff:10.0.0.1]/a.mp4"));
    assert!(!is_safe_motion_url(
        "https://[::ffff:169.254.169.254]/a.mp4"
    ));
    assert!(!is_safe_motion_url("https://[::127.0.0.1]/a.mp4"));
    assert!(!is_safe_motion_url("https://[::192.168.0.1]/a.mp4"));
}

#[tokio::test]
async fn cache_mp4_refuses_unsafe_url_before_network() {
    // The in-function guard rejects an unsafe initial URL before any
    // filesystem/network work — no reliance on the caller validating it —
    // and flags it `UnsafeUrl` (a security rejection) so the caller skips
    // rather than streaming the raw url.
    let tmp = tempfile::tempdir().expect("tempdir");
    for url in ["https://127.0.0.1/x.mp4", "http://example.com/x.mp4"] {
        let err = cache_mp4(tmp.path(), url, DEFAULT_MAX_CACHE_BYTES)
            .await
            .expect_err("unsafe url must be refused");
        assert!(
            matches!(err, CacheError::UnsafeUrl),
            "{url} must be a security failure, got {err:?}"
        );
    }
    // The guard rejected before touching the filesystem: no `.mp4` cached
    // and no `.part` temp staged.
    let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
        .expect("read tempdir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "guard must reject before any file is created; found {leftovers:?}"
    );
}

#[test]
fn redirect_decision_allows_ten_hops_then_rejects() {
    let safe = "https://cdn.example.com/a.mp4";
    // Up to 10 previously-followed hops with a safe target: keep following.
    for hops in 0..=10 {
        assert_eq!(
            redirect_decision(hops, safe),
            RedirectDecision::Follow,
            "hop {hops} within budget must follow"
        );
    }
    // The 11th redirect (and beyond) is refused regardless of target.
    assert_eq!(redirect_decision(11, safe), RedirectDecision::TooMany);
    assert_eq!(redirect_decision(50, safe), RedirectDecision::TooMany);
    // A safe hop count but an internal target is refused as unsafe...
    assert_eq!(
        redirect_decision(0, "https://127.0.0.1/a.mp4"),
        RedirectDecision::Unsafe
    );
    // ...and the hop limit takes precedence over the SSRF check (both
    // fail closed, but an over-long chain reports `TooMany`).
    assert_eq!(
        redirect_decision(11, "https://127.0.0.1/a.mp4"),
        RedirectDecision::TooMany
    );
}

#[test]
fn redact_url_drops_userinfo_query_and_fragment() {
    // Credentials / signed tokens must never reach a logged error string.
    assert_eq!(
        redact_url("https://user:pass@cdn.example.com/a.mp4?sig=secret#frag"),
        "https://cdn.example.com/a.mp4"
    );
    assert_eq!(
        redact_url("https://cdn.example.com/a.mp4"),
        "https://cdn.example.com/a.mp4"
    );
    assert_eq!(
        redact_url("https://cdn.example.com"),
        "https://cdn.example.com"
    );
}

#[test]
fn ipv4_embedded_loopback_is_internal() {
    use std::net::Ipv6Addr;
    let internal = [
        "::ffff:127.0.0.1",   // mapped loopback
        "::ffff:192.168.0.1", // mapped private
        "::127.0.0.1",        // compatible loopback
        "::192.168.0.1",      // compatible private
    ];
    for s in internal {
        assert!(
            is_internal_ip(s.parse::<Ipv6Addr>().unwrap().into()),
            "{s} should be classified internal"
        );
    }
    // A genuine global v6 address is not internal.
    assert!(!is_internal_ip(
        "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap().into()
    ));
}
