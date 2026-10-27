use super::*;

#[test]
fn success_is_not_a_failure() {
    for status in [200, 201, 204, 299] {
        assert_eq!(classify_status(status), None, "status {status}");
    }
}

#[test]
fn credentials_get_their_own_class_so_a_refresh_can_be_tried() {
    assert_eq!(classify_status(401), Some(FailureKind::Unauthorized));
}

#[test]
fn a_malformed_payload_and_a_conflict_are_both_permanent() {
    // Different codes, different fixes, same verdict for the queue:
    // sending the same bytes again cannot help.
    assert_eq!(classify_status(422), Some(FailureKind::Permanent));
    assert_eq!(classify_status(409), Some(FailureKind::Permanent));
}

fn failure(status: u16, code: &str) -> RemoteFailure {
    RemoteFailure {
        kind: classify_status(status).unwrap(),
        status: Some(status),
        code: Some(code.into()),
        message: "…".into(),
    }
}

#[test]
fn the_two_meanings_of_409_are_told_apart_by_code_not_status() {
    // Confusing them is destructive both ways: an expired cursor
    // read as a conflict abandons a write that would have worked, a
    // conflict read as an expired cursor throws away a healthy
    // projection.
    let expired = failure(409, CODE_CURSOR_EXPIRED);
    assert!(expired.is_cursor_expired());
    assert!(!expired.is_conflict());

    let conflict = failure(409, CODE_CONFLICT);
    assert!(conflict.is_conflict());
    assert!(!conflict.is_cursor_expired());

    assert_eq!(expired.status, conflict.status, "the status cannot decide");
}

#[test]
fn a_failure_without_a_code_claims_neither_meaning() {
    // A body we could not parse must not be guessed into a
    // destructive branch.
    let opaque = RemoteFailure {
        kind: FailureKind::Permanent,
        status: Some(409),
        code: None,
        message: "no error body".into(),
    };
    assert!(!opaque.is_cursor_expired());
    assert!(!opaque.is_conflict());
}

#[test]
fn the_code_shows_up_when_a_failure_is_displayed() {
    assert_eq!(
        failure(409, CODE_CURSOR_EXPIRED).to_string(),
        "409 cursor_expired: …"
    );
}

#[test]
fn missing_and_forbidden_are_permanent() {
    assert_eq!(classify_status(403), Some(FailureKind::Permanent));
    assert_eq!(classify_status(404), Some(FailureKind::Permanent));
}

#[test]
fn congestion_and_server_faults_are_worth_retrying() {
    for status in [408, 429, 500, 502, 503, 504] {
        assert_eq!(
            classify_status(status),
            Some(FailureKind::Transient),
            "status {status}"
        );
    }
}

#[test]
fn a_mutation_carries_both_identity_headers() {
    let binding = RemoteBinding {
        server_url: "https://music.example".into(),
        identity: RemoteIdentity::Waveflow {
            account_id: "acc".into(),
            device_id: "dev".into(),
            cursor: 0,
        },
        active_library_id: None,
        bootstrapped_at: None,
    };
    let pair = TokenPair {
        access_token: "wfa_token".into(),
        refresh_token: "wfr_token".into(),
        expires_at: None,
        username: None,
    };
    let client = RemoteClient::from_parts(&binding, &pair).unwrap();

    let request = client
        .mutate(reqwest::Method::PUT, "/api/v2/favorites/track/t1", "op-1")
        .build()
        .unwrap();

    assert_eq!(request.headers()[OPERATION_ID_HEADER], "op-1");
    assert_eq!(request.headers()[DEVICE_ID_HEADER], "dev");
    assert_eq!(
        request.headers()[reqwest::header::AUTHORIZATION],
        "Bearer wfa_token"
    );
    assert_eq!(
        request.url().as_str(),
        "https://music.example/api/v2/favorites/track/t1"
    );
}

#[test]
fn a_read_carries_no_idempotency_headers() {
    let binding = RemoteBinding {
        server_url: "https://music.example/".into(),
        identity: RemoteIdentity::Waveflow {
            account_id: "acc".into(),
            device_id: "dev".into(),
            cursor: 0,
        },
        active_library_id: None,
        bootstrapped_at: None,
    };
    let pair = TokenPair {
        access_token: "wfa_token".into(),
        refresh_token: "wfr_token".into(),
        expires_at: None,
        username: None,
    };
    let client = RemoteClient::from_parts(&binding, &pair).unwrap();
    let request = client.get("/api/v2/sync/snapshot").build().unwrap();

    assert!(request.headers().get(OPERATION_ID_HEADER).is_none());
    assert!(request.headers().get(DEVICE_ID_HEADER).is_none());
    // The trailing slash on the base must not survive into the path.
    assert_eq!(
        request.url().as_str(),
        "https://music.example/api/v2/sync/snapshot"
    );
}

#[test]
fn a_plain_identifier_is_left_as_it_is() {
    let id = "31be1c98-3984-40ff-8352-73fc6e2c29b5";
    assert_eq!(segment(id).unwrap(), id);
    assert_eq!(segment("a.b_c~d").unwrap(), "a.b_c~d");
}

#[test]
fn a_separator_in_an_identifier_stays_inside_its_segment() {
    assert_eq!(segment("a/b").unwrap(), "a%2Fb");
    assert_eq!(segment("a?b=c").unwrap(), "a%3Fb%3Dc");
    assert_eq!(segment("a#b").unwrap(), "a%23b");
    assert_eq!(segment("../admin").unwrap(), "..%2Fadmin");
    assert_eq!(segment("50%").unwrap(), "50%25");
    assert_eq!(segment("é è").unwrap(), "%C3%A9%20%C3%A8");
    assert_eq!(segment("local:x").unwrap(), "local%3Ax");
}

#[test]
fn a_dot_segment_or_an_empty_identifier_is_refused() {
    for id in ["", ".", ".."] {
        assert_eq!(segment(id), Err(InvalidPathSegment), "{id:?}");
    }
    // Not a dot segment: a longer run of dots is an ordinary name.
    assert_eq!(segment("...").unwrap(), "...");
}

/// The property that matters, checked on the URL reqwest would send:
/// whatever the identifier, the request stays on the route it was
/// built for, and the query is the one the caller wrote.
#[test]
fn an_encoded_identifier_cannot_leave_its_route() {
    let base = "https://music.example";
    for id in [
        "a/b",
        "../../admin",
        "..%2F..%2Fadmin",
        "x?admin=1",
        "x#frag",
        "%2e%2e",
        "x/../../y",
        "\\..\\admin",
    ] {
        let path = format!("/api/v2/tracks/{}/lyrics", segment(id).unwrap());
        let url = reqwest::Url::parse(&format!("{base}{path}")).unwrap();
        let segments: Vec<_> = url.path_segments().unwrap().collect();
        assert_eq!(segments.len(), 5, "{id:?} -> {url}");
        assert_eq!(segments[..3], ["api", "v2", "tracks"], "{id:?} -> {url}");
        assert_eq!(segments[4], "lyrics", "{id:?} -> {url}");
        assert_eq!(url.query(), None, "{id:?} -> {url}");
        assert_eq!(url.fragment(), None, "{id:?} -> {url}");
        let decoded = percent_decode(segments[3]);
        assert_eq!(decoded, id, "{id:?} does not round-trip");
    }
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}
