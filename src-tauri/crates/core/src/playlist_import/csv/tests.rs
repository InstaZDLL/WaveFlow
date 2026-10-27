use super::*;

#[test]
fn a_comma_export_with_quoted_fields_is_read_in_order() {
    let text =
        "\u{feff}\"Track Name\",\"Artist Name(s)\",\"Album Name\",\"Duration (ms)\",\"ISRC\"\r\n\
                \"Hey, Jude\",\"The Beatles\",\"Past Masters\",\"431333\",\"GBAYE0601690\"\r\n\
                \"Say \"\"Hello\"\"\",\"A, B\",\"\",\"\",\"\"\r\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.entries.len(), 2);
    let first = &parsed.entries[0];
    assert_eq!(first.title, "Hey, Jude");
    assert_eq!(first.artist.as_deref(), Some("The Beatles"));
    assert_eq!(first.album.as_deref(), Some("Past Masters"));
    assert_eq!(first.duration_ms, Some(431_333));
    assert_eq!(first.isrc.as_deref(), Some("GBAYE0601690"));
    let second = &parsed.entries[1];
    assert_eq!(second.title, "Say \"Hello\"");
    assert_eq!(second.artist.as_deref(), Some("A, B"));
    assert_eq!(second.album, None);
    assert_eq!(second.duration_ms, None);
}

#[test]
fn a_semicolon_export_with_columns_in_another_order() {
    let text = "artist;duration;title\nDaft Punk;5:37;Around the World\n\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].title, "Around the World");
    assert_eq!(parsed.entries[0].artist.as_deref(), Some("Daft Punk"));
    assert_eq!(parsed.entries[0].duration_ms, Some(337_000));
}

#[test]
fn a_tab_export_and_a_line_break_inside_a_quoted_field() {
    let text = "Title\tArtist\n\"Two\nlines\"\tSomeone\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.entries[0].title, "Two\nlines");
    assert_eq!(parsed.entries[0].artist.as_deref(), Some("Someone"));
}

#[test]
fn rows_without_a_title_are_skipped() {
    let text = "title,artist\n,Nobody\nReal,Somebody\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].title, "Real");
}

#[test]
fn a_header_without_a_title_column_is_refused() {
    assert_eq!(parse("artist,album\nA,B\n"), Err(ParseError::NoTitleColumn));
    assert_eq!(parse(""), Err(ParseError::Empty));
    assert_eq!(parse("title\n\n"), Err(ParseError::Empty));
}

#[test]
fn a_short_row_reads_its_missing_fields_as_empty() {
    let parsed = parse("title,artist,album\nOnly a title\n").unwrap();
    assert_eq!(parsed.entries[0].title, "Only a title");
    assert_eq!(parsed.entries[0].artist, None);
}
