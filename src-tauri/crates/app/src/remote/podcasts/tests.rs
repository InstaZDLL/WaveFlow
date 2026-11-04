//! The server's podcasts against a fake server that answers what the test says, and
//! the binding on the really migrated schema.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::super::binding::{self, RemoteBinding, RemoteIdentity};
use super::super::stream::mint_ticket;
use super::super::test_support::scope_on;
use super::super::tokens::{self, TokenPair};
use super::*;

/// A token in a private feed's address, which no answer of ours may carry on.
const FEED_TOKEN: &str = "feed-token-5f1c";

struct Seen {
    method: String,
    path: String,
    head: String,
    body: String,
}

/// Answers by `METHOD path`, and 404 otherwise. Keeps what it was asked.
struct FakeServer {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeServer {
    fn seen_paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|seen| format!("{} {}", seen.method, seen.path))
            .collect()
    }
}

async fn fake_server(routes: &[(&str, u16, &str)]) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let routes: Arc<HashMap<String, (u16, String)>> = Arc::new(
        routes
            .iter()
            .map(|(route, status, body)| ((*route).to_owned(), (*status, (*body).to_owned())))
            .collect(),
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (routes, log) = (Arc::clone(&routes), Arc::clone(&log));
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = [0u8; 2048];
                let head_end = loop {
                    if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => raw.extend_from_slice(&buffer[..read]),
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while raw.len() < head_end + length {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => raw.extend_from_slice(&buffer[..read]),
                    }
                }
                let mut parts = head.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let body = String::from_utf8_lossy(&raw[head_end..]).into_owned();
                let (status, answer) =
                    routes.get(&format!("{method} {path}")).cloned().unwrap_or((
                        404,
                        r#"{"code":"not_found","message":"Not found"}"#.to_owned(),
                    ));
                log.lock().unwrap().push(Seen {
                    method,
                    path,
                    head,
                    body,
                });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    FakeServer { base, seen }
}

fn binding_of(server_url: &str, account_id: &str) -> RemoteBinding {
    RemoteBinding {
        server_url: server_url.to_owned(),
        identity: RemoteIdentity::Waveflow {
            account_id: account_id.to_owned(),
            device_id: "dev-1".to_owned(),
            cursor: 0,
        },
        active_library_id: None,
        bootstrapped_at: None,
    }
}

fn tokens_pair() -> TokenPair {
    TokenPair {
        access_token: "access-token".to_owned(),
        refresh_token: "refresh-token".to_owned(),
        expires_at: None,
        username: None,
    }
}

fn client_for<'a>(server: &FakeServer) -> RemoteClient<'a> {
    RemoteClient::from_parts(&binding_of(&server.base, "acc-1"), &tokens_pair()).unwrap()
}

/// What a manager's overview looks like, with a private feed in it.
fn manager_overview(enabled: bool) -> String {
    format!(
        r#"{{"enabled":{enabled},"can_manage":true,"channels":[{{
            "id":"7a1f7f7e-0000-4000-8000-000000000001",
            "url":"https://feeds.example.org/rss/{FEED_TOKEN}.xml?auth={FEED_TOKEN}",
            "title":"Private show",
            "description":"About things",
            "site_url":"https://example.org/{FEED_TOKEN}",
            "image_url":"https://example.org/{FEED_TOKEN}.jpg",
            "artwork_hash":"abc123",
            "status":"completed",
            "error":null,
            "created_at":1,
            "refreshed_at":2,
            "episode_count":1
        }}]}}"#
    )
}

fn episode_json(id: &str, status: &str, position_ms: &str) -> String {
    format!(
        r#"{{"id":"{id}","channel_id":"c1","channel_title":"Show","artwork_hash":null,
            "title":"Ep {id}","description":null,"published_at":5,"duration_ms":600000,
            "status":"{status}","error":null,"size":null,"suffix":"mp3",
            "content_type":"audio/mpeg","position_ms":{position_ms},"created_at":1}}"#
    )
}

fn failure_of(status: u16, code: &str, message: &str) -> RemoteFailure {
    RemoteFailure {
        kind: crate::remote::client::classify_status(status).unwrap(),
        status: Some(status),
        code: Some(code.to_owned()),
        message: message.to_owned(),
    }
}

// ── reading ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_overview_reads_and_carries_no_address_on() {
    let body = manager_overview(true);
    let server = fake_server(&[("GET /api/v2/podcasts", 200, &body)]).await;
    let overview = overview(&client_for(&server)).await.unwrap();
    assert!(overview.enabled);
    assert!(overview.can_manage);
    assert_eq!(overview.channels[0].title, "Private show");

    // The answer held the token three times. What goes on to the webview and to a
    // logger has nowhere to keep it.
    assert!(body.contains(FEED_TOKEN));
    let serialized = serde_json::to_string(&overview).unwrap();
    let printed = format!("{overview:?}");
    for output in [serialized, printed] {
        assert!(!output.contains(FEED_TOKEN), "{output}");
        assert!(!output.contains("example.org"), "{output}");
    }
}

#[tokio::test]
async fn a_server_with_podcasts_off_says_so() {
    let body = manager_overview(false);
    let server = fake_server(&[("GET /api/v2/podcasts", 200, &body)]).await;
    assert!(visible(overview(&client_for(&server)).await).is_none());
}

#[tokio::test]
async fn a_server_with_podcasts_on_shows() {
    let body = manager_overview(true);
    let server = fake_server(&[("GET /api/v2/podcasts", 200, &body)]).await;
    assert!(visible(overview(&client_for(&server)).await).is_some());
}

#[tokio::test]
async fn nothing_that_goes_wrong_reaching_the_overview_is_more_than_an_absent_section() {
    for (status, body) in [
        (401, r#"{"code":"unauthorized","message":"Nope"}"#),
        (404, r#"{"code":"not_found","message":"Not found"}"#),
        (500, r#"{"code":"internal","message":"Boom"}"#),
        (200, "<html>a proxy's page</html>"),
    ] {
        let server = fake_server(&[("GET /api/v2/podcasts", status, body)]).await;
        assert!(
            visible(overview(&client_for(&server)).await).is_none(),
            "{status}"
        );
    }
    // Nobody home.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", closed.local_addr().unwrap().port());
    drop(closed);
    let client = RemoteClient::from_parts(&binding_of(&base, "acc-1"), &tokens_pair()).unwrap();
    assert!(visible(overview(&client).await).is_none());
}

#[tokio::test]
async fn a_channel_reads_with_its_episodes_and_each_position() {
    let body = format!(
        r#"{{"id":"c1","url":null,"title":"Show","description":null,"site_url":null,
             "image_url":null,"artwork_hash":null,"status":"completed","error":null,
             "created_at":1,"refreshed_at":null,"episode_count":2,
             "episodes":[{},{}]}}"#,
        episode_json("e1", "completed", "90000"),
        episode_json("e2", "new", "null"),
    );
    let server = fake_server(&[("GET /api/v2/podcasts/c1", 200, &body)]).await;
    let read = channel(&client_for(&server), "c1").await.unwrap();
    let episodes = read.episodes.unwrap();
    assert_eq!(episodes[0].position_ms, Some(90_000));
    assert!(episodes[0].is_stored());
    assert_eq!(episodes[1].position_ms, None);
    assert!(!episodes[1].is_stored());
}

// ── identifiers ─────────────────────────────────────────────────────

#[tokio::test]
async fn an_id_with_a_separator_stays_one_segment_on_every_route() {
    let server = fake_server(&[]).await;
    let client = client_for(&server);
    let id = "a/../b?x=1#c";
    let encoded = "a%2F..%2Fb%3Fx%3D1%23c";
    let _ = channel(&client, id).await;
    let _ = episode(&client, id).await;
    let _ = delete_channel(&client, id).await;
    let _ = request_download(&client, id).await;
    let _ = delete_episode(&client, id).await;
    let _ = save_position(&client, id, 1).await;
    let _ = clear_position(&client, id).await;
    let _ = mint_ticket(&client, id).await;
    assert_eq!(
        server.seen_paths(),
        vec![
            format!("GET /api/v2/podcasts/{encoded}"),
            format!("GET /api/v2/podcast-episodes/{encoded}"),
            format!("DELETE /api/v2/podcasts/{encoded}"),
            format!("POST /api/v2/podcast-episodes/{encoded}/download"),
            format!("DELETE /api/v2/podcast-episodes/{encoded}"),
            format!("PUT /api/v2/bookmarks/{encoded}"),
            format!("DELETE /api/v2/bookmarks/{encoded}"),
            format!("POST /api/v2/tracks/{encoded}/stream-ticket"),
        ]
    );
}

#[tokio::test]
async fn a_dot_segment_is_refused_before_anybody_is_asked() {
    let server = fake_server(&[]).await;
    let client = client_for(&server);
    for id in ["", ".", ".."] {
        assert!(channel(&client, id).await.is_err());
        assert!(episode(&client, id).await.is_err());
        assert!(save_position(&client, id, 1).await.is_err());
        assert!(clear_position(&client, id).await.is_err());
        assert!(delete_channel(&client, id).await.is_err());
        assert!(request_download(&client, id).await.is_err());
        assert!(delete_episode(&client, id).await.is_err());
        assert!(mint_ticket(&client, id).await.is_err());
    }
    assert!(server.seen_paths().is_empty());
}

// ── playing ─────────────────────────────────────────────────────────

#[tokio::test]
async fn an_episode_with_no_file_gets_no_ticket_and_the_refusal_says_where_it_stands() {
    let server = fake_server(&[]).await;
    let failure = mint_ticket(&client_for(&server), "e2").await.unwrap_err();
    assert_eq!(failure.status, Some(404));
}

#[tokio::test]
async fn a_ticket_is_built_on_our_own_base_and_an_absolute_answer_is_refused() {
    let server = fake_server(&[
        (
            "POST /api/v2/tracks/e1/stream-ticket",
            200,
            r#"{"url":"/api/v2/stream/sealed","expires_at":1}"#,
        ),
        (
            "POST /api/v2/tracks/e2/stream-ticket",
            200,
            r#"{"url":"https://elsewhere.example/stream/sealed","expires_at":1}"#,
        ),
        (
            "POST /api/v2/tracks/e3/stream-ticket",
            200,
            r#"{"url":"//elsewhere.example/stream/sealed","expires_at":1}"#,
        ),
    ])
    .await;
    let client = client_for(&server);
    assert_eq!(
        mint_ticket(&client, "e1").await.unwrap(),
        format!("{}/api/v2/stream/sealed", server.base)
    );
    assert!(mint_ticket(&client, "e2").await.is_err());
    assert!(mint_ticket(&client, "e3").await.is_err());
}

// ── the position ────────────────────────────────────────────────────

#[tokio::test]
async fn a_position_is_a_put_a_forgotten_one_a_delete_and_neither_claims_a_replay() {
    let server = fake_server(&[
        ("PUT /api/v2/bookmarks/e1", 204, ""),
        ("DELETE /api/v2/bookmarks/e1", 204, ""),
    ])
    .await;
    let client = client_for(&server);
    save_position(&client, "e1", 12_345).await.unwrap();
    clear_position(&client, "e1").await.unwrap();

    let seen = server.seen.lock().unwrap();
    assert_eq!(seen[0].method, "PUT");
    let sent: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(sent["position_ms"], 12_345);
    assert_eq!(seen[1].method, "DELETE");
    for request in seen.iter() {
        let head = request.head.to_ascii_lowercase();
        assert!(head.contains("authorization: bearer access-token"));
        assert!(!head.contains("x-waveflow-operation-id"), "{head}");
    }
}

// ── managing ────────────────────────────────────────────────────────

#[tokio::test]
async fn subscribing_sends_the_address_to_the_server_and_keeps_none_of_it() {
    let answer = format!(
        r#"{{"id":"c9","url":"https://feeds.example.org/{FEED_TOKEN}.xml","title":"New",
             "description":null,"site_url":null,"image_url":null,"artwork_hash":null,
             "status":"completed","error":null,"created_at":1,"refreshed_at":1,
             "episode_count":0}}"#
    );
    let server = fake_server(&[("POST /api/v2/podcasts", 201, &answer)]).await;
    let feed = format!("https://feeds.example.org/{FEED_TOKEN}.xml");
    let channel = subscribe(&client_for(&server), &feed).await.unwrap();
    assert_eq!(channel.id, "c9");
    assert!(!serde_json::to_string(&channel)
        .unwrap()
        .contains(FEED_TOKEN));
    let seen = server.seen.lock().unwrap();
    let sent: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(sent["url"], feed);
}

#[tokio::test]
async fn the_gestures_of_a_manager_reach_their_routes() {
    let server = fake_server(&[
        ("POST /api/v2/podcasts/refresh", 202, ""),
        ("DELETE /api/v2/podcasts/c1", 204, ""),
        ("POST /api/v2/podcast-episodes/e1/download", 202, ""),
        ("DELETE /api/v2/podcast-episodes/e1", 204, ""),
    ])
    .await;
    let client = client_for(&server);
    refresh(&client).await.unwrap();
    delete_channel(&client, "c1").await.unwrap();
    request_download(&client, "e1").await.unwrap();
    delete_episode(&client, "e1").await.unwrap();
}

#[tokio::test]
async fn a_feed_refused_by_the_server_comes_back_as_its_word() {
    let server = fake_server(&[(
        "POST /api/v2/podcasts",
        422,
        r#"{"code":"feed_rejected","message":"The feed could not be fetched or read","reason":"http_403"}"#,
    )])
    .await;
    let failure = subscribe(&client_for(&server), "https://example.org/feed")
        .await
        .unwrap_err();
    assert_eq!(failure_word(&failure), "http_403");
}

#[tokio::test]
async fn a_refused_gesture_is_read_as_refused_and_says_nothing_of_a_role() {
    let server = fake_server(&[]).await;
    let client = client_for(&server);
    // Every one of these is the 404 an account without the role gets, which is also
    // the 404 of a channel deleted a moment ago. One word for both.
    for failure in [
        delete_channel(&client, "c1").await.unwrap_err(),
        request_download(&client, "e1").await.unwrap_err(),
        delete_episode(&client, "e1").await.unwrap_err(),
        refresh(&client).await.unwrap_err(),
        subscribe(&client, "https://example.org/feed")
            .await
            .unwrap_err(),
    ] {
        assert_eq!(failure_word(&failure), "server_refused");
    }
}

// ── words ───────────────────────────────────────────────────────────

#[test]
fn a_word_comes_from_the_status_and_never_from_what_the_server_wrote() {
    let cases = [
        (401, "unauthorized", "server_signed_out"),
        (403, "forbidden", "server_refused"),
        (404, "not_found", "server_refused"),
        (409, "conflict", "server_conflict"),
        (422, "validation_error", "address_refused"),
        (429, "rate_limited", "server_unreachable"),
        (500, "internal", "server_unreachable"),
        (503, "unavailable", "server_unreachable"),
    ];
    for (status, code, word) in cases {
        let failure = failure_of(status, code, "Some sentence with secret-text in it");
        assert_eq!(failure_word(&failure), word, "{status}");
    }
}

#[test]
fn a_reason_off_the_list_is_not_passed_on() {
    for (reason, word) in [
        ("destination_refused", "destination_refused"),
        ("too_large", "too_large"),
        ("http_404", "http_404"),
        ("http_", "invalid_feed"),
        ("http_40x", "invalid_feed"),
        ("<script>alert(1)</script>", "invalid_feed"),
        ("https://feeds.example.org/private", "invalid_feed"),
    ] {
        let failure = failure_of(422, "feed_rejected", reason);
        assert_eq!(failure_word(&failure), word, "{reason}");
    }
}

#[test]
fn no_answer_and_an_answer_nobody_can_read_are_both_unreachable() {
    for kind in [FailureKind::Transient, FailureKind::Permanent] {
        let failure = RemoteFailure {
            kind,
            status: None,
            code: None,
            message: "http://server/api/v2/stream/ticket".to_owned(),
        };
        assert_eq!(failure_word(&failure), "server_unreachable");
    }
}

// ── whose it is ─────────────────────────────────────────────────────

/// Real schema, as the binding's own tests have it.
async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(":memory:")
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

#[test]
fn a_scope_holds_for_its_server_and_account_only() {
    let scope = scope_on("https://music.example/", "acc-1");
    let same = RemoteClient::from_parts(
        &binding_of("https://music.example", "acc-1"),
        &tokens_pair(),
    )
    .unwrap();
    assert!(scope.holds_for(&same));
    let other_server = RemoteClient::from_parts(
        &binding_of("https://other.example", "acc-1"),
        &tokens_pair(),
    )
    .unwrap();
    assert!(!scope.holds_for(&other_server));
    let other_account = RemoteClient::from_parts(
        &binding_of("https://music.example", "acc-2"),
        &tokens_pair(),
    )
    .unwrap();
    assert!(!scope.holds_for(&other_account));
}

#[test]
fn a_binding_with_no_account_has_no_scope() {
    let subsonic = RemoteBinding {
        server_url: "https://music.example".to_owned(),
        identity: RemoteIdentity::Subsonic {
            username: "someone".to_owned(),
        },
        active_library_id: None,
        bootstrapped_at: None,
    };
    let client = RemoteClient::from_parts(&subsonic, &tokens_pair()).unwrap();
    assert!(ServerScope::of(&client, 1).is_none());
}

#[test]
fn a_printed_scope_names_no_server() {
    let scope = scope_on("https://music.example", "acc-1");
    let printed = format!("{scope:?}");
    assert!(!printed.contains("music.example"), "{printed}");
    assert!(!printed.contains("acc-1"), "{printed}");
}

#[tokio::test]
async fn a_scope_stops_being_current_when_the_binding_moves_on() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let scope = scope_on("https://music.example", "acc-1");

    // Nothing bound.
    assert!(!scope.is_current(&mut conn).await.unwrap());

    binding::write(&mut conn, &binding_of("https://music.example/", "acc-1"))
        .await
        .unwrap();
    // Bound and not signed in: nothing may be written for anybody.
    assert!(!scope.is_current(&mut conn).await.unwrap());

    tokens::write(&mut conn, &tokens_pair()).await.unwrap();
    assert!(scope.is_current(&mut conn).await.unwrap());

    // Another account on the same server: the episode id is the same, the position
    // would be somebody else's.
    binding::write(&mut conn, &binding_of("https://music.example", "acc-2"))
        .await
        .unwrap();
    assert!(!scope.is_current(&mut conn).await.unwrap());

    // Another server.
    binding::write(&mut conn, &binding_of("https://other.example", "acc-1"))
        .await
        .unwrap();
    assert!(!scope.is_current(&mut conn).await.unwrap());

    // Back, then signed out.
    binding::write(&mut conn, &binding_of("https://music.example", "acc-1"))
        .await
        .unwrap();
    assert!(scope.is_current(&mut conn).await.unwrap());
    tokens::clear(&mut conn).await.unwrap();
    assert!(!scope.is_current(&mut conn).await.unwrap());
}

#[test]
fn a_server_on_the_network_may_stream_from_there_and_one_outside_may_not_lead_back_in() {
    assert!(!scope_on("http://192.168.1.20:8080", "acc-1")
        .origin()
        .is_public());
    assert!(!scope_on("http://localhost:8080", "acc-1")
        .origin()
        .is_public());
    assert!(scope_on("https://music.example.org", "acc-1")
        .origin()
        .is_public());
    // An address nothing vouches for gets the stricter rule.
    assert!(scope_on("not a url", "acc-1").origin().is_public());
}
