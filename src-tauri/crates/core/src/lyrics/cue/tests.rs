use super::*;
use crate::lyrics::{srt, transcript_json, vtt, ParseError};

fn texts(document: &Document) -> Vec<String> {
    document.lines.iter().map(Line::text).collect()
}

#[test]
fn a_clock_takes_its_spellings_and_refuses_the_impossible() {
    assert_eq!(parse_clock("00:01.500"), Some(1_500));
    assert_eq!(parse_clock("01:00:02,250"), Some(3_602_250));
    assert_eq!(parse_clock("00:02"), Some(2_000));
    assert_eq!(parse_clock("00:02.5"), Some(2_500));
    assert_eq!(parse_clock("00:02.123456"), Some(2_123));
    for impossible in [
        "",
        "12",
        "00:60.000",
        "60:00.000",
        "-1:00.000",
        "aa:bb.ccc",
        "00:01.xyz",
        "101:00:00.000",
        "99999999999999999999:00:00.000",
        "1:2:3:4",
    ] {
        assert_eq!(parse_clock(impossible), None, "{impossible}");
    }
}

#[test]
fn webvtt_keeps_cues_speakers_and_order() {
    let document = vtt::parse(
        "\u{feff}WEBVTT - a title\nKind: captions\n\n\
         NOTE a comment\nwith two lines\n\n\
         STYLE\n::cue { color: red }\n\n\
         late\n00:00:05.000 --> 00:00:06.000 align:start\n<v Ana>Second</v> line\n\n\
         00:01.000 --> 00:02.000\n<c.yellow>First</c> <00:01.500>one &amp; only\n\n\
         00:07.000 --> 00:03.000\nend before start\n\n\
         oops --> nope\nnot a cue\n",
    )
    .unwrap();
    assert_eq!(
        texts(&document),
        ["First one & only", "Second line", "end before start"]
    );
    assert_eq!(document.lines[0].begin_ms, Some(1_000));
    assert_eq!(document.lines[0].end_ms, Some(2_000));
    assert_eq!(
        document.lines[2].end_ms, None,
        "an end before its start is no end"
    );
    assert_eq!(document.lines[1].agent.as_deref(), Some("v1"));
    assert_eq!(document.agents[0].name.as_deref(), Some("Ana"));
    assert!(document.is_line_synced());
}

#[test]
fn what_is_not_webvtt_is_refused() {
    for body in ["", "<html><body>404</body></html>", "WEBVTTX\n\n", "{}"] {
        assert_eq!(vtt::parse(body), Err(ParseError::NotTimedText), "{body}");
    }
    assert_eq!(vtt::parse("WEBVTT").unwrap().lines.len(), 0);
}

#[test]
fn srt_is_read_and_loses_its_formatting() {
    let document = srt::parse(
        "1\r\n00:00:01,000 --> 00:00:02,000\r\n<i>One</i> {\\an8}line\r\nand more\r\n\r\n\
         2\r\n00:00:00,500 --> 00:00:00,900\r\nEarlier\r\n\r\n\
         3\r\nbroken --> time\r\nnot kept\r\n",
    )
    .unwrap();
    assert_eq!(texts(&document), ["Earlier", "One line and more"]);
    assert_eq!(
        srt::parse("<html>404</html>"),
        Err(ParseError::NotTimedText)
    );
    assert_eq!(srt::parse(""), Err(ParseError::NotTimedText));
    assert_eq!(strip_for_test("{\\unclosed"), "{\\unclosed");
}

fn strip_for_test(text: &str) -> String {
    srt::parse(&format!("00:01.000 --> 00:02.000\n{text}\n"))
        .unwrap()
        .lines[0]
        .text()
}

#[test]
fn a_json_transcript_of_sentences_stays_as_written() {
    let document = transcript_json::parse(
        r#"{"version":"1.0.0","segments":[
            {"speaker":"Ana","startTime":3.0,"endTime":4.5,"body":"A later sentence."},
            {"speaker":"Bo","startTime":1.0,"endTime":2.5,"body":"<b>An</b> earlier one."},
            {"speaker":"Ana","startTime":-1,"body":"Negative is impossible."},
            {"speaker":"Ana","startTime":1e300,"body":"So is this."},
            {"startTime":5,"body":""},
            {"body":"No time."}
        ]}"#,
    )
    .unwrap();
    assert_eq!(texts(&document), ["An earlier one.", "A later sentence."]);
    assert_eq!(document.lines[0].begin_ms, Some(1_000));
    assert_eq!(document.lines[0].agent.as_deref(), Some("v1"));
    assert_eq!(document.lines[1].agent.as_deref(), Some("v2"));
}

#[test]
fn a_transcript_of_single_words_is_gathered_into_lines() {
    let words = |list: &[(f64, &str)]| {
        let segments: Vec<String> = list
            .iter()
            .map(|(at, body)| {
                format!(
                    r#"{{"speaker":"Ana","startTime":{at},"endTime":{},"body":"{body}"}}"#,
                    at + 0.2
                )
            })
            .collect();
        format!(r#"{{"segments":[{}]}}"#, segments.join(","))
    };
    let document = transcript_json::parse(&words(&[
        (0.0, "Hello"),
        (0.3, "there"),
        (0.5, ","),
        (0.6, "world."),
        (0.9, "Next"),
        (5.0, "after"),
        (5.2, "silence"),
    ]))
    .unwrap();
    assert_eq!(
        texts(&document),
        ["Hello there, world.", "Next", "after silence"]
    );
    assert!(document.lines[0].is_word_timed());
    assert_eq!(document.lines[0].end_ms, Some(800));
}

#[test]
fn what_is_not_a_json_transcript_is_refused() {
    for body in ["", "[]", "{}", r#"{"segments":3}"#, "<html/>"] {
        assert_eq!(
            transcript_json::parse(body),
            Err(ParseError::NotTimedText),
            "{body}"
        );
    }
    let deep = "[".repeat(100_000);
    assert_eq!(transcript_json::parse(&deep), Err(ParseError::NotTimedText));
}

#[test]
fn cues_and_speakers_are_bounded() {
    let mut body = String::from("WEBVTT\n\n");
    for index in 0..(MAX_CUES + 10) {
        body.push_str(&format!("00:00.000 --> 00:01.000\n<v S{index}>x\n\n"));
    }
    let document = vtt::parse(&body).unwrap();
    assert_eq!(document.lines.len(), MAX_CUES);
    assert_eq!(document.agents.len(), MAX_SPEAKERS);
    assert!(document.lines.last().unwrap().agent.is_none());
}
