use super::directory::*;

/// A search answer shaped as the directory writes one, two shows trimmed to
/// what is read.
const SEARCH: &str = r#"{
  "resultCount": 2,
  "results": [
    {
      "wrapperType": "track", "kind": "podcast",
      "collectionId": 1200361736, "trackId": 1200361736,
      "artistName": "The Daily Desk",
      "collectionName": "The Daily",
      "feedUrl": "https://feeds.example.com/Sl5CSM3S",
      "artworkUrl30": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/aa/30x30bb.jpg",
      "artworkUrl100": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/aa/100x100bb.jpg",
      "artworkUrl600": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/aa/600x600bb.jpg",
      "collectionExplicitness": "cleaned",
      "trackCount": 2412,
      "primaryGenreName": "Daily News",
      "genreIds": ["1526", "26", "1489"]
    },
    {
      "wrapperType": "track", "kind": "podcast",
      "collectionId": "1583548883",
      "artistName": "Radio",
      "collectionName": "Entrez dans l&#39;Histoire",
      "feedUrl": "http://feeds.example.fr/histoire.xml",
      "artworkUrl100": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/bb/100x100bb.png",
      "collectionExplicitness": "explicit",
      "trackCount": 900,
      "genreIds": ["1487", "26"]
    }
  ]
}"#;

/// A chart answer, two entries trimmed to what is read.
const CHART: &str = r#"{
  "feed": {
    "author": {"name": {"label": "iTunes Store"}},
    "entry": [
      {
        "im:name": {"label": "Les Grosses Têtes"},
        "im:image": [
          {"label": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/fe/55x55bb.png", "attributes": {"height": "55"}},
          {"label": "https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/fe/170x170bb.png", "attributes": {"height": "170"}}
        ],
        "im:artist": {"label": "RTL"},
        "id": {"label": "https://podcasts.apple.com/fr/podcast/id369369012?uo=2", "attributes": {"im:id": "369369012"}},
        "category": {"attributes": {"im:id": "1303", "term": "Comedy", "label": "Humour"}}
      },
      {
        "im:name": {"label": "Entrez dans l'Histoire"},
        "im:artist": {"label": "RTL"},
        "id": {"attributes": {"im:id": "1583548883"}},
        "category": {"attributes": {"im:id": "1487"}}
      }
    ]
  }
}"#;

fn search(json: &str) -> Vec<DirectoryShow> {
    parse_search(json.as_bytes()).expect("a search answer")
}

fn chart(json: &str) -> Vec<DirectoryShow> {
    parse_chart(json.as_bytes()).expect("a chart answer")
}

/// A search answer holding `results` as written.
fn results(results: &str) -> String {
    format!(r#"{{"resultCount": 1, "results": [{results}]}}"#)
}

// ── a search ────────────────────────────────────────────────────────

#[test]
fn a_search_keeps_each_shows_feed_picture_genre_and_count() {
    let shows = search(SEARCH);
    assert_eq!(shows.len(), 2);
    let daily = &shows[0];
    assert_eq!(daily.id, 1_200_361_736);
    assert_eq!(daily.title, "The Daily");
    assert_eq!(daily.author.as_deref(), Some("The Daily Desk"));
    assert_eq!(
        daily.feed_url.as_deref(),
        Some("https://feeds.example.com/Sl5CSM3S")
    );
    // The first genre it knows: 1526 is a sub-genre and 26 is "Podcasts".
    assert_eq!(daily.genre_id, Some(1489));
    assert_eq!(
        daily.artwork_url.as_deref(),
        Some("https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/aa/300x300bb.jpg")
    );
    assert_eq!(daily.episode_count, Some(2412));
    assert!(!daily.explicit);

    let history = &shows[1];
    // An id written as a string, an entity decoded once, a feed over http kept.
    assert_eq!(history.id, 1_583_548_883);
    assert_eq!(history.title, "Entrez dans l'Histoire");
    assert_eq!(
        history.feed_url.as_deref(),
        Some("http://feeds.example.fr/histoire.xml")
    );
    assert_eq!(history.genre_id, Some(1487));
    assert!(history.explicit);
}

#[test]
fn a_lookup_is_read_as_a_search() {
    let shows = search(&results(
        r#"{"kind": "podcast", "collectionId": 7, "collectionName": "Seven", "feedUrl": "https://x.example/f"}"#,
    ));
    assert_eq!(shows[0].feed_url.as_deref(), Some("https://x.example/f"));
}

#[test]
fn a_result_without_an_id_or_a_title_is_not_a_show() {
    let shows = search(&results(
        r#"{"collectionName": "No id"},
           {"collectionId": 0, "collectionName": "Zero"},
           {"collectionId": -4, "collectionName": "Negative"},
           {"collectionId": 1.5, "collectionName": "Fraction"},
           {"collectionId": "12a", "collectionName": "Letters"},
           {"collectionId": "99999999999999999", "collectionName": "Too long"},
           {"collectionId": 9007199254740992, "collectionName": "Past a double"},
           {"collectionId": 3},
           {"collectionId": 4, "collectionName": "   "},
           {"collectionId": 5, "collectionName": "<b></b>"},
           {"collectionId": 6, "collectionName": ["a list"]},
           {"collectionId": 8, "collectionName": "Kept"}"#,
    ));
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].id, 8);
}

#[test]
fn something_other_than_a_podcast_is_skipped() {
    let shows = search(&results(
        r#"{"kind": "song", "collectionId": 1, "collectionName": "A song"},
           {"kind": "podcast", "collectionId": 2, "collectionName": "A show"},
           {"collectionId": 3, "collectionName": "No kind said"}"#,
    ));
    let ids: Vec<u64> = shows.iter().map(|show| show.id).collect();
    assert_eq!(ids, [2, 3]);
}

#[test]
fn a_field_of_the_wrong_type_is_absent_and_the_show_stays() {
    let shows = search(&results(
        r#"{"collectionId": 1, "collectionName": "Odd",
            "artistName": 12, "feedUrl": {"href": "https://x.example/f"},
            "artworkUrl100": false, "trackCount": "many", "genreIds": "1489",
            "collectionExplicitness": 1}"#,
    ));
    let show = &shows[0];
    assert_eq!(show.title, "Odd");
    assert_eq!(show.author, None);
    assert_eq!(show.feed_url, None);
    assert_eq!(show.artwork_url, None);
    assert_eq!(show.episode_count, None);
    assert_eq!(show.genre_id, None);
    assert!(!show.explicit);
}

#[test]
fn a_feed_address_that_no_fetch_may_try_is_dropped() {
    for address in [
        "ftp://feeds.example.com/f.xml",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "https://user:secret@feeds.example.com/f.xml",
        "/relative/feed.xml",
        "",
    ] {
        let json = results(&format!(
            r#"{{"collectionId": 1, "collectionName": "Show", "feedUrl": {}}}"#,
            serde_json::to_string(address).unwrap()
        ));
        let shows = search(&json);
        assert_eq!(shows[0].feed_url, None, "{address}");
    }
    let long = format!("https://x.example/{}", "a".repeat(3_000));
    let shows = search(&results(&format!(
        r#"{{"collectionId": 1, "collectionName": "Show", "feedUrl": "{long}"}}"#
    )));
    assert_eq!(shows[0].feed_url, None);
}

#[test]
fn a_title_is_one_line_of_plain_text_of_bounded_length() {
    let huge = "x".repeat(1_000_000);
    let shows = search(&results(&format!(
        r#"{{"collectionId": 1, "collectionName": "<script>alert(1)</script><b>Bold</b>\n\tnews\u0007"}},
           {{"collectionId": 2, "collectionName": "{huge}", "artistName": "{huge}"}}"#
    )));
    assert_eq!(shows[0].title, "Bold news");
    assert_eq!(shows[1].title.chars().count(), 300);
    assert_eq!(shows[1].author.as_ref().unwrap().chars().count(), 300);
}

#[test]
fn a_show_listed_twice_is_kept_once_and_at_most_two_hundred_are_kept() {
    let mut many: Vec<String> = (1..=300)
        .map(|id| format!(r#"{{"collectionId": {id}, "collectionName": "Show {id}"}}"#))
        .collect();
    many.insert(
        1,
        r#"{"collectionId": 1, "collectionName": "Again"}"#.to_owned(),
    );
    let shows = search(&results(&many.join(",")));
    assert_eq!(shows.len(), MAX_DIRECTORY_RESULTS);
    assert_eq!(shows[0].title, "Show 1");
    assert_eq!(shows[1].id, 2);
}

// ── a chart ─────────────────────────────────────────────────────────

#[test]
fn a_chart_keeps_its_order_its_largest_picture_and_no_feed() {
    let shows = chart(CHART);
    assert_eq!(shows.len(), 2);
    assert_eq!(shows[0].id, 369_369_012);
    assert_eq!(shows[0].title, "Les Grosses Têtes");
    assert_eq!(shows[0].author.as_deref(), Some("RTL"));
    assert_eq!(shows[0].genre_id, Some(1303));
    assert_eq!(
        shows[0].artwork_url.as_deref(),
        Some("https://is1-ssl.mzstatic.com/image/thumb/Podcasts/v4/fe/300x300bb.png")
    );
    assert_eq!(shows[0].feed_url, None);
    assert_eq!(shows[1].id, 1_583_548_883);
    assert_eq!(shows[1].artwork_url, None);
}

#[test]
fn a_chart_of_one_entry_has_it_as_an_object() {
    let shows = chart(
        r#"{"feed": {"entry": {"im:name": {"label": "Alone"}, "id": {"attributes": {"im:id": "5"}}}}}"#,
    );
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].title, "Alone");
}

#[test]
fn an_empty_chart_has_no_entry() {
    assert!(chart(r#"{"feed": {"author": {}}}"#).is_empty());
    assert!(chart(r#"{"feed": {"entry": []}}"#).is_empty());
}

#[test]
fn a_chart_entry_without_an_id_or_a_name_is_skipped() {
    let shows = chart(
        r#"{"feed": {"entry": [
            {"im:name": {"label": "No id"}},
            {"im:name": {"label": "Id not in attributes"}, "id": {"label": "369"}},
            {"id": {"attributes": {"im:id": "7"}}},
            {"im:name": "not a label", "id": {"attributes": {"im:id": "8"}}},
            {"im:name": {"label": "Kept"}, "id": {"attributes": {"im:id": "9"}},
             "im:image": "not a list", "category": {"attributes": {"im:id": "26"}}},
            "a string",
            42
        ]}}"#,
    );
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].id, 9);
    assert_eq!(shows[0].artwork_url, None);
    assert_eq!(shows[0].genre_id, None, "26 is not a genre listed");
}

// ── what is not an answer ───────────────────────────────────────────

#[test]
fn what_is_not_the_shape_asked_for_is_invalid() {
    for body in [
        "",
        "not json",
        "[]",
        "null",
        "\"text\"",
        r#"{"results": {}}"#,
        r#"{"results": "none"}"#,
        r#"{"resultCount": 0}"#,
        "{\"results\": [",
        "\u{feff}{\"results\": []}",
    ] {
        assert_eq!(
            parse_search(body.as_bytes()),
            Err(DirectoryError::Invalid),
            "{body:?}"
        );
    }
    for body in [
        "[]",
        r#"{"feed": []}"#,
        r#"{"feed": {"entry": "x"}}"#,
        r#"{"other": {}}"#,
    ] {
        assert_eq!(
            parse_chart(body.as_bytes()),
            Err(DirectoryError::Invalid),
            "{body}"
        );
    }
    assert_eq!(
        parse_search(&[0xff, 0xfe, 0x00]),
        Err(DirectoryError::Invalid)
    );
}

#[test]
fn an_answer_too_large_is_refused_unread() {
    let mut body = br#"{"results": []}"#.to_vec();
    body.resize(MAX_DIRECTORY_BYTES + 1, b' ');
    assert_eq!(parse_search(&body), Err(DirectoryError::TooLarge));
    assert_eq!(parse_chart(&body), Err(DirectoryError::TooLarge));
}

#[test]
fn a_deep_nesting_is_refused_without_exhausting_the_stack() {
    // Run on a small stack, so a recursion that the parser's limit did not stop
    // would show here as a crash rather than pass on the test thread's 2 MiB.
    let outcome = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let depth = 100_000;
            let body = format!(
                r#"{{"results": [{{"collectionId": 1, "collectionName": "x", "genreIds": {}{}}}]}}"#,
                "[".repeat(depth),
                "]".repeat(depth)
            );
            (parse_search(body.as_bytes()), parse_chart(body.as_bytes()))
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(outcome.0, Err(DirectoryError::Invalid));
    assert_eq!(outcome.1, Err(DirectoryError::Invalid));
}

// ── pictures ────────────────────────────────────────────────────────

#[test]
fn a_picture_is_https_only_and_a_resizable_one_is_asked_at_its_thumbnail_size() {
    assert_eq!(
        thumbnail_url("https://is5-ssl.mzstatic.com/image/thumb/a/b/100x100bb.jpg").as_deref(),
        Some("https://is5-ssl.mzstatic.com/image/thumb/a/b/300x300bb.jpg")
    );
    assert_eq!(
        thumbnail_url("https://is5-ssl.mzstatic.com/image/thumb/a/b/600x600bb.webp").as_deref(),
        Some("https://is5-ssl.mzstatic.com/image/thumb/a/b/300x300bb.webp")
    );
    // Not the directory's host, or not a sized name: left as it is.
    for kept in [
        "https://cdn.example.com/a/100x100bb.jpg",
        "https://is5-ssl.mzstatic.com/image/thumb/a/cover.jpg",
        "https://is5-ssl.mzstatic.com/image/thumb/a/100x100bb.j/pg",
        "https://is5-ssl.mzstatic.com/image/thumb/a/12345x1bb.jpg",
        "https://is5-ssl.mzstatic.com/image/thumb/a/100x100bb.jpeg2",
        "https://mzstatic.com.example.org/a/100x100bb.jpg",
    ] {
        assert_eq!(thumbnail_url(kept).as_deref(), Some(kept), "{kept}");
    }
    for refused in [
        "http://is5.mzstatic.com/a/100x100bb.jpg",
        "https://user:pw@is5-ssl.mzstatic.com/a/100x100bb.jpg",
        "data:image/png;base64,AAAA",
        "javascript:alert(1)",
        "//is5-ssl.mzstatic.com/a.jpg",
    ] {
        assert_eq!(thumbnail_url(refused), None, "{refused}");
    }
}

// ── what is sent ────────────────────────────────────────────────────

#[test]
fn a_country_is_two_letters_in_lower_case() {
    assert_eq!(country_code("FR").as_deref(), Some("fr"));
    assert_eq!(country_code(" us ").as_deref(), Some("us"));
    for refused in ["", "f", "fra", "f1", "../", "é1", "f/", "zz/"] {
        assert_eq!(country_code(refused), None, "{refused}");
    }
}

#[test]
fn a_term_is_one_bounded_line() {
    assert_eq!(
        search_term("  le\n\tmonde \u{0}").as_deref(),
        Some("le monde")
    );
    assert_eq!(
        search_term(&"a".repeat(500)).unwrap().chars().count(),
        MAX_TERM_CHARS
    );
    assert_eq!(search_term(" \n "), None);
}

#[test]
fn a_term_stays_inside_its_parameter() {
    let url = search_url("crime & co&country=us#x", "fr");
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    assert!(pairs.contains(&("term".into(), "crime & co&country=us#x".into())));
    assert_eq!(
        pairs.iter().filter(|(key, _)| key == "country").count(),
        1,
        "{url}"
    );
    assert!(pairs.contains(&("country".into(), "fr".into())));
    assert_eq!(url.host_str(), Some("itunes.apple.com"));
    assert_eq!(url.fragment(), None);
}

#[test]
fn a_chart_is_narrowed_only_to_a_genre_it_knows() {
    assert_eq!(
        chart_url("fr", Some(1487)).as_str(),
        "https://itunes.apple.com/fr/rss/toppodcasts/limit=200/genre=1487/json"
    );
    assert_eq!(
        chart_url("jp", None).as_str(),
        "https://itunes.apple.com/jp/rss/toppodcasts/limit=200/json"
    );
    assert_eq!(
        chart_url("jp", Some(26)).as_str(),
        "https://itunes.apple.com/jp/rss/toppodcasts/limit=200/json"
    );
}

#[test]
fn a_lookup_asks_about_every_id_at_once_up_to_its_bound() {
    let url = lookup_url(&[1, 22, 333], "de");
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    assert!(pairs.contains(&("id".into(), "1,22,333".into())));
    assert!(pairs.contains(&("country".into(), "de".into())));
    let many: Vec<u64> = (1..=500).collect();
    let url = lookup_url(&many, "de");
    let ids = url
        .query_pairs()
        .find(|(key, _)| key == "id")
        .unwrap()
        .1
        .split(',')
        .count();
    assert_eq!(ids, MAX_LOOKUP_IDS);
}
