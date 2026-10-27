use super::*;

#[test]
fn summary_keeps_short_paragraph_intact() {
    let full = "Daft Punk are a French duo.\n\nSecond paragraph here.";
    assert_eq!(make_summary(full), "Daft Punk are a French duo.");
}

#[test]
fn summary_truncates_long_text_at_word_boundary() {
    let full = "word ".repeat(100);
    let summary = make_summary(full.trim());
    assert!(summary.ends_with('…'));
    assert!(summary.chars().count() <= SUMMARY_MAX + 1);
    // Never cuts mid-word.
    assert!(!summary.trim_end_matches('…').ends_with("wor"));
}

#[test]
fn bio_for_lang_falls_back_to_english() {
    let payload = ArtistPayload {
        name: Some("X".into()),
        bio_en: Some("English bio".into()),
        bio_de: Some("  ".into()), // blank → ignored
        ..Default::default()
    };
    assert_eq!(payload.bio_for_lang("fr").as_deref(), Some("English bio"));
    assert_eq!(payload.bio_for_lang("de").as_deref(), Some("English bio"));
}

/// Every fanart the artist has, in TheAudioDB's order, blanks
/// dropped — the picker offers the list and the hero paints the
/// first of it.
#[test]
fn fanart_urls_keep_every_image_in_order() {
    let payload = ArtistPayload {
        fanart: Some("  ".into()), // blank → skipped
        fanart2: Some("https://cdn/fanart2.jpg".into()),
        fanart4: Some("https://cdn/fanart4.jpg".into()),
        ..Default::default()
    };
    assert_eq!(
        payload.fanart_urls(),
        vec![
            "https://cdn/fanart2.jpg".to_string(),
            "https://cdn/fanart4.jpg".to_string(),
        ]
    );
}

/// An artist with no fanart has **nothing** wide, even when
/// TheAudioDB offers a wide thumb or a banner: both are 1000×185
/// strips, and the hero falls back to its blurred-photo tier rather
/// than stretching one (#693).
#[test]
fn a_strip_is_not_a_backdrop() {
    let payload: ArtistPayload = serde_json::from_str(
        r#"{
                "strArtist": "X",
                "strArtistWideThumb": "https://cdn/wide.jpg",
                "strArtistBanner": "https://cdn/banner.jpg"
            }"#,
    )
    .expect("payload with strips only");
    assert!(payload.fanart_urls().is_empty());
    assert_eq!(ArtistPayload::default().fanart_urls(), Vec::<String>::new());
}

use crate::metadata::test_server::{serve, serve_json, strict_http, Reply};

#[tokio::test]
async fn the_operators_key_goes_where_the_free_one_used_to() {
    let provider =
        serve_json(r#"{"artists":[{"strArtist":"Björk","strBiography":"Born in Reykjavík."}]}"#)
            .await;
    let client = TheAudioDbClient::with_http(
        strict_http(),
        format!("{}/api/v1/json", provider.base_url),
        "operator-key",
    )
    .unwrap();

    let artist = client
        .artist_info("Björk", "en")
        .await
        .unwrap()
        .expect("a match");

    assert_eq!(artist.bio_full.as_deref(), Some("Born in Reykjavík."));
    let line = provider.request_line(0);
    assert!(
        line.starts_with("GET /api/v1/json/operator-key/search.php?"),
        "{line}"
    );
    assert!(!line.contains("/123/"), "{line}");
}

#[tokio::test]
async fn a_failure_status_is_a_failure_and_not_an_absence() {
    let provider = serve(Reply {
        status: "503 Service Unavailable",
        headers: &[],
        body: "",
        announce_length: true,
    })
    .await;
    let client =
        TheAudioDbClient::with_http(strict_http(), provider.base_url.clone(), "k").unwrap();

    let error = client
        .artist_info("Björk", "en")
        .await
        .expect_err("an outage must not read as an unknown artist");

    assert!(
        matches!(error, LookupError::Status { status: 503 }),
        "{error:?}"
    );
}

/// TheAudioDB takes its key in the path rather than the query, which
/// a redaction aimed at query strings alone would miss.
#[tokio::test]
async fn a_transport_error_does_not_carry_the_key() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let client =
        TheAudioDbClient::with_http(strict_http(), format!("http://{address}"), "the-secret-key")
            .unwrap();

    let error = client
        .artist_info("Björk", "en")
        .await
        .expect_err("nothing is listening");

    assert!(matches!(error, LookupError::Http(_)), "{error:?}");
    assert!(
        !format!("{error:?}").contains("the-secret-key"),
        "{error:?}"
    );
}

#[test]
fn a_plaintext_address_off_this_machine_is_refused() {
    let refused = TheAudioDbClient::with_http(
        strict_http(),
        "http://www.theaudiodb.com/api/v1/json",
        "operator-key",
    );
    assert!(matches!(refused, Err(BaseUrlError::Plaintext { .. })));
}
