use super::*;

#[test]
fn detects_the_star_placeholder_across_cdn_hosts() {
    assert!(is_lastfm_star_placeholder(
        "https://lastfm.freetls.fastly.net/i/u/300x300/2a96cbd8b46e442fc41c2b86b821562f.png"
    ));
    assert!(is_lastfm_star_placeholder(
        "https://lastfm-img2.akamaized.net/i/u/174s/2a96cbd8b46e442fc41c2b86b821562f.png"
    ));
}

#[test]
fn accepts_a_real_lastfm_picture() {
    assert!(!is_lastfm_star_placeholder(
        "https://lastfm.freetls.fastly.net/i/u/300x300/abc123def456abc123def456abc12345.png"
    ));
}

use crate::metadata::test_server::{serve, serve_json, strict_http, Reply};

/// A secret and a password made at run time. Handed to a signed call,
/// a literal is a hard-coded credential to CodeQL whatever the test is
/// about, and these tests are about the body and the error, not the
/// values.
fn throwaway_credentials() -> (String, String) {
    let nonce = std::process::id();
    (nonce.to_string(), nonce.wrapping_add(1).to_string())
}

fn client_for(base_url: &str) -> LastfmClient {
    LastfmClient::with_http(strict_http(), format!("{base_url}/2.0/")).unwrap()
}

#[tokio::test]
async fn a_biography_is_read_through_an_injected_client_and_address() {
    let provider = serve_json(
        r#"{"artist":{"name":"Björk","bio":{"summary":"Icelandic. <a href=\"https://www.last.fm/music/Bj%C3%B6rk\">Read more on Last.fm</a>","content":"Born in Reykjavík."}}}"#,
    )
    .await;

    let info = client_for(&provider.base_url)
        .artist_get_info("Björk", "the-key")
        .await
        .unwrap()
        .expect("a biography");

    assert_eq!(info.bio_full.as_deref(), Some("Born in Reykjavík."));
    let line = provider.request_line(0);
    assert!(line.starts_with("GET /2.0/?"), "{line}");
    assert!(line.contains("method=artist.getinfo"), "{line}");
    assert!(line.contains("api_key=the-key"), "{line}");
}

#[tokio::test]
async fn an_unknown_artist_is_an_answer() {
    let provider =
        serve_json(r#"{"error":6,"message":"The artist you supplied could not be found"}"#).await;

    let info = client_for(&provider.base_url)
        .artist_get_info("Nobody At All", "the-key")
        .await
        .expect("not found is not a failure");

    assert!(info.is_none());
}

/// The reason this module's read methods stopped answering `Ok` for
/// every error code: a refused key is a failure to ask, and a caller
/// that caches answers would otherwise remember it as an artist
/// without a biography.
#[tokio::test]
async fn a_refused_key_is_a_failure_and_not_an_absence() {
    let provider = serve(Reply {
        status: "403 Forbidden",
        headers: &[],
        body: r#"{"error":10,"message":"Invalid API key - You must be granted a valid key by last.fm"}"#,
        announce_length: true,
    })
    .await;

    let error = client_for(&provider.base_url)
        .artist_get_info("Björk", "a-bad-key")
        .await
        .expect_err("a refused key must not read as an absence");

    assert!(
        matches!(error, LookupError::Refused { code: 10 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_spent_quota_under_a_success_status_is_still_a_failure() {
    let provider = serve_json(r#"{"error":29,"message":"Rate limit exceeded"}"#).await;

    let error = client_for(&provider.base_url)
        .artist_get_similar("Björk", "the-key", 10)
        .await
        .expect_err("a throttled call must not read as no neighbours");

    assert!(
        matches!(error, LookupError::Refused { code: 29 }),
        "{error:?}"
    );
}

#[tokio::test]
/// The redirect stays inside the origin on purpose: the desktop's own
/// client follows those, so only the client handed to `with_http` —
/// which follows none — can be the reason this one is refused. A
/// redirect to another origin would be refused by either, and could
/// not tell them apart.
async fn a_redirect_is_not_followed_by_a_client_that_follows_none() {
    let provider = serve(Reply {
        status: "302 Found",
        headers: &["location: /2.0/moved"],
        body: "",
        announce_length: true,
    })
    .await;

    let error = client_for(&provider.base_url)
        .artist_get_info("Björk", "the-key")
        .await
        .expect_err("a redirect is not an answer");

    assert!(
        matches!(error, LookupError::Status { status: 302 }),
        "{error:?}"
    );
    assert_eq!(provider.hits(), 1, "the redirect was followed");
}

#[tokio::test]
async fn an_answer_over_the_ceiling_is_refused_even_unannounced() {
    let padding = "x".repeat(MAX_RESPONSE_BYTES);
    let body = format!(r#"{{"artist":{{"name":"{padding}"}}}}"#);
    let provider = serve(Reply {
        status: "200 OK",
        headers: &[],
        body: &body,
        announce_length: false,
    })
    .await;

    let error = client_for(&provider.base_url)
        .artist_get_info("Björk", "the-key")
        .await
        .expect_err("an oversized answer must not be read whole");

    assert!(matches!(error, LookupError::TooLarge { .. }), "{error:?}");
}

#[tokio::test]
async fn a_musicbrainz_id_is_asked_as_one() {
    let provider = serve_json(r#"{"artist":{"name":"Björk"}}"#).await;

    client_for(&provider.base_url)
        .artist_get_info_by_mbid("87c5dedd-371d-4a53-9f7f-80522fb7f3cb", "the-key")
        .await
        .unwrap();

    let line = provider.request_line(0);
    assert!(
        line.contains("mbid=87c5dedd-371d-4a53-9f7f-80522fb7f3cb"),
        "{line}"
    );
    assert!(!line.contains("artist="), "{line}");
}

/// The key travels in the query string, and `reqwest` prints the URL
/// of a failed request — so an unredacted error would hand the key to
/// whatever logs it.
#[tokio::test]
async fn a_transport_error_does_not_carry_the_key() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let error = LastfmClient::with_http(strict_http(), format!("http://{address}/2.0/"))
        .unwrap()
        .artist_get_info("Björk", "the-secret-key")
        .await
        .expect_err("nothing is listening");

    assert!(matches!(error, LookupError::Http(_)), "{error:?}");
    assert!(!format!("{error}").contains("the-secret-key"), "{error}");
    assert!(
        !format!("{error:?}").contains("the-secret-key"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_failure_status_with_an_empty_object_is_a_failure() {
    let provider = serve(Reply {
        status: "503 Service Unavailable",
        headers: &[],
        body: "{}",
        announce_length: true,
    })
    .await;

    let error = client_for(&provider.base_url)
        .artist_get_info("Björk", "the-key")
        .await
        .expect_err("an outage must not read as an unknown artist");

    assert!(
        matches!(error, LookupError::Status { status: 503 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_signed_answer_over_the_ceiling_is_refused_even_unannounced() {
    let (secret, password) = throwaway_credentials();
    let padding = "x".repeat(MAX_RESPONSE_BYTES);
    let body = format!(r#"{{"session":{{"name":"{padding}","key":"k"}}}}"#);
    let provider = serve(Reply {
        status: "200 OK",
        headers: &[],
        body: &body,
        announce_length: false,
    })
    .await;

    let error = client_for(&provider.base_url)
        .auth_get_mobile_session("the-key", &secret, "someone", &password)
        .await
        .expect_err("an oversized answer must not be read whole");

    assert!(matches!(error, LastfmError::TooLarge { .. }), "{error:?}");
}

/// A reply that fails to decode can still carry the session key, which
/// is good until revoked — and the error it becomes is logged.
#[tokio::test]
async fn a_session_reply_that_fails_to_decode_does_not_carry_its_key() {
    let (secret, password) = throwaway_credentials();
    let provider = serve_json(r#"{"session":{"key":"the-session-key"}}"#).await;

    let error = client_for(&provider.base_url)
        .auth_get_mobile_session("the-key", &secret, "someone", &password)
        .await
        .expect_err("a session without a name does not decode");

    assert!(!format!("{error}").contains("the-session-key"), "{error}");
    assert!(
        !format!("{error:?}").contains("the-session-key"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_signed_transport_error_does_not_carry_its_url() {
    let (secret, password) = throwaway_credentials();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let client = LastfmClient::with_http(
        strict_http(),
        format!("http://{address}/a-path-worth-hiding/2.0/"),
    )
    .unwrap();

    let error = client
        .auth_get_mobile_session("the-key", &secret, "someone", &password)
        .await
        .expect_err("nothing is listening");

    assert!(matches!(error, LastfmError::Http(_)), "{error:?}");
    assert!(
        !format!("{error:?}").contains("a-path-worth-hiding"),
        "{error:?}"
    );
}

#[test]
fn a_plaintext_address_off_this_machine_is_refused() {
    let refused = LastfmClient::with_http(strict_http(), "http://ws.audioscrobbler.com/2.0/");
    assert!(matches!(refused, Err(BaseUrlError::Plaintext { .. })));
}
