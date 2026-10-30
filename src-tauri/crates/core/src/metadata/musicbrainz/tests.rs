use super::*;
use std::sync::Mutex as StdMutex;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A listener that answers its connections with `replies` in turn (the
/// last one repeated), and keeps each request's head.
struct Sequence {
    base_url: String,
    heads: Arc<StdMutex<Vec<(Instant, String)>>>,
}

async fn serve(replies: Vec<(&'static str, &'static [&'static str], &'static str)>) -> Sequence {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let heads = Arc::new(StdMutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        let mut index = 0;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut head = [0u8; 4096];
            let read = socket.read(&mut head).await.unwrap_or(0);
            seen.lock().unwrap().push((
                Instant::now(),
                String::from_utf8_lossy(&head[..read]).into_owned(),
            ));
            let (status, headers, body) = replies[index.min(replies.len() - 1)];
            index += 1;
            let mut response = format!("HTTP/1.1 {status}\r\n");
            for header in headers {
                response.push_str(header);
                response.push_str("\r\n");
            }
            response.push_str(&format!(
                "content-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            ));
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    Sequence {
        base_url: format!("http://{address}/ws/2"),
        heads,
    }
}

fn client(base: &str, interval: Duration) -> MusicBrainzClient {
    MusicBrainzClient::with_http(
        reqwest::Client::builder()
            .user_agent(user_agent("9.9.9"))
            .build()
            .unwrap(),
        base,
        Arc::new(Pacer::new(interval)),
        Duration::from_millis(20),
    )
}

const ONE_RELEASE: &str = r#"{"releases":[{"id":"9dce399d-3ec7-34a4-97d0-b4bac54e5f08","score":100,
"title":"Nevermind","track-count":13,"artist-credit":[{"name":"Nirvana",
"artist":{"id":"5b11f4ce-a62d-471e-81fc-a69a8278c7da","name":"Nirvana"}}]}]}"#;

#[tokio::test]
async fn a_search_sends_the_identifying_agent_and_parses_the_hits() {
    let server = serve(vec![("200 OK", &[], ONE_RELEASE)]).await;
    let hits = client(&server.base_url, Duration::from_millis(1))
        .search_releases("Nevermind", Some("Nirvana"), 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track_count, Some(13));
    assert_eq!(credit_text(&hits[0].artist_credit), "Nirvana");
    let heads = server.heads.lock().unwrap();
    let head = heads[0].1.to_ascii_lowercase();
    assert!(head.contains("user-agent: waveflow/9.9.9 ( https://waveflow.app )"));
    assert!(head.starts_with("get /ws/2/release?query=release%3a%22nevermind%22"));
}

#[tokio::test]
async fn a_503_is_retried_and_never_read_as_an_empty_answer() {
    let server = serve(vec![
        ("503 Service Unavailable", &["retry-after: 0"], "{}"),
        ("200 OK", &[], ONE_RELEASE),
    ])
    .await;
    let hits = client(&server.base_url, Duration::from_millis(1))
        .search_releases("Nevermind", None, 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(server.heads.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn throttling_that_does_not_end_is_an_error() {
    let server = serve(vec![("429 Too Many Requests", &[], "{}")]).await;
    let err = client(&server.base_url, Duration::from_millis(1))
        .search_releases("Nevermind", None, 5)
        .await
        .unwrap_err();
    assert!(matches!(err, LookupError::Status { status: 429 }), "{err}");
    assert_eq!(server.heads.lock().unwrap().len(), MAX_ATTEMPTS as usize);
}

#[tokio::test]
async fn requests_are_paced() {
    let server = serve(vec![("200 OK", &[], r#"{"releases":[]}"#)]).await;
    let c = client(&server.base_url, Duration::from_millis(150));
    // Empty phrase results fall back to a loose query: two requests.
    c.search_releases("Nevermind", Some("Nirvana"), 5)
        .await
        .unwrap();
    let heads = server.heads.lock().unwrap();
    assert_eq!(heads.len(), 2);
    let gap = heads[1].0.duration_since(heads[0].0);
    assert!(gap >= Duration::from_millis(140), "{gap:?}");
}

#[tokio::test]
async fn an_unknown_release_is_none_and_a_bad_identifier_is_not_sent() {
    let server = serve(vec![("404 Not Found", &[], r#"{"error":"Not Found"}"#)]).await;
    let c = client(&server.base_url, Duration::from_millis(1));
    assert!(c
        .get_release("9dce399d-3ec7-34a4-97d0-b4bac54e5f08")
        .await
        .unwrap()
        .is_none());
    assert!(c.get_release("../artist/x").await.unwrap().is_none());
    assert_eq!(server.heads.lock().unwrap().len(), 1);
}

#[test]
fn queries_escape_what_lucene_would_read() {
    assert_eq!(
        release_query("Say \"Hi\"", Some("AC/DC"), true),
        r#"release:"Say \"Hi\"" AND artist:"AC/DC""#
    );
    assert_eq!(
        release_query("Live! (1977)", None, false),
        r"release:(Live\! \(1977\))"
    );
    assert_eq!(release_query("  ", Some("x"), true), "");
}

#[test]
fn only_uuids_are_identifiers() {
    assert!(is_mbid("5b11f4ce-a62d-471e-81fc-a69a8278c7da"));
    assert!(!is_mbid("5b11f4cea62d471e81fca69a8278c7da"));
    assert!(!is_mbid("5b11f4ce-a62d-471e-81fc-a69a8278c7d/"));
}
