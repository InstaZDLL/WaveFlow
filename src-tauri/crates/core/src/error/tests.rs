/// Minimal error whose `source()` we control, so the cause-chain
/// walk can be tested without conjuring a real `reqwest::Error`.
#[derive(Debug)]
struct Chained {
    msg: &'static str,
    source: Option<Box<Chained>>,
}

impl std::fmt::Display for Chained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.msg)
    }
}

impl std::error::Error for Chained {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

fn chain(msgs: &[&'static str]) -> Chained {
    let mut it = msgs.iter().rev();
    let last = Chained {
        msg: it.next().expect("at least one"),
        source: None,
    };
    it.fold(last, |acc, msg| Chained {
        msg,
        source: Some(Box::new(acc)),
    })
}

/// The whole point: the root cause has to survive into the string
/// the user pastes into a bug report (issue #452).
#[test]
fn describe_error_keeps_the_root_cause() {
    let err = chain(&[
        "error sending request for url (https://de1.api.radio-browser.info/json/…)",
        "client error (Connect)",
        "dns error",
        "failed to lookup address information",
    ]);
    let out = super::describe_error(&err);
    assert!(
        out.contains("failed to lookup address information"),
        "root cause lost: {out}"
    );
    assert!(out.starts_with("error sending request for url"));
}

#[test]
fn describe_error_handles_a_lone_error() {
    let err = chain(&["timed out"]);
    assert_eq!(super::describe_error(&err), "timed out");
}

/// `reqwest`/`hyper` often wrap an error in one that Displays
/// identically; echoing it would read "timed out: timed out".
#[test]
fn describe_error_skips_a_layer_that_restates_its_parent() {
    let err = chain(&["timed out", "timed out"]);
    assert_eq!(super::describe_error(&err), "timed out");
}

/// The dedup must fire on a repeat of the previous layer and
/// nothing else. Comparing against the accumulated buffer
/// instead would drop a distinct cause that merely happens to end
/// it — here "connect" is a suffix of "failed to connect", but it
/// is its own layer and has to survive.
#[test]
fn describe_error_only_dedups_a_repeat_of_the_previous_layer() {
    let err = chain(&["failed to connect", "connect"]);
    assert_eq!(super::describe_error(&err), "failed to connect: connect");
}

/// `source()` is author-controlled — a pathological chain must not
/// build an unbounded string.
#[test]
fn describe_error_caps_a_very_deep_chain() {
    let deep = vec!["layer"; 64];
    // Distinct messages, otherwise the dedup above hides the cap.
    let mut err = Chained {
        msg: "root",
        source: None,
    };
    for _ in 0..deep.len() {
        err = Chained {
            msg: "wrap",
            source: Some(Box::new(err)),
        };
    }
    let out = super::describe_error(&err);
    assert!(out.ends_with('\u{2026}'), "chain not capped: {out}");
    assert!(out.len() < 200, "chain not capped: {out}");
}

/// An error that prints its source after a colon and also returns
/// it from `source()` — thiserror's `{0}` over a `#[from]` field —
/// would otherwise say the cause twice.
#[test]
fn describe_error_skips_a_layer_its_parent_already_printed() {
    let err = chain(&[
        "http request failed: error sending request",
        "error sending request",
        "client error (Connect)",
    ]);
    assert_eq!(
        super::describe_error(&err),
        "http request failed: error sending request: client error (Connect)"
    );
}

// ----- describe_http_error ------------------------------------------

/// What every test request below targets: credentials in the
/// userinfo, a stream ticket in the path, a key in the query.
fn secret_url(origin: &str) -> String {
    format!("http://listener:hunter2@{origin}/api/v2/stream/tk_9f8e7d6c?key=s3cr3t#frag")
}

fn assert_no_secret(out: &str) {
    for (i, secret) in [
        "hunter2",
        "listener",
        "tk_9f8e7d6c",
        "s3cr3t",
        "/api/",
        "frag",
    ]
    .iter()
    .enumerate()
    {
        // By its index: the message must not carry the fixture
        // secret, nor the text that leaked it.
        assert!(
            !out.contains(secret),
            "fixture secret #{i} leaked into the message"
        );
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        // A system proxy would answer for the host itself and hide the
        // transport error these tests are about.
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

/// A port nobody listens on: the message must say the connection
/// was refused, not just that the request failed.
#[test]
fn describe_http_error_says_why_a_closed_port_failed() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let err = client()
        .get(secret_url(&format!("127.0.0.1:{port}")))
        .send()
        .expect_err("nothing listens on a dropped port");
    let out = super::describe_http_error(&err);
    assert!(out.starts_with("error sending request: "), "{out}");
    assert!(out.to_lowercase().contains("connect"), "cause lost: {out}");
    assert!(
        out.contains(&std::io::Error::from(std::io::ErrorKind::ConnectionRefused).to_string())
            || out.to_lowercase().contains("refused")
            || out.contains("os error"),
        "refusal lost: {out}"
    );
    assert_no_secret(&out);
}

/// A host that cannot resolve (`.invalid` is reserved for exactly
/// this, RFC 6761): the message must name the DNS failure.
#[test]
fn describe_http_error_says_why_an_unknown_host_failed() {
    // Its own deadline, far past the resolver's: on Windows the lookup
    // walks the DNS suffixes and LLMNR and takes over 11 s to fail, and
    // a request cut at `client()`'s 5 s would read "timed out" instead
    // of the DNS failure this test is about. The lookup always ends.
    let err = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("client")
        .get(secret_url("waveflow-test.invalid"))
        .send()
        .expect_err(".invalid never resolves");
    let out = super::describe_http_error(&err);
    assert!(out.contains("dns error"), "cause lost: {out}");
    assert_no_secret(&out);
}

/// A server that accepts and then says nothing: the message must
/// say the request timed out.
#[test]
fn describe_http_error_says_a_request_timed_out() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = listener.local_addr().expect("addr").to_string();
    let silent = std::thread::spawn(move || listener.accept().map(|(stream, _)| stream));
    let err = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_millis(300))
        .build()
        .expect("client")
        .get(secret_url(&origin))
        .send()
        .expect_err("the server never answers");
    let out = super::describe_http_error(&err);
    assert!(out.contains("timed out"), "cause lost: {out}");
    assert_no_secret(&out);
    drop(silent.join());
}

/// The URL can come back further down the chain: a redirect policy
/// that refuses names the target it refused. That target is cut
/// down to its origin like the request URL.
#[test]
fn describe_http_error_redacts_a_url_a_cause_prints() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = listener.local_addr().expect("addr").to_string();
    let target = secret_url(&origin);
    let location = target.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(reply.as_bytes()).expect("write");
    });
    let err = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let refused = format!("refused to follow ({})", attempt.url());
            attempt.error(refused)
        }))
        .build()
        .expect("client")
        .get(format!("http://{origin}/start"))
        .send()
        .expect_err("the policy refuses the redirect");
    server.join().expect("server");
    let out = super::describe_http_error(&err);
    assert!(
        out.contains(&format!("refused to follow (http://{origin})")),
        "{out}"
    );
    assert_no_secret(&out);
}

/// The walker for an error of a reqwest this crate cannot name: the
/// first layer still prints its URL, which comes out as its origin.
#[test]
fn describe_error_without_urls_keeps_the_cause_and_only_the_origin() {
    let err = chain(&[
        "error sending request for url (https://u:p@h.example/latest.json?k=s3cr3t)",
        "client error (Connect)",
        "dns error",
    ]);
    assert_eq!(
        super::describe_error_without_urls(&err),
        "error sending request for url (https://h.example): client error (Connect): dns error"
    );
}

#[test]
fn redact_urls_keeps_the_origin_and_the_prose_around_it() {
    assert_eq!(
        super::redact_urls("failed at (https://u:p@host.example:8443/a?b=c), retrying"),
        "failed at (https://host.example:8443), retrying"
    );
    assert_eq!(super::redact_urls("no url here"), "no url here");
    assert_eq!(
        super::redact_urls("for url (https://h.example/p?k=v): refused"),
        "for url (https://h.example): refused"
    );
    assert_eq!(
        super::redact_urls("«https://h.example/x"),
        "«https://h.example"
    );
    assert_eq!(super::redact_urls("bad http://[::1 tail"), "bad <url> tail");
    assert_eq!(
        super::redact_urls("line\nhttps://h.example/x\tend"),
        "line\nhttps://h.example\tend"
    );
}
