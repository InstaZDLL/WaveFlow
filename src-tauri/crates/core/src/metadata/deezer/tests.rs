use super::*;

fn hit(xl: Option<&str>, big: Option<&str>) -> DeezerArtistHit {
    DeezerArtistHit {
        id: 1,
        name: "Test".into(),
        picture_small: None,
        picture_medium: None,
        picture_big: big.map(str::to_string),
        picture_xl: xl.map(str::to_string),
        nb_album: None,
        nb_fan: None,
    }
}

fn decode_artists(body: &str) -> DeezerResult<Vec<DeezerArtistHit>> {
    serde_json::from_str::<DeezerReply<DeezerSearchResponse<DeezerArtistHit>>>(body)
        .expect("body should parse as one arm or the other")
        .into_result()
        .map(|r| r.data)
}

#[test]
fn an_in_band_error_is_an_error_and_not_an_empty_result() {
    // Served with HTTP 200. Before this arm existed the missing
    // `data` field made it a decode failure, which every caller
    // logged and treated as "Deezer knows nothing about this".
    let err = decode_artists(
        r#"{"error":{"type":"Exception","message":"Quota limit exceeded","code":4}}"#,
    )
    .expect_err("an error body must not read as a result");
    assert!(err.is_quota_exceeded(), "{err}");
    assert_eq!(
        err.to_string(),
        "deezer refused the request: Quota limit exceeded (code 4)"
    );
}

#[test]
fn a_genuinely_empty_result_stays_a_result() {
    // The case the error arm must not swallow: Deezer answered, and
    // the answer is that it has nothing.
    assert_eq!(decode_artists(r#"{"data":[]}"#).unwrap().len(), 0);
}

#[test]
fn a_populated_result_still_decodes() {
    let hits = decode_artists(r#"{"data":[{"id":27,"name":"Daft Punk"}]}"#).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Daft Punk");
}

#[test]
fn a_body_carrying_both_reads_as_the_refusal() {
    // `#[serde(untagged)]` tries the variants in declaration order
    // and `Error` ignores unknown fields, so a body with `data` *and*
    // `error` resolves to the refusal. That is the precedence we
    // want — a partial payload next to a refusal is still a refusal
    // — and this test is what keeps a variant reorder from silently
    // flipping it.
    let err = decode_artists(
        r#"{"data":[{"id":27,"name":"Daft Punk"}],"error":{"message":"Quota limit exceeded","code":4}}"#,
    )
    .expect_err("a body carrying an error must not read as a result");
    assert!(err.is_quota_exceeded(), "{err}");
}

#[test]
fn a_status_level_rate_limit_is_a_quota_refusal_too() {
    assert!(DeezerError::RateLimited.is_quota_exceeded());
}

#[test]
fn a_non_quota_refusal_is_reported_as_itself() {
    let err = decode_artists(r#"{"error":{"type":"DataException","message":"no data"}}"#)
        .expect_err("still an error");
    assert!(!err.is_quota_exceeded());
    assert_eq!(err.to_string(), "deezer refused the request: no data");
}

#[test]
fn detects_empty_hash_placeholder() {
    assert!(is_placeholder_artist_picture(
        "https://e-cdns-images.dzcdn.net/images/artist//500x500-000000-80-0-0.jpg"
    ));
    assert!(is_placeholder_artist_picture(
        "https://e-cdns-images.dzcdn.net/images/artist/d41d8cd98f00b204e9800998ecf8427e/500x500.jpg"
    ));
}

#[test]
fn accepts_a_real_hash() {
    assert!(!is_placeholder_artist_picture(
        "https://e-cdns-images.dzcdn.net/images/artist/f2bc007e9133c946ac3c3907ddc5d2ea/500x500.jpg"
    ));
}

#[test]
fn best_picture_skips_a_placeholder_and_takes_the_next_real_size() {
    // Exercises the `.find()` skip: a placeholder in the largest slot
    // must not short-circuit — the next real size wins.
    let h = hit(
        Some("https://e-cdns-images.dzcdn.net/images/artist//1000x1000.jpg"),
        Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg"),
    );
    assert_eq!(
        h.best_picture().as_deref(),
        Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg")
    );
}

#[test]
fn best_picture_is_none_when_every_size_is_a_placeholder() {
    // The real-world shape: all sizes share the one empty hash.
    let ph = hit(
        Some("https://e-cdns-images.dzcdn.net/images/artist//1000x1000.jpg"),
        Some("https://e-cdns-images.dzcdn.net/images/artist//500x500.jpg"),
    );
    assert_eq!(ph.best_picture(), None);
}

#[test]
fn best_picture_prefers_the_largest_real_size() {
    let h = hit(
        Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/1000x1000.jpg"),
        Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg"),
    );
    assert_eq!(
        h.best_picture().as_deref(),
        Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/1000x1000.jpg")
    );
}

#[test]
fn best_picture_is_none_when_no_sizes_present() {
    assert_eq!(hit(None, None).best_picture(), None);
}

use crate::metadata::test_server::{serve, serve_json, strict_http, Reply};

#[tokio::test]
async fn a_search_is_read_through_an_injected_client_and_address() {
    let provider = serve_json(r#"{"data":[{"id":464,"name":"Björk"}]}"#).await;
    let client = DeezerClient::with_http(strict_http(), provider.base_url.clone());

    let hits = client.search_artist("Björk").await.unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "Björk");
    assert!(provider.request_line(0).starts_with("GET /search/artist?"));
}

#[tokio::test]
async fn an_answer_over_the_ceiling_is_refused_even_unannounced() {
    let padding = "x".repeat(crate::metadata::MAX_RESPONSE_BYTES);
    let body = format!(r#"{{"data":[{{"id":1,"name":"{padding}"}}]}}"#);
    let provider = serve(Reply {
        status: "200 OK",
        headers: &[],
        body: &body,
        announce_length: false,
    })
    .await;
    let client = DeezerClient::with_http(strict_http(), provider.base_url.clone());

    let error = client
        .search_artist("Björk")
        .await
        .expect_err("an oversized answer must not be read whole");

    assert!(matches!(error, DeezerError::TooLarge { .. }), "{error:?}");
    assert!(!error.is_quota_exceeded());
}

#[tokio::test]
async fn a_playlist_listing_carries_isrc_and_seconds() {
    let provider = serve_json(
        r#"{"data":[{"id":1,"title":"One","duration":225,"isrc":"GBAYE0601690",
               "artist":{"id":2,"name":"A"},"album":{"id":3,"title":"B"}}],"total":1}"#,
    )
    .await;
    let client = DeezerClient::with_http(strict_http(), provider.base_url.clone());

    let (tracks, more) = client
        .get_playlist_tracks_page(908_622_995, 0)
        .await
        .unwrap();

    assert!(!more);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].duration_ms(), Some(225_000));
    assert_eq!(tracks[0].isrc.as_deref(), Some("GBAYE0601690"));
    assert_eq!(
        tracks[0].album.as_ref().and_then(|a| a.title.as_deref()),
        Some("B")
    );
    assert!(provider
        .request_line(0)
        .starts_with("GET /playlist/908622995/tracks?index=0&limit=100"));
}

/// The search travels in the query string, which a caller that logs
/// this error must not log.
#[tokio::test]
async fn a_transport_error_does_not_carry_the_search() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let client = DeezerClient::with_http(strict_http(), format!("http://{address}"));

    let error = client
        .search_artist("the-searched-name")
        .await
        .expect_err("nothing is listening");

    assert!(matches!(error, DeezerError::Http(_)), "{error:?}");
    assert!(
        !format!("{error:?}").contains("the-searched-name"),
        "{error:?}"
    );
}
