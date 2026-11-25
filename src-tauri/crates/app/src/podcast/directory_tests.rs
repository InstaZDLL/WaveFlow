use std::sync::Mutex;
use std::time::Duration;

use url::Url;
use waveflow_core::podcast::FetchFailure;

use super::directory::*;
use super::fetch::FetchError;
use super::refresh::PodcastError;
use super::tests::{enabled_pool, switch, TestLeases};

type Answer = Result<Vec<u8>, FetchError>;

/// Answers from a function of the address, and records what was asked.
struct Table {
    answer: Box<dyn Fn(&Url) -> Answer + Send + Sync>,
    asked: Mutex<Vec<Url>>,
}

impl Table {
    fn new(answer: impl Fn(&Url) -> Answer + Send + Sync + 'static) -> Self {
        Self {
            answer: Box::new(answer),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<Url> {
        self.asked.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl Asker for Table {
    async fn ask(&self, url: &Url, _max_bytes: usize) -> Answer {
        self.asked.lock().unwrap().push(url.clone());
        (self.answer)(url)
    }
}

const CHART: &str = r#"{"feed": {"entry": [
  {"im:name": {"label": "First"}, "id": {"attributes": {"im:id": "11"}},
   "im:image": [{"label": "https://is1-ssl.mzstatic.com/a/170x170bb.png"}]},
  {"im:name": {"label": "Second"}, "id": {"attributes": {"im:id": "22"}}}
]}}"#;

const LOOKUP: &str = r#"{"results": [
  {"collectionId": 22, "collectionName": "Second", "feedUrl": "https://feeds.example.com/22"},
  {"collectionId": 11, "collectionName": "First", "feedUrl": "https://feeds.example.com/11",
   "genreIds": ["1482", "26", "1301"]}
]}"#;

/// The chart and the lookup of its shows, as the directory answers them.
fn directory_answers(url: &Url) -> Answer {
    if url.path().ends_with("/json") {
        Ok(CHART.as_bytes().to_vec())
    } else if url.path() == "/lookup" {
        Ok(LOOKUP.as_bytes().to_vec())
    } else {
        Ok(br#"{"results": [{"collectionId": 33, "collectionName": "Found", "feedUrl": "https://feeds.example.com/33"}]}"#.to_vec())
    }
}

async fn leases() -> TestLeases {
    TestLeases(enabled_pool().await)
}

fn feeds(shows: &[waveflow_core::podcast::directory::DirectoryShow]) -> Vec<Option<&str>> {
    shows.iter().map(|show| show.feed_url.as_deref()).collect()
}

/// A small image, encoded for real.
fn png() -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbImage::new(4, 4)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

// ── charts ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_chart_is_read_with_its_feeds_in_two_requests_then_kept() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let directory = Directory::default();
    let shows = directory
        .chart(&leases, &table, "fr", Some(1487))
        .await
        .unwrap();
    assert_eq!(
        shows
            .iter()
            .map(|show| show.title.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"],
        "the chart's order, not the lookup's"
    );
    assert_eq!(
        feeds(&shows),
        [
            Some("https://feeds.example.com/11"),
            Some("https://feeds.example.com/22")
        ]
    );
    assert!(
        shows.iter().all(|show| show.genre_id == Some(1487)),
        "a narrowed chart's shows are of its genre"
    );
    let asked = table.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(
        asked[0].as_str(),
        "https://itunes.apple.com/fr/rss/toppodcasts/limit=200/genre=1487/json"
    );
    assert!(
        asked[1].query().unwrap().contains("id=11%2C22"),
        "{}",
        asked[1]
    );

    let again = directory
        .chart(&leases, &table, "fr", Some(1487))
        .await
        .unwrap();
    assert_eq!(again.len(), 2);
    assert_eq!(table.count(), 2, "a kept chart asks nothing");
    // Another genre or country is another question.
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    directory
        .chart(&leases, &table, "de", Some(1487))
        .await
        .unwrap();
    assert_eq!(table.count(), 6);
}

#[tokio::test]
async fn a_chart_takes_a_shows_genre_from_its_lookup() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let shows = Directory::default()
        .chart(&leases, &table, "fr", None)
        .await
        .unwrap();
    assert_eq!(shows[0].genre_id, Some(1301));
    assert_eq!(shows[1].genre_id, None);
}

#[tokio::test]
async fn a_chart_whose_lookup_fails_is_shown_without_feeds() {
    let leases = leases().await;
    let table = Table::new(|url| {
        if url.path() == "/lookup" {
            Err(FetchError::Failed(FetchFailure::Status(403)))
        } else {
            directory_answers(url)
        }
    });
    let shows = Directory::default()
        .chart(&leases, &table, "fr", None)
        .await
        .unwrap();
    assert_eq!(feeds(&shows), [None, None]);
}

#[tokio::test]
async fn a_refusal_is_rate_limited_and_is_not_kept() {
    let leases = leases().await;
    for status in [403, 429] {
        let table = Table::new(move |_| Err(FetchError::Failed(FetchFailure::Status(status))));
        let directory = Directory::default();
        for _ in 0..2 {
            let err = directory
                .chart(&leases, &table, "fr", None)
                .await
                .unwrap_err();
            assert!(matches!(err, PodcastError::RateLimited), "{err:?}");
        }
        assert_eq!(table.count(), 2, "a failure is asked again");
    }
}

#[tokio::test]
async fn an_answer_that_is_not_one_is_directory_invalid() {
    let leases = leases().await;
    let table = Table::new(|_| Ok(b"<html>busy</html>".to_vec()));
    let err = Directory::default()
        .search(&leases, &table, "fr", "news")
        .await
        .unwrap_err();
    assert!(matches!(err, PodcastError::DirectoryInvalid), "{err:?}");
    let table = Table::new(|_| Err(FetchError::Failed(FetchFailure::TooLarge)));
    let err = Directory::default()
        .search(&leases, &table, "fr", "news")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PodcastError::Failed(FetchFailure::TooLarge)),
        "{err:?}"
    );
}

// ── the switch and offline mode ─────────────────────────────────────

#[tokio::test]
async fn with_the_switch_off_nothing_is_asked_and_nothing_kept_is_served() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let directory = Directory::default();
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    directory.search(&leases, &table, "fr", "x").await.unwrap();
    let before = table.count();
    switch(&leases.0, false).await;
    assert!(matches!(
        directory.chart(&leases, &table, "fr", None).await,
        Err(PodcastError::Disabled)
    ));
    assert!(matches!(
        directory.search(&leases, &table, "fr", "x").await,
        Err(PodcastError::Disabled)
    ));
    assert!(matches!(
        directory.feed_of(&leases, &table, "fr", 11).await,
        Err(PodcastError::Disabled)
    ));
    assert!(matches!(
        directory.picture(&leases, &table, 11).await,
        Err(PodcastError::Disabled)
    ));
    assert_eq!(table.count(), before);
}

#[tokio::test]
async fn offline_is_told_as_offline() {
    let leases = leases().await;
    let table = Table::new(|_| Err(FetchError::Offline));
    let directory = Directory::default();
    assert!(matches!(
        directory.search(&leases, &table, "fr", "x").await,
        Err(PodcastError::Offline)
    ));
    assert!(matches!(
        directory.chart(&leases, &table, "fr", None).await,
        Err(PodcastError::Offline)
    ));
}

#[tokio::test]
async fn offline_during_a_charts_lookup_stops_the_chart() {
    let leases = leases().await;
    let table = Table::new(|url| {
        if url.path() == "/lookup" {
            Err(FetchError::Offline)
        } else {
            directory_answers(url)
        }
    });
    let directory = Directory::default();
    assert!(matches!(
        directory.chart(&leases, &table, "fr", None).await,
        Err(PodcastError::Offline)
    ));
}

// ── a show's feed ───────────────────────────────────────────────────

#[tokio::test]
async fn a_feed_already_given_is_used_without_asking_again() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let directory = Directory::default();
    directory
        .search(&leases, &table, "fr", "found")
        .await
        .unwrap();
    assert_eq!(table.count(), 1);
    let feed = directory.feed_of(&leases, &table, "fr", 33).await.unwrap();
    assert_eq!(feed.as_str(), "https://feeds.example.com/33");
    assert_eq!(table.count(), 1);
}

#[tokio::test]
async fn a_show_not_met_yet_is_looked_up() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let directory = Directory::default();
    let feed = directory.feed_of(&leases, &table, "jp", 22).await.unwrap();
    assert_eq!(feed.as_str(), "https://feeds.example.com/22");
    let asked = table.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].path(), "/lookup");
    assert!(asked[0].query().unwrap().contains("id=22&"), "{}", asked[0]);
    // Found once, known after.
    directory.feed_of(&leases, &table, "jp", 22).await.unwrap();
    assert_eq!(table.count(), 1);
}

#[tokio::test]
async fn a_show_the_lookup_gives_no_feed_for_is_no_feed() {
    let leases = leases().await;
    let table = Table::new(|_| {
        Ok(br#"{"results": [{"collectionId": 5, "collectionName": "No feed"}]}"#.to_vec())
    });
    let err = Directory::default()
        .feed_of(&leases, &table, "fr", 5)
        .await
        .unwrap_err();
    assert!(matches!(err, PodcastError::NoFeed), "{err:?}");
    let table = Table::new(|_| Ok(br#"{"results": []}"#.to_vec()));
    let err = Directory::default()
        .feed_of(&leases, &table, "fr", 5)
        .await
        .unwrap_err();
    assert!(matches!(err, PodcastError::NoFeed), "{err:?}");
}

#[tokio::test]
async fn a_feed_the_directory_places_on_a_local_network_is_refused() {
    let leases = leases().await;
    for address in [
        "http://192.168.1.10/feed.xml",
        "http://127.0.0.1:8080/feed.xml",
        "http://localhost/feed.xml",
        "http://router.local/feed.xml",
        "http://[::1]/feed.xml",
        "http://10.0.0.2/feed.xml",
    ] {
        let body = format!(
            r#"{{"results": [{{"collectionId": 9, "collectionName": "Local", "feedUrl": "{address}"}}]}}"#
        );
        let table = Table::new(move |_| Ok(body.clone().into_bytes()));
        let err = Directory::default()
            .feed_of(&leases, &table, "fr", 9)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PodcastError::Failed(FetchFailure::AddressRefused)),
            "{address}: {err:?}"
        );
    }
}

// ── pictures ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_picture_is_a_data_address_kept_in_memory() {
    let leases = leases().await;
    let image = png();
    let table = Table::new(move |url| {
        if url.host_str() == Some("is1-ssl.mzstatic.com") {
            Ok(image.clone())
        } else {
            directory_answers(url)
        }
    });
    let directory = Directory::default();
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    let asked = table.count();
    let picture = directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .unwrap();
    assert!(picture.starts_with("data:image/png;base64,"), "{picture}");
    assert_eq!(table.count(), asked + 1);
    assert_eq!(
        table.asked().last().unwrap().as_str(),
        "https://is1-ssl.mzstatic.com/a/300x300bb.png"
    );
    directory.picture(&leases, &table, 11).await.unwrap();
    assert_eq!(table.count(), asked + 1, "a picture kept asks nothing");
    // A show with no picture, and one never met, ask nothing either.
    assert!(directory
        .picture(&leases, &table, 22)
        .await
        .unwrap()
        .is_none());
    assert!(directory
        .picture(&leases, &table, 999)
        .await
        .unwrap()
        .is_none());
    assert_eq!(table.count(), asked + 1);
}

#[tokio::test]
async fn what_is_not_an_image_is_no_picture_and_is_not_asked_again() {
    let leases = leases().await;
    let table = Table::new(|url| {
        if url.host_str() == Some("is1-ssl.mzstatic.com") {
            Ok(b"<html>not an image</html>".to_vec())
        } else {
            directory_answers(url)
        }
    });
    let directory = Directory::default();
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    let asked = table.count();
    assert!(directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .is_none());
    assert!(directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .is_none());
    assert_eq!(table.count(), asked + 1);
}

#[tokio::test]
async fn a_picture_refused_for_now_is_asked_again_later() {
    let leases = leases().await;
    let table = Table::new(|url| {
        if url.host_str() == Some("is1-ssl.mzstatic.com") {
            Err(FetchError::Failed(FetchFailure::Status(429)))
        } else {
            directory_answers(url)
        }
    });
    let directory = Directory::default();
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    let asked = table.count();
    assert!(directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .is_none());
    assert!(directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .is_none());
    assert_eq!(table.count(), asked + 2);
}

#[tokio::test]
async fn a_picture_the_host_could_not_serve_now_is_asked_again_and_a_gone_one_is_not() {
    for (status, kept) in [(500, false), (503, false), (404, true), (410, true)] {
        let leases = leases().await;
        let table = Table::new(move |url| {
            if url.host_str() == Some("is1-ssl.mzstatic.com") {
                Err(FetchError::Failed(FetchFailure::Status(status)))
            } else {
                directory_answers(url)
            }
        });
        let directory = Directory::default();
        directory.chart(&leases, &table, "fr", None).await.unwrap();
        let asked = table.count();
        assert!(directory
            .picture(&leases, &table, 11)
            .await
            .unwrap()
            .is_none());
        assert!(directory
            .picture(&leases, &table, 11)
            .await
            .unwrap()
            .is_none());
        let expected = if kept { asked + 1 } else { asked + 2 };
        assert_eq!(table.count(), expected, "{status}");
    }
}

#[tokio::test]
async fn a_kept_answer_records_its_shows_again() {
    let leases = leases().await;
    let image = png();
    let table = Table::new(move |url| {
        if url.host_str() == Some("is1-ssl.mzstatic.com") {
            Ok(image.clone())
        } else {
            directory_answers(url)
        }
    });
    let directory = Directory::default();
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    directory.forget_shows();
    // Served from what was kept, and the show's picture and feed are known again.
    directory.chart(&leases, &table, "fr", None).await.unwrap();
    assert!(directory
        .picture(&leases, &table, 11)
        .await
        .unwrap()
        .is_some());
    let asked = table.count();
    directory.feed_of(&leases, &table, "fr", 11).await.unwrap();
    assert_eq!(table.count(), asked, "the feed came from the kept chart");
}

#[test]
fn a_data_address_is_made_only_of_an_accepted_image() {
    assert!(data_url(&png())
        .unwrap()
        .starts_with("data:image/png;base64,iVBORw0KGgo"));
    assert_eq!(data_url(b""), None);
    assert_eq!(
        data_url(b"GIF89a"),
        None,
        "a header with no image behind it"
    );
    assert_eq!(data_url(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
}

// ── the budget ──────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn past_its_budget_a_request_waits_for_the_window_to_move() {
    let directory = Directory::default();
    for _ in 0..MAX_PER_WINDOW {
        directory.paced().await;
    }
    let start = tokio::time::Instant::now();
    let waiting = tokio::spawn(async move {
        directory.paced().await;
        tokio::time::Instant::now()
    });
    let done = waiting.await.unwrap();
    assert_eq!(done.duration_since(start), WINDOW);
}

#[tokio::test(start_paused = true)]
async fn requests_spread_over_the_window_do_not_wait() {
    let directory = Directory::default();
    let start = tokio::time::Instant::now();
    for _ in 0..(MAX_PER_WINDOW * 3) {
        directory.paced().await;
        tokio::time::sleep(WINDOW / MAX_PER_WINDOW as u32).await;
    }
    let spent = tokio::time::Instant::now().duration_since(start);
    assert!(spent <= WINDOW * 3 + Duration::from_secs(1), "{spent:?}");
}

#[tokio::test]
async fn a_kept_search_expires() {
    let leases = leases().await;
    let table = Table::new(directory_answers);
    let directory = Directory::default();
    directory.search(&leases, &table, "fr", "a").await.unwrap();
    directory.age_answers(SEARCH_TTL - Duration::from_secs(5));
    directory.search(&leases, &table, "fr", "a").await.unwrap();
    assert_eq!(table.count(), 1);
    directory.age_answers(Duration::from_secs(10));
    directory.search(&leases, &table, "fr", "a").await.unwrap();
    assert_eq!(table.count(), 2);
}
