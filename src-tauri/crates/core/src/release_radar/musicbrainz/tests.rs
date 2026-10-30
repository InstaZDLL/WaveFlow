use super::*;
use crate::metadata::test_server::{serve, serve_json, Reply};

const ANSWER: &str = r#"{"release-groups":[{"id":"rg-1","title":"New","first-release-date":"2026-09-01",
  "primary-type":"Album","artist-credit":[{"name":"Nova","joinphrase":"","artist":{"id":"mb-nova","name":"Nova"}}]}]}"#;

#[tokio::test]
async fn the_request_names_the_application_and_sends_the_query() {
    let served = serve_json(ANSWER).await;
    let client = MusicBrainzClient::at(&served.base_url);
    let page = client
        .recent_release_groups(&ArtistQuery::Name("Nova"), "2026-05-01")
        .await
        .unwrap();
    assert_eq!(page.releases.len(), 1);
    assert_eq!(page.releases[0].credit_mbid.as_deref(), Some("mb-nova"));

    let line = served.request_line(0);
    assert!(line.starts_with("GET /release-group?query="), "{line}");
    assert!(line.contains("fmt=json"), "{line}");
    // The phrase and the window, form-encoded.
    assert!(line.contains("artist%3A%22Nova%22"), "{line}");
    assert!(
        line.contains("firstreleasedate%3A%5B2026-05-01+TO+*%5D"),
        "{line}"
    );
    assert!(USER_AGENT.starts_with("WaveFlow/"));
    assert!(USER_AGENT.contains("github.com/InstaZDLL/WaveFlow"));
}

#[tokio::test]
async fn a_503_is_a_request_to_slow_down_with_its_delay() {
    let served = serve(Reply {
        status: "503 Service Unavailable",
        headers: &["retry-after: 4"],
        body: "",
        announce_length: true,
    })
    .await;
    let client = MusicBrainzClient::at(&served.base_url);
    let err = client
        .recent_release_groups(&ArtistQuery::Mbid("mb-nova"), "2026-05-01")
        .await
        .unwrap_err();
    match err {
        MusicBrainzError::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(Duration::from_secs(4)))
        }
        other => panic!("expected a rate limit, got {other:?}"),
    }
    assert_eq!(served.hits(), 1, "a rate limit is never retried here");
}

#[tokio::test]
async fn a_long_retry_after_is_capped_and_a_429_counts_too() {
    let served = serve(Reply {
        status: "429 Too Many Requests",
        headers: &["retry-after: 3600"],
        body: "",
        announce_length: true,
    })
    .await;
    let err = MusicBrainzClient::at(&served.base_url)
        .recent_release_groups(&ArtistQuery::Name("Nova"), "2026-05-01")
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MusicBrainzError::RateLimited { retry_after: Some(d) } if d == MAX_RETRY_AFTER
    ));
}

#[tokio::test]
async fn another_failure_status_is_reported_as_such() {
    let served = serve(Reply {
        status: "400 Bad Request",
        headers: &[],
        body: "{}",
        announce_length: true,
    })
    .await;
    let err = MusicBrainzClient::at(&served.base_url)
        .recent_release_groups(&ArtistQuery::Name("Nova"), "2026-05-01")
        .await
        .unwrap_err();
    assert!(matches!(err, MusicBrainzError::Status { status: 400 }));
}
