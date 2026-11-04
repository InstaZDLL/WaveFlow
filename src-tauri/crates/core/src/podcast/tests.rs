use url::Url;

use super::address::*;
use super::chapters::{self, ChapterSource};
use super::feed::{parse_date, parse_duration, parse_feed};
use super::opml::{read_opml, write_opml};
use super::transcript;
use super::*;

fn base() -> Url {
    Url::parse("https://feeds.example.org/show.xml").unwrap()
}

// ── the feed ────────────────────────────────────────────────────────

#[test]
fn an_rss_feed_is_read_newest_first_and_an_item_without_audio_is_skipped() {
    let feed = parse_feed(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
  <channel>
    <title>The Show</title>
    <description><![CDATA[<p>About <b>things</b></p>]]></description>
    <link>https://example.org/</link>
    <language>fr</language>
    <itunes:author>Ana</itunes:author>
    <itunes:image href="https://example.org/cover.jpg"/>
    <item>
      <title>First</title>
      <guid>ep-1</guid>
      <pubDate>Mon, 01 Jan 2024 10:00:00 +0000</pubDate>
      <itunes:duration>1:02:03</itunes:duration>
      <itunes:season>2</itunes:season>
      <itunes:episode>7</itunes:episode>
      <enclosure url="/audio/1.mp3" type="audio/mpeg" length="1234"/>
    </item>
    <item>
      <title>Second</title>
      <pubDate>Tue, 02 Jan 2024 10:00:00 +0000</pubDate>
      <enclosure url="https://cdn.example.org/2.mp3" type="audio/mpeg"/>
    </item>
    <item><title>No audio</title></item>
  </channel>
</rss>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.title, "The Show");
    assert_eq!(feed.description.as_deref(), Some("About things"));
    assert_eq!(feed.author.as_deref(), Some("Ana"));
    assert_eq!(feed.language.as_deref(), Some("fr"));
    assert_eq!(
        feed.image_url.as_deref(),
        Some("https://example.org/cover.jpg")
    );
    assert_eq!(feed.episodes.len(), 2);
    assert_eq!(feed.episodes[0].title, "Second");
    assert_eq!(feed.episodes[0].guid, "https://cdn.example.org/2.mp3");
    assert_eq!(
        feed.episodes[1].enclosure_url,
        "https://feeds.example.org/audio/1.mp3"
    );
    assert_eq!(feed.episodes[1].duration_ms, Some(3_723_000));
    assert_eq!(feed.episodes[1].enclosure_length, Some(1234));
    assert_eq!(feed.episodes[1].season, Some(2));
    assert_eq!(feed.episodes[1].episode_number, Some(7));
}

#[test]
fn a_feed_may_say_it_has_moved() {
    let moved = |address: &str| {
        let document = format!(
            "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
             <title>T</title><itunes:new-feed-url>{address}</itunes:new-feed-url>\
             </channel></rss>"
        );
        parse_feed(document.as_bytes(), &base())
            .unwrap()
            .new_feed_url
    };
    assert_eq!(
        moved("https://new.example.org/feed").as_deref(),
        Some("https://new.example.org/feed")
    );
    assert_eq!(
        moved("/elsewhere.xml").as_deref(),
        Some("https://feeds.example.org/elsewhere.xml")
    );
    assert_eq!(moved("file:///etc/passwd"), None);
    assert_eq!(moved("https://user:pw@new.example.org/"), None);
}

/// An address in a text node is judged whole. Cut to a bound, one that is too long
/// would become an address the feed never named; decoded a second time, its query
/// would change.
#[test]
fn an_address_in_a_text_node_is_neither_cut_nor_decoded_twice() {
    let site = |address: &str| {
        let document =
            format!("<rss><channel><title>T</title><link>{address}</link></channel></rss>");
        parse_feed(document.as_bytes(), &base()).unwrap().site_url
    };
    let too_long = format!("https://example.org/{}", "a".repeat(MAX_URL_CHARS));
    assert_eq!(site(&too_long), None);
    assert_eq!(
        site("  https://example.org/?a=1&amp;lt=2  ").as_deref(),
        Some("https://example.org/?a=1&lt=2")
    );
    let feed = parse_feed(
        format!(
            "<feed xmlns=\"http://www.w3.org/2005/Atom\"><title>T</title><logo>{too_long}</logo>\
             <icon>https://example.org/icon.png</icon></feed>"
        )
        .as_bytes(),
        &base(),
    )
    .unwrap();
    assert_eq!(feed.image_url, None, "a logo too long is refused, not cut");
}

#[test]
fn an_atom_feed_is_read() {
    let feed = parse_feed(
        br#"<feed xmlns="http://www.w3.org/2005/Atom" xml:lang="en">
  <title>Atom Show</title>
  <author><name>Bo</name><email>bo@example.org</email></author>
  <entry>
    <id>urn:1</id><title>One</title><updated>2024-03-01T00:00:00Z</updated>
    <link rel="enclosure" href="https://cdn.example.org/1.ogg" type="audio/ogg"/>
  </entry>
</feed>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.title, "Atom Show");
    assert_eq!(feed.author.as_deref(), Some("Bo"));
    assert_eq!(feed.language.as_deref(), Some("en"));
    assert_eq!(feed.episodes[0].guid, "urn:1");
    assert_eq!(feed.episodes[0].published_at, Some(1_709_251_200_000));
}

/// RSS 1.0 spells its enclosure's attributes in namespaces of their own.
#[test]
fn an_rss_1_0_feed_is_read() {
    let feed = parse_feed(
        br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
    xmlns="http://purl.org/rss/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:enc="http://purl.oclc.org/net/rss_2.0/enc#">
  <channel rdf:about="https://example.org/"><title>RDF Show</title><dc:creator>Cy</dc:creator></channel>
  <image rdf:about="x"><url>https://example.org/c.png</url></image>
  <item rdf:about="https://example.org/1">
    <title>One</title><dc:date>2024-03-01T00:00:00Z</dc:date>
    <enc:enclosure rdf:resource="https://cdn.example.org/1.mp3" enc:type="audio/mpeg" enc:length="9"/>
  </item>
</rdf:RDF>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.title, "RDF Show");
    assert_eq!(feed.author.as_deref(), Some("Cy"));
    assert_eq!(feed.image_url.as_deref(), Some("https://example.org/c.png"));
    assert_eq!(
        feed.episodes[0].enclosure_url,
        "https://cdn.example.org/1.mp3"
    );
    assert_eq!(
        feed.episodes[0].enclosure_type.as_deref(),
        Some("audio/mpeg")
    );
    assert_eq!(feed.episodes[0].enclosure_length, Some(9));
}

#[test]
fn an_episode_keeps_its_chapters_and_transcripts() {
    let feed = parse_feed(
        br#"<rss xmlns:podcast="https://podcastindex.org/namespace/1.0"
              xmlns:psc="http://podlove.org/simple-chapters">
<channel><title>T</title>
<item><enclosure url="https://cdn.example.org/1.mp3"/>
  <podcast:chapters url="/1/chapters.json" type="application/json+chapters"/>
  <podcast:transcript url="https://t.example.org/1.txt" type="text/plain" language="fr"/>
  <podcast:transcript url="https://t.example.org/1.vtt" type="text/vtt"/>
  <podcast:transcript url="https://t.example.org/1.pdf" type="application/pdf"/>
  <podcast:transcript url="javascript:alert(1)" type="text/vtt"/>
  <psc:chapters version="1.2">
    <psc:chapter start="00:05:00.000" title="Later"/>
    <psc:chapter start="00:00:00" title="&lt;b&gt;Start&lt;/b&gt;" href="https://evil.example/" image="x"/>
    <psc:chapter start="nonsense" title="Dropped"/>
    <psc:chapter start="00:01:00"/>
  </psc:chapters>
</item>
<item><enclosure url="https://cdn.example.org/2.mp3"/>
  <podcast:chapters url="https://x.example.org/2.xml" type="text/xml"/>
</item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    let first = feed
        .episodes
        .iter()
        .find(|e| e.enclosure_url.ends_with("1.mp3"))
        .unwrap();
    assert_eq!(
        first.chapters_url.as_deref(),
        Some("https://feeds.example.org/1/chapters.json")
    );
    assert_eq!(
        first.inline_chapters,
        [
            Chapter {
                start_ms: 0,
                title: "Start".into()
            },
            Chapter {
                start_ms: 300_000,
                title: "Later".into()
            }
        ]
    );
    assert_eq!(first.transcripts.len(), 2);
    assert_eq!(first.transcripts[0].kind, TranscriptKind::Text);
    assert_eq!(first.transcripts[0].language.as_deref(), Some("fr"));
    assert_eq!(first.transcripts[1].kind, TranscriptKind::Vtt);
    let second = feed
        .episodes
        .iter()
        .find(|e| e.enclosure_url.ends_with("2.mp3"))
        .unwrap();
    assert_eq!(second.chapters_url, None, "chapters that are not JSON");
}

// ── hostile input ───────────────────────────────────────────────────

/// The billion laughs, and its quieter relatives: any DTD is refused, so no
/// entity is declared and nothing expands.
#[test]
fn a_document_type_declaration_is_refused_outright() {
    let laughs = br#"<?xml version="1.0"?>
<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]>
<rss><channel><title>&lol2;</title></channel></rss>"#;
    assert_eq!(parse_feed(laughs, &base()), Err(FeedError::Invalid));
    let external = br#"<?xml version="1.0"?>
<!DOCTYPE rss [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
<rss><channel><title>&xxe;</title></channel></rss>"#;
    assert_eq!(parse_feed(external, &base()), Err(FeedError::Invalid));
    assert_eq!(read_opml(external), None);
    assert_eq!(parse_feed(b"not xml", &base()), Err(FeedError::Invalid));
    assert_eq!(
        parse_feed(b"<html><body/></html>", &base()),
        Err(FeedError::Invalid)
    );
    assert_eq!(
        parse_feed(b"<rss version=\"2.0\"></rss>", &base()),
        Err(FeedError::Invalid),
        "rss with no channel"
    );
}

#[test]
fn an_entity_not_declared_refuses_the_document() {
    assert_eq!(
        parse_feed(
            b"<rss><channel><title>&nope;</title></channel></rss>",
            &base()
        ),
        Err(FeedError::Invalid)
    );
}

#[test]
fn too_many_bytes_or_nodes_are_refused() {
    let oversized = vec![b' '; MAX_FEED_BYTES + 1];
    assert_eq!(parse_feed(&oversized, &base()), Err(FeedError::Invalid));
    assert_eq!(read_opml(&oversized), None);
    // Twelve bytes a node, inside the byte bound and past the node ceiling.
    let items = "<item></item>".repeat(1_100_000);
    let document = format!("<rss><channel>{items}</channel></rss>");
    assert!(document.len() < MAX_FEED_BYTES);
    assert_eq!(
        parse_feed(document.as_bytes(), &base()),
        Err(FeedError::Invalid)
    );
}

/// Neither `roxmltree` nor anything here recurses on depth: this comes back,
/// rather than taking the stack with it, and the text inside is still read.
#[test]
fn a_very_deep_document_comes_back() {
    let opening = "<a>".repeat(200_000);
    let closing = "</a>".repeat(200_000);
    let document = format!("<rss><channel><title>{opening}x{closing}</title></channel></rss>");
    assert_eq!(parse_feed(document.as_bytes(), &base()).unwrap().title, "x");
}

#[test]
fn encodings_are_believed_only_when_the_bytes_agree() {
    let latin = b"<?xml version=\"1.0\" encoding = 'ISO-8859-1'?>\
        <rss><channel><title>Caf\xe9</title></channel></rss>";
    assert_eq!(parse_feed(latin, &base()).unwrap().title, "Café");

    // A BOM wins over what the declaration says.
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "<rss><channel><title>Thé</title></channel></rss>".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    assert_eq!(parse_feed(&utf16, &base()).unwrap().title, "Thé");

    // An encoding nobody knows falls back to UTF-8, and is read when it is.
    let unknown = "<?xml version=\"1.0\" encoding=\"klingon\"?>\
        <rss><channel><title>Thé</title></channel></rss>";
    assert_eq!(
        parse_feed(unknown.as_bytes(), &base()).unwrap().title,
        "Thé"
    );

    // Bytes that are not what is claimed are refused rather than guessed at.
    let lying = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <rss><channel><title>Caf\xe9</title></channel></rss>";
    assert_eq!(parse_feed(lying, &base()), Err(FeedError::Invalid));
    let undeclared = b"<rss><channel><title>Caf\xe9</title></channel></rss>";
    assert_eq!(parse_feed(undeclared, &base()), Err(FeedError::Invalid));
    // An encoding that only ever decodes to replacement characters.
    let replacement = b"<?xml version=\"1.0\" encoding=\"iso-2022-kr\"?><rss/>";
    assert_eq!(parse_feed(replacement, &base()), Err(FeedError::Invalid));
}

#[test]
fn impossible_dates_are_no_date_and_sort_last() {
    assert_eq!(
        parse_date("Mon, 01 Jan 2024 10:00:00 +0000"),
        Some(1_704_103_200_000)
    );
    for impossible in [
        "",
        "yesterday",
        "Mon, 31 Feb 2024 10:00:00 +0000",
        "2024-02-31T00:00:00Z",
        "9999-12-31T23:59:59Z",
        "1001-01-01T00:00:00Z",
        "Mon, 01 Jan 99999 10:00:00 +0000",
    ] {
        assert_eq!(parse_date(impossible), None, "{impossible}");
    }
    let feed = parse_feed(
        br#"<rss><channel><title>T</title>
<item><guid>a</guid><pubDate>not a date</pubDate><enclosure url="https://c.example.org/a"/></item>
<item><guid>b</guid><pubDate>Mon, 01 Jan 2024 10:00:00 +0000</pubDate><enclosure url="https://c.example.org/b"/></item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.episodes[0].guid, "b");
    assert_eq!(feed.episodes[1].published_at, None);
}

#[test]
fn duplicate_guids_keep_the_newest() {
    let feed = parse_feed(
        br#"<rss><channel><title>T</title>
<item><guid>same</guid><title>Old</title><pubDate>Mon, 01 Jan 2024 10:00:00 +0000</pubDate>
  <enclosure url="https://c.example.org/old"/></item>
<item><guid>same</guid><title>New</title><pubDate>Tue, 02 Jan 2024 10:00:00 +0000</pubDate>
  <enclosure url="https://c.example.org/new"/></item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.episodes.len(), 1);
    assert_eq!(feed.episodes[0].title, "New");
}

#[test]
fn an_enclosure_that_is_not_http_or_not_audio_is_not_an_episode() {
    let feed = parse_feed(
        br#"<rss><channel><title>T</title>
<item><enclosure url="file:///etc/passwd"/></item>
<item><enclosure url="javascript:alert(1)"/></item>
<item><enclosure url="https://user:pw@c.example.org/creds.mp3"/></item>
<item><enclosure url="https://c.example.org/v.mp4" type="video/mp4"/></item>
<item><enclosure url="https://c.example.org/p.pdf" type="application/pdf"/></item>
<item><enclosure url="https://c.example.org/h.html" type="text/html"/></item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    assert!(feed.episodes.is_empty());
    let both = parse_feed(
        br#"<rss><channel><title>T</title>
<item><enclosure url="https://c.example.org/v.mp4" type="video/mp4"/>
<enclosure url="https://c.example.org/a.mp3" type="audio/mpeg"/></item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(
        both.episodes[0].enclosure_url,
        "https://c.example.org/a.mp3"
    );
}

#[test]
fn nothing_from_a_feed_stays_markup() {
    let feed = parse_feed(
        br#"<rss><channel>
<title>&lt;script&gt;alert(1)&lt;/script&gt;Show</title>
<description><![CDATA[<img src=x onerror=alert(1)><p>Fine &amp; dandy</p><script>steal()</script>]]></description>
<item><title><![CDATA[<a href="javascript:x">Linked</a> title]]></title>
<enclosure url="https://c.example.org/a"/></item>
</channel></rss>"#,
        &base(),
    )
    .unwrap();
    assert_eq!(feed.title, "Show");
    assert_eq!(feed.description.as_deref(), Some("Fine & dandy"));
    assert_eq!(feed.episodes[0].title, "Linked title");
}

/// Text is cut where it passes its bound, and a single text node of many
/// megabytes costs what the bound says rather than what the feed said.
#[test]
fn long_texts_are_cut_where_they_pass_their_bound() {
    let description = "word ".repeat(1_600);
    let items: String = (0..MAX_EPISODES_PER_FEED + 5)
        .map(|index| {
            format!(
                "<item><title>Episode {index}</title><description>{description}</description>\
                     <enclosure url=\"https://cdn.example.org/{index}.mp3\"/></item>"
            )
        })
        .collect();
    let document = format!("<rss><channel><title>Long</title>{items}</channel></rss>");
    let feed = parse_feed(document.as_bytes(), &base()).unwrap();
    assert_eq!(feed.episodes.len(), MAX_EPISODES_PER_FEED);
    let kept = feed.episodes[0].description.as_deref().unwrap();
    assert_eq!(kept.chars().count(), 3_999, "trimmed of its last space");

    let huge = "é".repeat(7 * 1024 * 1024);
    let document = format!("<rss><channel><title>{huge}</title></channel></rss>");
    let feed = parse_feed(document.as_bytes(), &base()).unwrap();
    assert_eq!(feed.title.chars().count(), 500);
    let long_address = format!(
        "<rss><channel><title>T</title><item>\
         <enclosure url=\"https://c.example.org/{}\"/></item></channel></rss>",
        "a".repeat(MAX_URL_CHARS)
    );
    assert!(parse_feed(long_address.as_bytes(), &base())
        .unwrap()
        .episodes
        .is_empty());
}

#[test]
fn durations_take_the_three_spellings() {
    assert_eq!(parse_duration("90"), Some(90_000));
    assert_eq!(parse_duration("01:30"), Some(90_000));
    assert_eq!(parse_duration("1:00:00"), Some(3_600_000));
    for impossible in [
        "x",
        "1:2:3:4",
        "-5",
        "0",
        "NaN",
        "inf",
        "1e300",
        "999999999",
        "",
    ] {
        assert_eq!(parse_duration(impossible), None, "{impossible}");
    }
}

// ── addresses and the words of a failure ────────────────────────────

#[test]
fn an_address_is_absolute_http_and_carries_no_credentials() {
    assert!(parse_address("https://example.org/feed").is_some());
    assert!(parse_address("  http://example.org/feed  ").is_some());
    assert_eq!(
        parse_address("https://example.org/feed#frag")
            .unwrap()
            .as_str(),
        "https://example.org/feed"
    );
    for refused in [
        "",
        "https://user:secret@example.org/feed",
        "https://user@example.org/feed",
        "file:///etc/passwd",
        "ftp://example.org/feed",
        "/feed.xml",
        "javascript:alert(1)",
        "data:text/plain,hi",
    ] {
        assert!(parse_address(refused).is_none(), "{refused}");
    }
    assert!(parse_address(&format!("https://e.org/{}", "a".repeat(MAX_URL_CHARS))).is_none());
}

/// A secure address may move anywhere secure, a clear one anywhere; a
/// secure one may not move into the clear. A scheme-relative target
/// (`//host/path`) keeps the scheme it came from, so it never downgrades.
#[test]
fn a_redirect_never_leaves_https_for_http() {
    let secure = Url::parse("https://feeds.example.org/show.xml?auth=t").unwrap();
    let clear = Url::parse("http://feeds.example.org/show.xml").unwrap();
    let hop = |from: &Url, to: &str| next_hop(from, to).map(String::from);
    assert_eq!(
        hop(&secure, "https://cdn.example.org/show.xml").unwrap(),
        "https://cdn.example.org/show.xml"
    );
    assert_eq!(
        hop(&secure, "/moved.xml").unwrap(),
        "https://feeds.example.org/moved.xml"
    );
    assert_eq!(
        hop(&secure, "//cdn.example.org/show.xml").unwrap(),
        "https://cdn.example.org/show.xml"
    );
    assert_eq!(
        hop(&clear, "//cdn.example.org/show.xml").unwrap(),
        "http://cdn.example.org/show.xml"
    );
    assert_eq!(
        hop(&clear, "https://cdn.example.org/show.xml").unwrap(),
        "https://cdn.example.org/show.xml"
    );
    for refused in [
        "http://cdn.example.org/show.xml",
        "HTTP://cdn.example.org/show.xml",
        "file:///etc/passwd",
        "https://user:pw@cdn.example.org/",
    ] {
        assert_eq!(
            hop(&secure, refused),
            Err(FetchFailure::AddressRefused),
            "{refused}"
        );
    }
}

/// The list the RFC fixes: nothing else may ever be stored or logged.
#[test]
fn a_failure_is_one_word_from_the_list() {
    for (failure, word) in [
        (FetchFailure::AddressRefused, "address_refused"),
        (FetchFailure::TooManyRedirects, "too_many_redirects"),
        (FetchFailure::TooLarge, "too_large"),
        (FetchFailure::Unreached, "unreached"),
        (FetchFailure::Status(404), "http_404"),
        (FetchFailure::InvalidFeed, "invalid_feed"),
        (FetchFailure::NotAudio, "not_audio"),
        (FetchFailure::QuotaExceeded, "quota_exceeded"),
    ] {
        assert_eq!(failure.code(), word);
    }
}

#[test]
fn keys_hold_nothing_of_what_they_stand_for() {
    let url = Url::parse("https://feeds.example.org/private/s3cret/feed.xml").unwrap();
    let key = feed_key(&url);
    assert_eq!(key.len(), 64);
    assert!(!key.contains("s3cret"));
    assert_eq!(key, feed_key(&url));
    assert_ne!(item_key("a"), item_key("b"));
}

#[test]
fn audio_is_recognised_by_its_bytes() {
    assert_eq!(sniff_audio(b"ID3\x04\0\0").map(|f| f.0), Some("mp3"));
    assert_eq!(
        sniff_audio(&[0xff, 0xfb, 0x90, 0x00]).map(|f| f.0),
        Some("mp3")
    );
    assert_eq!(sniff_audio(b"\0\0\0\x20ftypM4A ").map(|f| f.0), Some("m4a"));
    assert_eq!(sniff_audio(b"OggS\0\x02").map(|f| f.0), Some("ogg"));
    assert_eq!(sniff_audio(b"<html>").map(|f| f.0), None);
    assert_eq!(sniff_audio(b"MZ\x90\0").map(|f| f.0), None);
}

// ── OPML ────────────────────────────────────────────────────────────

#[test]
fn an_opml_file_gives_its_feeds_however_they_nest() {
    let entries = read_opml(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="1.0"><head><title>Mine</title></head><body>
  <outline text="Folder">
    <outline type="rss" text="Nested" xmlUrl="https://a.example.org/feed"/>
    <outline text="Deeper"><outline title="&lt;b&gt;Deep&lt;/b&gt;" xmlUrl="http://b.example.org/feed"/></outline>
  </outline>
  <outline type="rss" text="Duplicate" xmlUrl="https://a.example.org/feed"/>
  <outline type="rss" xmlUrl="https://c.example.org/feed"/>
  <outline type="rss" text="Evil" xmlUrl="file:///etc/passwd"/>
  <outline type="rss" text="Creds" xmlUrl="https://u:p@d.example.org/feed"/>
  <outline type="link" text="Not a feed" url="https://e.example.org/"/>
</body></opml>"#,
    )
    .unwrap();
    assert_eq!(
        entries,
        [
            OpmlEntry {
                title: Some("Nested".into()),
                feed_url: "https://a.example.org/feed".into()
            },
            OpmlEntry {
                title: Some("Deep".into()),
                feed_url: "http://b.example.org/feed".into()
            },
            OpmlEntry {
                title: None,
                feed_url: "https://c.example.org/feed".into()
            },
        ]
    );
    assert_eq!(read_opml(b"<rss><channel/></rss>"), None);
    assert_eq!(read_opml(b"not xml"), None);
}

#[test]
fn what_opml_writes_it_reads_back_whatever_the_title_holds() {
    let entries = vec![
        OpmlEntry {
            title: Some("Fish & \"Chips\" <live> 'n\u{7} stuff".into()),
            feed_url: "https://a.example.org/feed?x=1&y=2".into(),
        },
        OpmlEntry {
            title: None,
            feed_url: "https://b.example.org/feed".into(),
        },
    ];
    let written = write_opml(&entries);
    assert!(!written.contains('\u{7}'));
    let read = read_opml(written.as_bytes()).unwrap();
    assert_eq!(read[0].feed_url, entries[0].feed_url);
    assert_eq!(read[1], entries[1]);
    // Reading strips markup from a title, as it does from any feed text, so
    // what could pass for a tag does not survive the trip. The rest does.
    assert_eq!(read[0].title.as_deref(), Some("Fish & \"Chips\" 'n stuff"));
}

#[test]
fn opml_entries_are_bounded() {
    let outlines: String = (0..2_100)
        .map(|index| format!("<outline xmlUrl=\"https://e.example.org/{index}\"/>"))
        .collect();
    let document = format!("<opml><body>{outlines}</body></opml>");
    assert_eq!(read_opml(document.as_bytes()).unwrap().len(), 2_000);
}

// ── chapters ────────────────────────────────────────────────────────

#[test]
fn a_chapter_file_is_read_sorted_with_the_impossible_dropped() {
    let chapters = chapters::read_json(
        br#"{"version":"1.2.0","chapters":[
            {"startTime":60,"title":"Second","img":"https://evil.example/x.png","url":"https://evil.example/"},
            {"startTime":0.5,"title":"First"},
            {"startTime":-1,"title":"Negative"},
            {"startTime":1e300,"title":"Far away"},
            {"startTime":90,"title":"Hidden","toc":false},
            {"startTime":120},
            {"startTime":"130","title":"A string is not a time"},
            {"title":"No time"},
            {"startTime":180,"title":"   "},
            {"startTime":60,"title":"Same time as Second"}
        ]}"#,
    )
    .unwrap();
    assert_eq!(
        chapters,
        [
            Chapter {
                start_ms: 500,
                title: "First".into()
            },
            Chapter {
                start_ms: 60_000,
                title: "Second".into()
            },
        ]
    );
    assert_eq!(chapters::read_json(b"[]"), None);
    assert_eq!(chapters::read_json(b"{\"chapters\":3}"), None);
    assert_eq!(chapters::read_json(b"not json"), None);
    assert_eq!(
        chapters::read_json(&vec![b' '; MAX_CHAPTERS_BYTES + 1]),
        None
    );
    assert_eq!(chapters::read_json("[".repeat(100_000).as_bytes()), None);
}

#[test]
fn chapters_past_the_stream_are_dropped_when_its_length_is_known() {
    let chapters = vec![
        Chapter {
            start_ms: 1_000,
            title: "In".into(),
        },
        Chapter {
            start_ms: 9_000,
            title: "Past the end".into(),
        },
    ];
    assert_eq!(chapters::normalize(chapters.clone(), None).len(), 2);
    assert_eq!(chapters::normalize(chapters, Some(5_000)).len(), 1);
}

/// JSON, then inline, then `CHAP`: the first with a valid chapter wins, whole,
/// and sources are never merged.
#[test]
fn the_first_source_with_a_valid_chapter_wins_alone() {
    let one = |title: &str| {
        vec![Chapter {
            start_ms: 0,
            title: title.into(),
        }]
    };
    let invalid = vec![Chapter {
        start_ms: u64::MAX,
        title: "impossible".into(),
    }];
    let (source, chosen) = chapters::choose(Some(one("json")), one("inline"), one("id3")).unwrap();
    assert_eq!(
        (source, chosen[0].title.as_str()),
        (ChapterSource::Json, "json")
    );
    let (source, _) = chapters::choose(None, one("inline"), one("id3")).unwrap();
    assert_eq!(source, ChapterSource::Inline);
    let (source, _) = chapters::choose(Some(vec![]), vec![], one("id3")).unwrap();
    assert_eq!(source, ChapterSource::Id3);
    let (source, _) = chapters::choose(Some(invalid), one("inline"), vec![]).unwrap();
    assert_eq!(source, ChapterSource::Inline);
    assert_eq!(chapters::choose(None, vec![], vec![]), None);
}

// ── transcripts ─────────────────────────────────────────────────────

#[test]
fn transcripts_prefer_timed_formats_in_the_rfcs_order() {
    let reference = |kind: TranscriptKind, name: &str| TranscriptRef {
        url: format!("https://t.example.org/{name}"),
        kind,
        language: None,
    };
    let all = [
        reference(TranscriptKind::Html, "h"),
        reference(TranscriptKind::Text, "t"),
        reference(TranscriptKind::TranscriptJson, "j"),
        reference(TranscriptKind::Srt, "s"),
        reference(TranscriptKind::Vtt, "v1"),
        reference(TranscriptKind::Vtt, "v2"),
    ];
    assert!(transcript::choose(&all).unwrap().url.ends_with("v1"));
    assert!(transcript::choose(&all[..4]).unwrap().url.ends_with('s'));
    assert!(transcript::choose(&all[..3]).unwrap().url.ends_with('j'));
    assert!(transcript::choose(&all[..2]).unwrap().url.ends_with('h'));
    assert!(transcript::choose(&[]).is_none());
}

#[test]
fn a_transcript_kind_comes_from_its_type_then_its_extension() {
    let kind = |declared: Option<&str>, url: &str| TranscriptKind::recognise(declared, url);
    let url = "https://t.example.org/a.vtt?token=x";
    assert_eq!(
        kind(Some("text/vtt; charset=utf-8"), url),
        Some(TranscriptKind::Vtt)
    );
    assert_eq!(kind(None, url), Some(TranscriptKind::Vtt));
    assert_eq!(
        kind(Some("application/x-subrip"), url),
        Some(TranscriptKind::Srt),
        "the declared type wins"
    );
    assert_eq!(kind(Some("application/pdf"), url), None);
    assert_eq!(kind(None, "https://t.example.org/a"), None);
    assert!(TranscriptKind::Srt.is_timed());
    assert!(!TranscriptKind::Html.is_timed());
}

#[test]
fn an_untimed_transcript_becomes_lines_without_markup() {
    let html = transcript::read(
        TranscriptKind::Html,
        b"<html><head><style>p{}</style><title>T</title></head><body>\
          <p>First <b>line</b></p><script>steal()</script><p>Second&nbsp;line</p></body></html>",
    )
    .unwrap();
    let lines: Vec<String> = html.lines.iter().map(|l| l.text()).collect();
    assert_eq!(lines, ["T", "First line", "Second line"]);
    assert!(!html.is_line_synced());

    let text = transcript::read(TranscriptKind::Text, b"one\n\n  two  \n").unwrap();
    assert_eq!(text.lines.len(), 2);
}

#[test]
fn a_transcript_is_refused_when_it_is_too_large_or_not_what_it_says() {
    let oversized = vec![b' '; MAX_TRANSCRIPT_BYTES + 1];
    for kind in [
        TranscriptKind::Vtt,
        TranscriptKind::Srt,
        TranscriptKind::TranscriptJson,
        TranscriptKind::Text,
        TranscriptKind::Html,
    ] {
        assert!(transcript::read(kind, &oversized).is_err());
    }
    assert!(transcript::read(TranscriptKind::Vtt, b"<html>404</html>").is_err());
    assert!(transcript::read(TranscriptKind::Srt, b"<html>404</html>").is_err());
    assert!(transcript::read(TranscriptKind::TranscriptJson, b"<html>").is_err());
    // A timed transcript in a legacy encoding is still read.
    let (latin, _, _) =
        encoding_rs::WINDOWS_1252.encode("WEBVTT\n\n00:01.000 --> 00:02.000\nCafé\n");
    let document = transcript::read(TranscriptKind::Vtt, &latin).unwrap();
    assert_eq!(document.lines[0].text(), "Café");
}

// ── channel pictures ────────────────────────────────────────────────

#[test]
fn a_cover_is_an_image_of_a_sane_size() {
    let encode = |width, height| {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    };
    assert!(accepts_cover(&encode(8, 8)));
    assert!(!accepts_cover(b"<html>not an image</html>"));
    assert!(!accepts_cover(
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"
    ));
    assert!(!accepts_cover(b""));

    // A small file that declares an enormous image: only its header is read, so
    // this is refused without allocating what it asks for.
    let mut bomb = encode(8, 8);
    bomb[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    assert!(!accepts_cover(&bomb));
}
