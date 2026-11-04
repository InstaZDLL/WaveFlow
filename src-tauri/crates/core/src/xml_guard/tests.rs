use std::time::{Duration, Instant};

use url::Url;

use super::*;
use crate::lyrics::{ttml, ParseError};
use crate::podcast::feed::{parse_feed, FeedError};
use crate::podcast::opml::read_opml;

fn base() -> Url {
    Url::parse("https://feeds.example.org/show.xml").unwrap()
}

/// `<rss a0="" a1="" …>` holding about this many bytes of attributes.
fn attributes_on_the_root(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    out.push_str("<rss");
    let mut index = 0;
    while out.len() < bytes {
        out.push_str(&format!(" a{index}=\"\""));
        index += 1;
    }
    out.push_str("><channel><title>T</title></channel></rss>");
    out
}

/// A root declaring `root` namespaces over `children` elements declaring one more.
fn namespaces_over_children(root: usize, children: usize) -> String {
    let declarations: String = (0..root)
        .map(|index| format!(" xmlns:n{index}=\"u\""))
        .collect();
    let elements = "<b xmlns:z=\"u\"/>".repeat(children);
    format!("<rss{declarations}><channel><title>T</title>{elements}</channel></rss>")
}

/// The documents that took 13 s, 66 s and 12 s in a release build. The bound is for a
/// debug build on a loaded machine, and still far below what they would cost parsed.
#[test]
fn the_documents_that_stalled_the_parser_are_refused_at_once() {
    let documents = [
        attributes_on_the_root(930 * 1024),
        attributes_on_the_root(1_930 * 1024),
        namespaces_over_children(5_000, 400),
    ];
    let started = Instant::now();
    for document in &documents {
        assert_eq!(
            parse_feed(document.as_bytes(), &base()),
            Err(FeedError::Invalid)
        );
        assert_eq!(read_opml(document.as_bytes()), None);
        assert!(matches!(ttml::parse(document), Err(ParseError::Xml(_))));
    }
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn each_limit_sits_exactly_where_it_says() {
    let attributes = |count: usize| {
        let list: String = (0..count).map(|index| format!(" a{index}=\"\"")).collect();
        format!("<rss{list}/>")
    };
    assert!(within_limits(&attributes(MAX_ATTRIBUTES_PER_TAG)));
    assert!(!within_limits(&attributes(MAX_ATTRIBUTES_PER_TAG + 1)));

    let scoped = |count: usize| namespaces_over_children(count, 0);
    assert!(within_limits(&scoped(MAX_NAMESPACES_IN_SCOPE)));
    assert!(!within_limits(&scoped(MAX_NAMESPACES_IN_SCOPE + 1)));
    // Declared a few at a time down a chain, they are still all in scope.
    let chain = |depth: usize| {
        let opening: String = (0..depth)
            .map(|index| format!("<a xmlns:n{index}=\"u\">"))
            .collect();
        format!("{opening}{}", "</a>".repeat(depth))
    };
    assert!(within_limits(&chain(MAX_NAMESPACES_IN_SCOPE)));
    assert!(!within_limits(&chain(MAX_NAMESPACES_IN_SCOPE + 1)));
    // A sibling that has closed takes what it declared with it.
    let siblings = "<a xmlns:n=\"u\"></a>".repeat(MAX_NAMESPACES_IN_SCOPE * 2);
    assert!(within_limits(&format!("<rss>{siblings}</rss>")));

    // The root copies its 9, and each child the 10 it has in scope.
    let copies = |children: usize| namespaces_over_children(9, children);
    let fits = (MAX_NAMESPACE_COPIES - 9) / 10;
    assert!(within_limits(&copies(fits)));
    assert!(!within_limits(&copies(fits + 1)));
}

/// What is not markup does not count, and what is cannot hide.
#[test]
fn the_scan_reads_tags_as_the_parser_does() {
    let many: String = (0..200).map(|index| format!(" a{index}=\"\"")).collect();
    for quiet in [
        format!("<rss><!-- <b{many}/> --></rss>"),
        format!("<rss><![CDATA[<b{many}/>]]></rss>"),
        format!("<?xml version=\"1.0\"?><?pi <b{many}/> ?><rss/>"),
        format!("<rss x='<b{many}/>'/>"),
        "<rss x='>' y=\"'\"><title>a &lt; b</title></rss>".to_owned(),
        "<rss\n\tx = \"1\"\r\n y\n=\n'2' />".to_owned(),
    ] {
        assert!(within_limits(&quiet), "{}", &quiet[..30.min(quiet.len())]);
    }
    // An attribute after a value holding `>` is still an attribute of that tag.
    assert!(!within_limits(&format!("<rss x=\">\"{many}/>")));
    for broken in [
        "<",
        "< a/>",
        "<a",
        "<a b",
        "<a b=",
        "<a b=c/>",
        "<a b=\"",
        "<a /",
        "<a / >",
        "<!-- never closed",
        "<![CDATA[ never closed",
        "<? never closed",
        "</ never closed",
        "<!DOCTYPE a><a/>",
    ] {
        assert!(!within_limits(broken), "{broken}");
    }
    assert!(within_limits(""));
    assert!(within_limits("no markup at all"));
}

/// Nothing shaped to make the scan itself slow does: five megabytes of each.
#[test]
fn the_scan_is_linear_whatever_it_is_given() {
    let started = Instant::now();
    for shape in [
        "<a>",
        "<a b=\"\"/>",
        "</a>",
        "<!---->",
        "<?a?>",
        "<![CDATA[]]>",
        "x",
    ] {
        let input = shape.repeat(5 * 1024 * 1024 / shape.len());
        let _ = within_limits(&input);
    }
    assert!(started.elapsed() < Duration::from_secs(20));
}

/// The real documents' shapes pass: a feed with its usual namespaces, one declaring
/// one on every item, and lyrics.
#[test]
fn real_shapes_are_read() {
    let items = "<item xmlns:itunes=\"u\"><enclosure url=\"https://c.example.org/a\" \
                 type=\"audio/mpeg\" length=\"1\"/><guid>a</guid></item>"
        .repeat(3_000);
    let document = format!(
        "<rss version=\"2.0\" xmlns:atom=\"a\" xmlns:content=\"b\" xmlns:googleplay=\"c\" \
         xmlns:media=\"d\" xmlns:psc=\"e\" xmlns:podcast=\"f\" xmlns:omny=\"g\" xmlns:acast=\"h\">\
         <channel><title>T</title>{items}</channel></rss>"
    );
    assert_eq!(
        parse_feed(document.as_bytes(), &base())
            .unwrap()
            .episodes
            .len(),
        1
    );
    let lyrics = "<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:itunes=\"i\" xmlns:ttm=\"t\" \
                  xml:lang=\"en\"><head><metadata><ttm:agent type=\"person\" xml:id=\"v1\"/>\
                  </metadata></head><body><div><p begin=\"0s\" end=\"1s\" itunes:key=\"L1\" \
                  ttm:agent=\"v1\"><span begin=\"0s\" end=\"1s\">Word</span></p></div></body></tt>";
    assert_eq!(ttml::parse(lyrics).unwrap().lines.len(), 1);
}

/// What the limits still let through costs little: as many attributes as a tag may
/// have, on every tag of the largest feed read. The bound is for a debug build.
#[test]
fn the_worst_document_inside_the_limits_stays_cheap() {
    let list: String = (0..MAX_ATTRIBUTES_PER_TAG)
        .map(|index| format!(" a{index}=\"\""))
        .collect();
    let element = format!("<b{list}/>");
    let count = (crate::podcast::MAX_FEED_BYTES - 100) / element.len();
    let document = format!(
        "<rss><channel><title>T</title>{}</channel></rss>",
        element.repeat(count)
    );
    let started = Instant::now();
    assert!(parse_feed(document.as_bytes(), &base()).is_ok());
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    println!("worst inside the limits: {:?}", started.elapsed());
}
