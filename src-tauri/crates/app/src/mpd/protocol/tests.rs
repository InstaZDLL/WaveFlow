use super::*;

#[test]
fn tokenizes_a_bare_command() {
    let (verb, args) = tokenize("status").unwrap();
    assert_eq!(verb, "status");
    assert!(args.is_empty());
}

#[test]
fn tokenizes_quoted_arguments_with_spaces() {
    let (verb, args) = tokenize(r#"find artist "Tyler, The Creator""#).unwrap();
    assert_eq!(verb, "find");
    assert_eq!(args, vec!["artist", "Tyler, The Creator"]);
}

#[test]
fn unescapes_inner_quotes_and_backslashes() {
    let (_, args) = tokenize(r#"cmd "a \"b\" c" "d\\e""#).unwrap();
    assert_eq!(args, vec![r#"a "b" c"#, r"d\e"]);
}

#[test]
fn blank_line_is_not_a_command() {
    assert!(tokenize("   ").is_none());
    assert!(parse("").is_none());
}

#[test]
fn parses_a_bounded_range() {
    assert_eq!(
        parse("delete 3:7"),
        Some(Command::Delete(Range::Bounded { start: 3, end: 7 }))
    );
}

#[test]
fn parses_an_open_ended_range() {
    assert_eq!(
        parse("delete 3:"),
        Some(Command::Delete(Range::From { start: 3 }))
    );
}

#[test]
fn play_minus_one_means_resume_not_position() {
    // Several clients send `play -1` for their play button; reading
    // it as a position would jump to a nonexistent track.
    assert_eq!(parse("play -1"), Some(Command::Play(None)));
    assert_eq!(parse("play"), Some(Command::Play(None)));
    assert_eq!(parse("play 4"), Some(Command::Play(Some(4))));
}

#[test]
fn seekcur_distinguishes_absolute_from_relative() {
    assert_eq!(
        parse("seekcur 30"),
        Some(Command::SeekCur {
            seconds: 30.0,
            relative: false
        })
    );
    assert_eq!(
        parse("seekcur +30"),
        Some(Command::SeekCur {
            seconds: 30.0,
            relative: true
        })
    );
    assert_eq!(
        parse("seekcur -10"),
        Some(Command::SeekCur {
            seconds: -10.0,
            relative: true
        })
    );
}

#[test]
fn single_is_tri_state() {
    assert_eq!(parse("single 0"), Some(Command::Single(SingleMode::Off)));
    assert_eq!(parse("single 1"), Some(Command::Single(SingleMode::On)));
    assert_eq!(
        parse("single oneshot"),
        Some(Command::Single(SingleMode::OneShot))
    );
}

#[test]
fn setvol_clamps_out_of_range_values() {
    assert_eq!(parse("setvol 150"), Some(Command::SetVol(100)));
    assert_eq!(parse("setvol -5"), Some(Command::SetVol(0)));
}

#[test]
fn malformed_arguments_degrade_to_bad_args() {
    // A recognized verb with a bad argument is BadArgs (→ ACK_ERROR_ARG),
    // distinct from an unrecognized verb (→ ACK_ERROR_UNKNOWN).
    assert_eq!(
        parse("setvol loud"),
        Some(Command::BadArgs("setvol".into()))
    );
    assert_eq!(
        parse("repeat maybe"),
        Some(Command::BadArgs("repeat".into()))
    );
    assert_eq!(parse("seek 1"), Some(Command::BadArgs("seek".into())));
    // An unrecognized verb stays Unknown.
    assert_eq!(parse("bogus x"), Some(Command::Unknown("bogus".into())));
}

#[test]
fn range_resolves_and_clamps_against_queue_length() {
    assert_eq!(Range::Position(2).resolve(5), Some((2, 3)));
    assert_eq!(
        Range::Bounded { start: 1, end: 99 }.resolve(5),
        Some((1, 5))
    );
    assert_eq!(Range::From { start: 2 }.resolve(5), Some((2, 5)));
    // Starting past the end, and an inverted range, are both errors.
    assert_eq!(Range::Position(9).resolve(5), None);
    assert_eq!(Range::Bounded { start: 4, end: 2 }.resolve(5), None);
    assert_eq!(Range::From { start: 5 }.resolve(5), None);
}

#[test]
fn response_encodes_key_value_lines() {
    let mut r = Response::new();
    r.push("volume", 80);
    r.push_opt("Artist", Some("Radiohead"));
    r.push_opt("Album", None::<String>);
    assert_eq!(r.encode(), "volume: 80\nArtist: Radiohead\n");
}

#[test]
fn response_flattens_newlines_so_a_tag_cannot_forge_a_line() {
    let mut r = Response::new();
    r.push("Title", "evil\nOK\nArtist: nope");
    // The injected newlines become spaces, so the client still sees
    // exactly one Title field.
    assert_eq!(r.encode(), "Title: evil OK Artist: nope\n");
}

#[test]
fn ack_encodes_the_mpd_error_shape() {
    let mut ack = Ack::new(ACK_ERROR_UNKNOWN, "bogus", "unknown command \"bogus\"");
    ack.list_index = 2;
    assert_eq!(
        ack.encode(),
        "ACK [5@2] {bogus} unknown command \"bogus\"\n"
    );
}
