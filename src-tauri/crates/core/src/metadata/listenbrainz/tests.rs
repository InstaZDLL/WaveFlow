use super::*;

fn listen() -> Listen<'static> {
    Listen {
        artist: "Artist",
        track: "Track",
        release: Some("Album"),
        duration_ms: Some(200_000),
        track_number: Some(3),
        listened_at: Some(1_700_000_000),
    }
}

#[test]
fn a_single_listen_carries_its_time_and_metadata() {
    let body = submit_body(ListenType::Single, &listen());
    assert_eq!(body["listen_type"], "single");
    let item = &body["payload"][0];
    assert_eq!(item["listened_at"], 1_700_000_000);
    let meta = &item["track_metadata"];
    assert_eq!(meta["artist_name"], "Artist");
    assert_eq!(meta["track_name"], "Track");
    assert_eq!(meta["release_name"], "Album");
    assert_eq!(meta["additional_info"]["duration_ms"], 200_000);
    assert_eq!(meta["additional_info"]["tracknumber"], 3);
    assert_eq!(meta["additional_info"]["submission_client"], "WaveFlow");
}

/// The API refuses a "playing now" with a timestamp.
#[test]
fn playing_now_has_no_timestamp() {
    let body = submit_body(ListenType::PlayingNow, &listen());
    assert_eq!(body["listen_type"], "playing_now");
    assert!(body["payload"][0].get("listened_at").is_none());
}

/// Unknown fields are left out rather than sent empty or as zero.
#[test]
fn missing_fields_are_omitted() {
    let mut l = listen();
    l.release = Some("  ");
    l.duration_ms = Some(0);
    l.track_number = None;
    let body = submit_body(ListenType::Single, &l);
    let meta = &body["payload"][0]["track_metadata"];
    assert!(meta.get("release_name").is_none());
    assert!(meta["additional_info"].get("duration_ms").is_none());
    assert!(meta["additional_info"].get("tracknumber").is_none());
}

#[test]
fn failures_sort_into_what_can_be_done() {
    assert!(matches!(
        classify(401, String::new()),
        ListenBrainzError::Unauthorized
    ));
    assert!(classify(400, r#"{"error":"bad"}"#.into()).is_permanent());
    assert!(!classify(429, String::new()).is_permanent());
    assert!(!classify(503, String::new()).is_permanent());
    match classify(400, r#"{"code":400,"error":"Missing key"}"#.into()) {
        ListenBrainzError::Rejected { message, .. } => assert_eq!(message, "Missing key"),
        other => panic!("{other:?}"),
    }
}

use crate::metadata::test_server::{serve, serve_json, Reply};

/// The token rides every request, so a redirect out of the origin is
/// not followed: the other host is never contacted.
#[tokio::test]
async fn a_redirect_out_of_the_origin_is_not_followed() {
    let elsewhere = serve_json(r#"{"valid":true,"user_name":"someone"}"#).await;
    let location = format!("location: {}/validate-token", elsewhere.base_url);
    let asked = serve(Reply {
        status: "307 Temporary Redirect",
        headers: &[&location],
        body: "",
        announce_length: true,
    })
    .await;

    let result = ListenBrainzClient::at(&asked.base_url)
        .validate_token("the-token")
        .await;

    assert!(result.is_err(), "{result:?}");
    assert_eq!(elsewhere.hits(), 0, "the token was sent elsewhere");
}

/// An answer over the ceiling is refused, not read whole, even when it
/// announces no length.
#[tokio::test]
async fn an_oversized_answer_is_refused() {
    let padding = "x".repeat(MAX_RESPONSE_BYTES);
    let body = format!(r#"{{"valid":true,"user_name":"{padding}"}}"#);
    let provider = serve(Reply {
        status: "200 OK",
        headers: &[],
        body: &body,
        announce_length: false,
    })
    .await;

    let error = ListenBrainzClient::at(&provider.base_url)
        .validate_token("the-token")
        .await
        .expect_err("an oversized answer must not be read whole");

    assert!(
        matches!(error, ListenBrainzError::TooLarge { .. }),
        "{error:?}"
    );
}

/// An error body is read through the ceiling too; past it the status
/// still decides, and the message is simply empty.
#[tokio::test]
async fn an_oversized_error_body_still_classifies_by_status() {
    let body = "x".repeat(MAX_RESPONSE_BYTES + 1);
    let provider = serve(Reply {
        status: "503 Service Unavailable",
        headers: &[],
        body: &body,
        announce_length: false,
    })
    .await;

    let error = ListenBrainzClient::at(&provider.base_url)
        .submit("the-token", ListenType::Single, &listen())
        .await
        .expect_err("a 503 is a failure");

    assert!(
        matches!(
            &error,
            ListenBrainzError::Unavailable { status: 503, message } if message.is_empty()
        ),
        "{error:?}"
    );
}
