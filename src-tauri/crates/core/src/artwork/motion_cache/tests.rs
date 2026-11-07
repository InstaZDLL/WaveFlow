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

#[test]
fn reads_shorthand_ipv4_hosts_as_addresses() {
    // The WHATWG parser `reqwest` uses resolves every one of these to an
    // IPv4 address, so each meets the IP rules instead of passing as a name.
    for url in [
        // 127.0.0.1: decimal, hex, octal, two-part, three-part.
        "https://2130706433/a.mp4",
        "https://0x7f000001/a.mp4",
        "https://0X7F000001/a.mp4",
        "https://0177.0.0.1/a.mp4",
        "https://127.1/a.mp4",
        "https://127.0.1/a.mp4",
        "https://0x7f.1/a.mp4",
        "https://127.1./a.mp4",
        // 10.0.0.1 in two-part, hex and octal forms; 10.1.0.0 in three.
        "https://10.1/a.mp4",
        "https://0x0a.0.0.1/a.mp4",
        "https://012.0.0.1/a.mp4",
        "https://10.1.0/a.mp4",
        // 192.168.1.1: the last part fills the remaining 16 bits.
        "https://192.168.257/a.mp4",
        // 169.254.169.254, the cloud metadata service, in decimal.
        "https://2852039166/latest/meta-data",
        // 0.0.0.0: the unspecified address in one part.
        "https://0/a.mp4",
        // The same shorthands behind a port or a userinfo.
        "https://2130706433:8443/a.mp4",
        "https://user:pass@0x7f000001/a.mp4",
        // Full-width digits: IDNA maps them to ASCII, then to 127.0.0.1.
        "https://\u{ff11}\u{ff12}\u{ff17}.0.0.1/a.mp4",
    ] {
        assert!(!is_safe_motion_url(url), "{url} must be refused");
    }
    // A public address in shorthand is still an address, and allowed:
    // 134744072 is 8.8.8.8.
    assert!(is_safe_motion_url("https://134744072/a.mp4"));
    assert!(is_safe_motion_url("https://8.8.2056/a.mp4"));
}

#[test]
fn rejects_cgnat_this_network_and_broadcast() {
    for url in [
        // 100.64.0.0/10, carrier-grade NAT and mesh VPNs.
        "https://100.64.0.1/a.mp4",
        "https://100.127.255.254/a.mp4",
        // 100.100.100.100 in decimal.
        "https://1684300900/a.mp4",
        // 0.0.0.0/8, "this network".
        "https://0.1.2.3/a.mp4",
        // The limited broadcast, dotted and in decimal.
        "https://255.255.255.255/a.mp4",
        "https://4294967295/a.mp4",
    ] {
        assert!(!is_safe_motion_url(url), "{url} must be refused");
    }
    // Either side of the carrier-grade NAT range is public.
    assert!(is_safe_motion_url("https://100.63.255.255/a.mp4"));
    assert!(is_safe_motion_url("https://100.128.0.1/a.mp4"));
}

#[test]
fn keeps_names_that_look_numeric() {
    // A name whose last label is not a number is a name, resolved by DNS:
    // not an address in disguise.
    for url in [
        "https://1e100.net/a.mp4",
        "https://0x7f.example.com/a.mp4",
        "https://127.0.0.1.nip.io/a.mp4",
        "https://123.example/a.mp4",
    ] {
        assert!(is_safe_motion_url(url), "{url} must be allowed");
    }
}

#[test]
fn judges_the_host_the_connection_reaches() {
    // A backslash ends the authority for an https URL, so the host is
    // 127.0.0.1 and `example.com` is part of the path.
    assert!(!is_safe_motion_url(r"https://127.0.0.1\@example.com/a.mp4"));
    // A userinfo is dropped: the host after the `@` is judged.
    assert!(!is_safe_motion_url("https://user@127.0.0.1./a.mp4"));
    assert!(!is_safe_motion_url("https://example.com@127.0.0.1/a.mp4"));
    assert!(is_safe_motion_url("https://127.0.0.1@example.com/a.mp4"));
    // Tab and newline are stripped before parsing.
    assert!(!is_safe_motion_url("https://127.0.0.\n1/a.mp4"));
    assert!(!is_safe_motion_url("https://127.0.\t0.1/a.mp4"));
    // Case and IDNA: these are all `localhost`.
    assert!(!is_safe_motion_url("HTTPS://LOCALHOST/a.mp4"));
    assert!(!is_safe_motion_url("https://LocalHost./a.mp4"));
    assert!(!is_safe_motion_url("https://a.b.localhost/a.mp4"));
    assert!(!is_safe_motion_url("https://\u{24db}ocalhost/a.mp4"));
    // Nothing a fetch could open.
    assert!(!is_safe_motion_url(""));
    assert!(!is_safe_motion_url("https://"));
    assert!(!is_safe_motion_url("not a url"));
    // Extra slashes after an https scheme are skipped: the host is the
    // first segment, here 127.0.0.1.
    assert!(!is_safe_motion_url("https:///127.0.0.1/a.mp4"));
    assert!(!is_safe_motion_url(r"https:\\127.1\a.mp4"));
    // A trailing-dot name that is only dots.
    assert!(!is_safe_motion_url("https://./a.mp4"));
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
