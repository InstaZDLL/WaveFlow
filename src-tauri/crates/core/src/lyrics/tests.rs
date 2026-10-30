use crate::lyrics::ttml::{self, Timing, WriteOptions};
use crate::lyrics::{
    lrc, parse_any, word_stamp_present, AgentKind, Document, ExportError, Format, Line,
    Translation, Word,
};

/// A word-timed duet in the shape music services serve: agents in the
/// head, keyed translations and transliterations, backing vocals, a
/// syllable split. The text is invented.
const SERVICE_SHAPED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" itunes:timing="Word" xml:lang="ja">
  <head>
    <metadata>
      <ttm:agent type="person" xml:id="v1"/>
      <ttm:agent type="person" xml:id="v2"><ttm:name type="full">Second</ttm:name></ttm:agent>
      <ttm:agent type="group" xml:id="v1000"/>
      <iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal">
        <translations>
          <translation type="subtitle" xml:lang="en">
            <text for="L1">sky blue</text>
            <text for="L2">far away</text>
          </translation>
        </translations>
        <transliterations>
          <transliteration xml:lang="ja-Latn">
            <text for="L1"><span begin="1.0" end="1.5">sora</span> <span begin="1.5" end="2.4">aoi</span></text>
          </transliteration>
        </transliterations>
        <songwriters><songwriter>Writer One</songwriter></songwriters>
      </iTunesMetadata>
    </metadata>
  </head>
  <body dur="6.000">
    <div begin="1.0" end="6.0" itunes:songPart="Verse">
      <p begin="1.0" end="2.4" itunes:key="L1" ttm:agent="v1"><span begin="1.0" end="1.5">空</span> <span begin="1.5" end="1.9">青</span><span begin="1.9" end="2.4">い</span></p>
      <p begin="00:03.000" end="00:06.000" itunes:key="L2" ttm:agent="v2"><span begin="3.0" end="4.0">遠く</span> <span ttm:role="x-bg"><span begin="4.2" end="5.0">(echo</span> <span begin="5.0" end="6.0">echo)</span></span></p>
    </div>
  </body>
</tt>"#;

#[test]
fn reads_a_service_shaped_document() {
    let doc = ttml::parse(SERVICE_SHAPED).unwrap();
    assert_eq!(doc.metadata.language.as_deref(), Some("ja"));
    assert_eq!(doc.metadata.songwriters, vec!["Writer One"]);
    assert_eq!(doc.agents.len(), 3);
    assert_eq!(doc.agents[1].name.as_deref(), Some("Second"));
    assert_eq!(doc.agents[2].kind, AgentKind::Group);

    let first = &doc.lines[0];
    assert_eq!(first.begin_ms, Some(1000));
    assert_eq!(first.agent.as_deref(), Some("v1"));
    assert_eq!(first.part.as_deref(), Some("Verse"));
    assert_eq!(first.text(), "空 青い");
    assert!(first.words[1].joined, "adjacent spans are syllables");
    assert_eq!(first.translations[0].lang.as_deref(), Some("en"));
    assert_eq!(first.translations[0].text, "sky blue");
    assert_eq!(first.romanization.as_deref(), Some("sora aoi"));

    let second = &doc.lines[1];
    assert_eq!(second.begin_ms, Some(3000));
    assert_eq!(second.agent.as_deref(), Some("v2"));
    assert_eq!(second.text(), "遠く");
    assert_eq!(second.background.len(), 2);
    assert_eq!(second.background[0].begin_ms, Some(4200));
    assert_eq!(
        crate::lyrics::model::join_words(&second.background),
        "(echo echo)"
    );
    assert!(doc.is_word_synced());
}

#[test]
fn word_timed_round_trip_keeps_everything() {
    let doc = ttml::parse(SERVICE_SHAPED).unwrap();
    let xml = ttml::write(&doc, &WriteOptions::default()).unwrap();
    assert!(xml.contains("itunes:timing=\"Word\""));
    let again = ttml::parse(&xml).unwrap();
    assert_eq!(again, doc);
    // And writing is stable.
    assert_eq!(ttml::write(&again, &WriteOptions::default()).unwrap(), xml);
}

#[test]
fn line_timed_documents_round_trip() {
    let doc = lrc::parse("[00:01.00]first line\n[00:03.50]second & <last>\n");
    let xml = ttml::write(&doc, &WriteOptions::default()).unwrap();
    assert!(xml.contains("itunes:timing=\"Line\""));
    assert!(xml.contains("second &amp; &lt;last&gt;"));
    let again = ttml::parse(&xml).unwrap();
    assert_eq!(again.lines.len(), 2);
    assert_eq!(again.lines[1].text(), "second & <last>");
    assert_eq!(again.lines[0].begin_ms, Some(1000));
    assert_eq!(again.lines[0].end_ms, Some(3500));
    assert!(!again.is_word_synced());
}

#[test]
fn forcing_word_timing_names_the_gap() {
    let doc = lrc::parse("[00:01.00]two words\n");
    let options = WriteOptions {
        timing: Timing::Word,
    };
    assert_eq!(
        ttml::write(&doc, &options).unwrap_err(),
        ExportError::UnsyncedWord { line: 0, word: 1 }
    );
}

#[test]
fn undeclared_singers_are_declared() {
    let doc = Document {
        lines: vec![Line {
            begin_ms: Some(0),
            agent: Some("v2".into()),
            words: vec![Word::new("hi")],
            ..Line::default()
        }],
        ..Document::default()
    };
    let xml = ttml::write(&doc, &WriteOptions::default()).unwrap();
    assert!(xml.contains("<ttm:agent type=\"person\" xml:id=\"v2\"/>"));
}

#[test]
fn inline_translation_spans_are_read() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body><div>
      <p begin="1s" end="2s"><span begin="1s" end="2s">hola</span><span ttm:role="x-translation" xml:lang="en">hello</span><span ttm:role="x-roman">ola</span></p>
    </div></body></tt>"#;
    let doc = ttml::parse(xml).unwrap();
    assert_eq!(doc.lines[0].text(), "hola");
    assert_eq!(
        doc.lines[0].translations,
        vec![Translation {
            lang: Some("en".into()),
            text: "hello".into()
        }]
    );
    assert_eq!(doc.lines[0].romanization.as_deref(), Some("ola"));
}

#[test]
fn rejects_what_is_not_ttml() {
    assert!(matches!(
        ttml::parse("<html/>"),
        Err(crate::lyrics::ParseError::NotTtml(_))
    ));
    assert!(matches!(
        ttml::parse("<tt>"),
        Err(crate::lyrics::ParseError::Xml(_))
    ));
}

#[test]
fn json_project_round_trip() {
    let doc = ttml::parse(SERVICE_SHAPED).unwrap();
    let json = serde_json::to_string(&doc).unwrap();
    assert!(json.contains("\"beginMs\""));
    let back: Document = serde_json::from_str(&json).unwrap();
    assert_eq!(back, doc);
}

#[test]
fn parse_any_dispatches() {
    let (format, doc) = parse_any(SERVICE_SHAPED).unwrap();
    assert_eq!(format, Format::Ttml);
    assert_eq!(doc.lines.len(), 2);
}

#[test]
fn detect_format_plain() {
    let sample = "This is just\nsome text without any timestamps.";
    assert_eq!(Format::detect(sample), Format::Plain);
}

#[test]
fn detect_format_lrc() {
    let sample = "[ar:Some Artist]\n[ti:Some Title]\n[00:01.00]First line\n[00:05.50]Second line";
    assert_eq!(Format::detect(sample), Format::Lrc);
}

#[test]
fn detect_format_enhanced_lrc() {
    let sample =
        "[00:01.00]<00:01.00>Hello <00:01.50>world\n[00:03.00]<00:03.00>Another <00:03.40>line";
    assert_eq!(Format::detect(sample), Format::EnhancedLrc);
}

#[test]
fn detect_format_enhanced_lrc_no_colon_frac() {
    let sample = "[00:01.00]<00:01>plain stamps still count";
    assert_eq!(Format::detect(sample), Format::EnhancedLrc);
}

#[test]
fn detect_format_ttml_xml_decl() {
    let sample = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body>
    <div>
      <p begin="00:00:01.000" end="00:00:03.000">
        <span begin="00:00:01.000" end="00:00:01.500">Hello</span>
        <span begin="00:00:01.500" end="00:00:03.000">world</span>
      </p>
    </div>
  </body>
</tt>"#;
    assert_eq!(Format::detect(sample), Format::Ttml);
    // A byte-order mark in front changes nothing.
    assert_eq!(Format::detect(&format!("\u{feff}{sample}")), Format::Ttml);
}

#[test]
fn detect_format_ttml_no_decl() {
    let sample =
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="0s">x</p></div></body></tt>"#;
    assert_eq!(Format::detect(sample), Format::Ttml);
}

#[test]
fn detect_format_brackets_but_no_timestamp_stays_plain() {
    // A line starting with `[foo]` (LRC metadata header) without
    // any actual time-stamped line should NOT be classified as
    // synchronized.
    let sample = "[ar:Artist]\n[ti:Title]\nVerse without timestamps.";
    assert_eq!(Format::detect(sample), Format::Plain);
    assert_eq!(Format::detect("[chorus]\nla la"), Format::Plain);
}

#[test]
fn word_stamp_present_basic() {
    assert!(word_stamp_present("<00:01.50>word"));
    assert!(word_stamp_present("plain<5:00>more"));
    assert!(!word_stamp_present("nothing here"));
    assert!(!word_stamp_present("<not:a:stamp>"));
}

#[test]
fn formats_serialize_as_the_payloads_spell_them() {
    assert_eq!(
        serde_json::to_string(&Format::EnhancedLrc).unwrap(),
        "\"enhanced_lrc\""
    );
    assert_eq!(
        serde_json::from_str::<Format>("\"ttml\"").unwrap(),
        Format::Ttml
    );
}

#[test]
fn backing_vocals_alone_in_a_p_join_the_line_before() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata"><body><div>
      <p begin="1s" end="2s"><span begin="1s" end="2s">lead</span></p>
      <p begin="2s" end="3s"><span ttm:role="x-bg"><span begin="2s" end="3s">(echo)</span></span></p>
    </div></body></tt>"#;
    let doc = ttml::parse(xml).unwrap();
    assert_eq!(doc.lines.len(), 1);
    assert_eq!(doc.lines[0].background[0].text, "(echo)");
    assert_eq!(doc.lines[0].end_ms, Some(3000));
    let again = ttml::parse(&ttml::write(&doc, &WriteOptions::default()).unwrap()).unwrap();
    assert_eq!(again.lines[0].background.len(), 1);
}

#[test]
fn an_empty_line_does_not_turn_word_timing_into_line_timing() {
    let mut doc = lrc::parse(
        "[00:01.00]<00:01.00>a <00:01.50>b <00:02.00>
",
    );
    // A line the editor added and left empty: no format writes it.
    doc.lines.push(Line::default());
    assert!(doc.is_word_synced());
    assert!(doc.is_line_synced());
    let xml = ttml::write(&doc, &WriteOptions::default()).unwrap();
    assert!(xml.contains("itunes:timing=\"Word\""));
    assert!(!Document::default().is_line_synced());
    assert!(!Document {
        lines: vec![Line::default()],
        ..Document::default()
    }
    .is_word_synced());
}

#[test]
fn a_half_word_synced_document_keeps_its_word_timing_in_ttml() {
    let mut doc = lrc::parse(
        "[00:01.00]<00:01.00>a <00:01.50>b <00:02.00>
[00:03.00]c d
",
    );
    assert!(!doc.is_word_synced());
    doc.lines[1].end_ms = Some(4000);
    let xml = ttml::write(&doc, &WriteOptions::default()).unwrap();
    assert!(xml.contains("itunes:timing=\"Word\""));
    let again = ttml::parse(&xml).unwrap();
    assert!(again.lines[0].is_word_timed());
    assert_eq!(again.lines[0].words[1].begin_ms, Some(1500));
    assert_eq!(again.lines[1].begin_ms, Some(3000));
    assert!(!again.lines[1].is_word_timed());
}
