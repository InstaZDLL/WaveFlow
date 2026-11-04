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

// Offline mode, against a real profile database and a server that counts what
// reaches it.

const PROFILE: i64 = 7;
/// Long gone, so `try_build_for` refreshes before anything else.
const LAPSED: Option<i64> = Some(1);

/// Answers the refresh with a fresh pair. The other route is the toggle in
/// Settings flipped while a request is in flight: offline from here on, and a
/// `401` for the request that was already out.
async fn answer(
    axum::extract::State(seen): axum::extract::State<Arc<std::sync::Mutex<Vec<String>>>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = request.uri().path().to_owned();
    seen.lock().unwrap().push(path.clone());
    match path.as_str() {
        "/api/v2/auth/refresh" => axum::Json(serde_json::json!({
            "access_token": "wfa_rotated",
            "refresh_token": "wfr_rotated",
            "expires_in": 3600,
            "device_id": "dev",
            "user": { "id": "acc", "username": "alice" },
        }))
        .into_response(),
        "/goes-offline" => {
            offline::flip_on_this_thread(true);
            axum::http::StatusCode::UNAUTHORIZED.into_response()
        }
        _ => axum::http::StatusCode::OK.into_response(),
    }
}

struct Bench {
    /// Never dropped: its plugin runtime holds a blocking HTTP client, and that
    /// cannot be dropped from inside an async test.
    state: std::mem::ManuallyDrop<AppState>,
    binding: RemoteBinding,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

impl Bench {
    /// A profile bound to the local server and signed in, its access token
    /// lasting until `expires_at`.
    async fn new(expires_at: Option<i64>) -> Self {
        use sqlx::sqlite::SqliteConnectOptions;
        use std::str::FromStr;

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_url = format!("http://{}", listener.local_addr().unwrap());
        let router = axum::Router::new()
            .fallback(answer)
            .with_state(Arc::clone(&seen));
        tokio::spawn(async move { axum::serve(listener, router).await });

        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::from_str(&format!(
            "sqlite://{}",
            dir.path().join("profile.db").display()
        ))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();

        let binding = RemoteBinding {
            server_url,
            identity: RemoteIdentity::Waveflow {
                account_id: "acc".into(),
                device_id: "dev".into(),
                cursor: 0,
            },
            active_library_id: None,
            bootstrapped_at: None,
        };
        {
            let mut conn = pool.acquire().await.unwrap();
            binding::write(&mut conn, &binding).await.unwrap();
            tokens::write(&mut conn, &signed_in(expires_at))
                .await
                .unwrap();
        }

        let app_db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let state = AppState::for_test(dir.path().to_path_buf(), app_db).await;
        *state.profile.write().await = Some(crate::state::ActiveProfile::for_test(PROFILE, pool));
        Self {
            state: std::mem::ManuallyDrop::new(state),
            binding,
            seen,
            _dir: dir,
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    async fn stored(&self) -> TokenPair {
        let pool = self
            .state
            .require_profile_pool_for(Some(PROFILE))
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        tokens::read(&mut conn).await.unwrap().unwrap()
    }

    /// A refresh by another task, already stored.
    async fn store_rotated(&self) {
        let pool = self
            .state
            .require_profile_pool_for(Some(PROFILE))
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        tokens::write(
            &mut conn,
            &TokenPair {
                access_token: "wfa_theirs".into(),
                refresh_token: "wfr_theirs".into(),
                expires_at: None,
                username: None,
            },
        )
        .await
        .unwrap();
    }
}

fn signed_in(expires_at: Option<i64>) -> TokenPair {
    TokenPair {
        access_token: "wfa_old".into(),
        refresh_token: "wfr_old".into(),
        expires_at,
        username: Some("alice".into()),
    }
}

/// The control for the tests below: the same bench, online, does reach the
/// server, so a count of zero there means something.
#[tokio::test]
async fn online_a_lapsed_token_is_refreshed_and_stored() {
    let bench = Bench::new(LAPSED).await;
    let _online = offline::scoped_to_this_thread(false);
    let built = RemoteClient::try_build_for(&bench.state, PROFILE).await;
    assert!(built.is_ok_and(|client| client.is_some()));
    assert_eq!(bench.seen(), ["/api/v2/auth/refresh"]);
    assert_eq!(bench.stored().await.refresh_token, "wfr_rotated");
}

#[tokio::test]
async fn offline_a_lapsed_token_sends_nothing() {
    let bench = Bench::new(LAPSED).await;
    let _offline = offline::scoped_to_this_thread(true);
    let built = RemoteClient::try_build_for(&bench.state, PROFILE).await;
    assert_eq!(bench.seen(), Vec::<String>::new());
    assert!(built.is_err());
    assert_eq!(bench.stored().await, signed_in(LAPSED));
}

/// The path a person can reach: "Sync now" in Settings tests nothing before it
/// builds its client.
#[tokio::test]
async fn offline_syncing_with_a_lapsed_token_sends_nothing() {
    let bench = Bench::new(LAPSED).await;
    let _offline = offline::scoped_to_this_thread(true);
    let synced = crate::remote::sync::sync_now(&bench.state).await;
    assert_eq!(bench.seen(), Vec::<String>::new());
    assert!(synced.is_err());
    assert_eq!(bench.stored().await, signed_in(LAPSED));
}

/// Adopting what another task stored asks nothing of the network, so offline
/// does not stand in its way.
#[tokio::test]
async fn offline_a_pair_already_rotated_is_still_adopted() {
    let bench = Bench::new(LAPSED).await;
    bench.store_rotated().await;
    let _offline = offline::scoped_to_this_thread(true);
    let adopted = refresh(&bench.state, PROFILE, &bench.binding, &signed_in(LAPSED))
        .await
        .unwrap();
    assert_eq!(adopted.refresh_token, "wfr_theirs");
    assert_eq!(bench.seen(), Vec::<String>::new());
}

/// Offline since the request left: the `401` it comes back with spends no
/// refresh token.
#[tokio::test]
async fn offline_since_the_request_left_a_401_does_not_refresh() {
    let bench = Bench::new(None).await;
    let _online = offline::scoped_to_this_thread(false);
    let client = RemoteClient::try_build_for(&bench.state, PROFILE)
        .await
        .unwrap()
        .unwrap();
    let sent = client.send_ok(client.get("/goes-offline")).await;
    assert_eq!(bench.seen(), ["/goes-offline"]);
    assert!(sent.is_err());
    assert_eq!(bench.stored().await, signed_in(None));
}

/// Nor is the request sent a second time, even with a fresh token already in
/// the database to send it with. A replay is a request like any other.
#[tokio::test]
async fn offline_since_the_request_left_it_is_not_replayed() {
    let bench = Bench::new(None).await;
    let _online = offline::scoped_to_this_thread(false);
    let client = RemoteClient::try_build_for(&bench.state, PROFILE)
        .await
        .unwrap()
        .unwrap();
    bench.store_rotated().await;
    let failure = client
        .send_ok(client.get("/goes-offline"))
        .await
        .unwrap_err();
    assert_eq!(bench.seen(), ["/goes-offline"]);
    assert_eq!(failure.kind, FailureKind::Transient);
}
