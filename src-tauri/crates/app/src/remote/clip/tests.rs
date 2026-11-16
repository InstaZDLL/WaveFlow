//! The clip ticket against a server that answers what each test says.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;

use super::super::binding::{RemoteBinding, RemoteIdentity};
use super::super::tokens::TokenPair;
use super::*;

/// `METHOD path` → (status, JSON body). Anything else is the server's 404.
type Routes = HashMap<String, (u16, String)>;

#[derive(Clone)]
struct Fake {
    routes: Arc<Routes>,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn answer(
    axum::extract::State(fake): axum::extract::State<Fake>,
    request: axum::extract::Request,
) -> axum::response::Response {
    // The path as it arrived, still percent-encoded: what the router of the
    // real server would have to decode.
    let route = format!("{} {}", request.method(), request.uri().path());
    fake.seen.lock().unwrap().push(route.clone());
    let (status, body) = fake.routes.get(&route).cloned().unwrap_or((
        404,
        r#"{"code":"not_found","message":"Not found"}"#.to_owned(),
    ));
    (
        axum::http::StatusCode::from_u16(status).unwrap(),
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// A server on a free port, and what it was asked.
async fn server(routes: &[(&str, u16, &str)]) -> (String, Arc<Mutex<Vec<String>>>) {
    let fake = Fake {
        routes: Arc::new(
            routes
                .iter()
                .map(|(route, status, body)| ((*route).to_owned(), (*status, (*body).to_owned())))
                .collect(),
        ),
        seen: Arc::new(Mutex::new(Vec::new())),
    };
    let seen = Arc::clone(&fake.seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new().fallback(answer).with_state(fake);
    tokio::spawn(async move { axum::serve(listener, router).await });
    (base, seen)
}

fn client_for<'a>(base: &str) -> RemoteClient<'a> {
    let binding = RemoteBinding {
        server_url: base.to_owned(),
        identity: RemoteIdentity::Waveflow {
            account_id: "acc-1".to_owned(),
            device_id: "dev-1".to_owned(),
            cursor: 0,
        },
        active_library_id: None,
        bootstrapped_at: None,
    };
    let pair = TokenPair {
        access_token: "access-token".to_owned(),
        refresh_token: "refresh-token".to_owned(),
        expires_at: None,
        username: None,
    };
    RemoteClient::from_parts(&binding, &pair).unwrap()
}

#[tokio::test]
async fn a_ticket_is_built_on_our_own_base_and_keeps_its_expiry() {
    let (base, seen) = server(&[(
        "POST /api/v2/tracks/t1/clip-ticket",
        200,
        r#"{"url":"/api/v2/clip-stream/sealed","expires_at":1763300000000}"#,
    )])
    .await;
    let ticket = mint(&client_for(&base), "t1").await.unwrap();
    assert_eq!(
        ticket,
        Some(ClipTicket {
            url: format!("{base}/api/v2/clip-stream/sealed"),
            expires_at: 1_763_300_000_000,
        })
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["POST /api/v2/tracks/t1/clip-ticket"]
    );
}

#[tokio::test]
async fn the_frontend_reads_the_expiry_under_its_own_name() {
    let ticket = ClipTicket {
        url: "http://server/api/v2/clip-stream/sealed".into(),
        expires_at: 5,
    };
    assert_eq!(
        serde_json::to_value(&ticket).unwrap(),
        serde_json::json!({"url": "http://server/api/v2/clip-stream/sealed", "expiresAt": 5})
    );
}

#[tokio::test]
async fn a_404_is_no_clip_and_not_an_error() {
    // The server's answer to every refusal: no clip, somebody else's track,
    // a clip deleted since the scan.
    let (base, _) = server(&[]).await;
    assert_eq!(mint(&client_for(&base), "t1").await.unwrap(), None);
}

#[tokio::test]
async fn a_server_out_of_reach_is_an_error_and_not_a_missing_clip() {
    // A player told "no clip" would end the visit; a 503 is a reason to try
    // again, not to forget the clip.
    let (base, _) = server(&[(
        "POST /api/v2/tracks/t1/clip-ticket",
        503,
        r#"{"code":"unavailable","message":"busy"}"#,
    )])
    .await;
    assert!(mint(&client_for(&base), "t1").await.is_err());

    // Nobody listening at all.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    assert!(mint(&client_for(&base), "t1").await.is_err());
}

#[tokio::test]
async fn only_a_404_is_no_clip_and_a_broken_answer_is_an_error() {
    // Both are permanent failures to the client, and neither says the track
    // has no clip: a player told `None` would end the visit over a server
    // that broke its contract.
    let (base, _) = server(&[
        (
            "POST /api/v2/tracks/t1/clip-ticket",
            200,
            r#"{"ticket":"sealed"}"#,
        ),
        (
            "POST /api/v2/tracks/t2/clip-ticket",
            400,
            r#"{"code":"bad_request","message":"no"}"#,
        ),
    ])
    .await;
    let client = client_for(&base);
    assert!(mint(&client, "t1").await.is_err());
    assert!(mint(&client, "t2").await.is_err());
}

#[tokio::test]
async fn an_absolute_answer_is_refused_rather_than_followed() {
    let (base, _) = server(&[
        (
            "POST /api/v2/tracks/t1/clip-ticket",
            200,
            r#"{"url":"https://elsewhere.example/clip-stream/sealed","expires_at":1}"#,
        ),
        (
            "POST /api/v2/tracks/t2/clip-ticket",
            200,
            r#"{"url":"//elsewhere.example/clip-stream/sealed","expires_at":1}"#,
        ),
    ])
    .await;
    let client = client_for(&base);
    assert!(mint(&client, "t1").await.is_err());
    assert!(mint(&client, "t2").await.is_err());
}

#[tokio::test]
async fn an_identifier_stays_one_segment_and_a_dot_segment_asks_nobody() {
    let (base, seen) = server(&[(
        "POST /api/v2/tracks/a%2Fb/clip-ticket",
        200,
        r#"{"url":"/api/v2/clip-stream/sealed","expires_at":1}"#,
    )])
    .await;
    let client = client_for(&base);
    assert!(mint(&client, "a/b").await.unwrap().is_some());
    assert_eq!(mint(&client, "..").await.unwrap(), None);
    assert_eq!(
        *seen.lock().unwrap(),
        ["POST /api/v2/tracks/a%2Fb/clip-ticket"]
    );
}
