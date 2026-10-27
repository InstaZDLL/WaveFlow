use super::*;

fn song() -> MpdSong {
    MpdSong {
        queue_id: 42,
        position: 3,
        file_path: "/music/a.flac".into(),
        title: "Creep".into(),
        duration_ms: 238_000,
        artist_name: Some("Radiohead".into()),
        album_title: Some("Pablo Honey".into()),
        album_artist_name: Some("Radiohead".into()),
        track_number: Some(2),
        disc_number: Some(1),
        year: Some(1993),
        file_modified: 0,
    }
}

#[test]
fn writes_the_mpd_song_fields() {
    let mut out = super::super::protocol::Response::new();
    song().write_into(&mut out);
    let encoded = out.encode();
    assert!(encoded.contains("file: /music/a.flac\n"));
    assert!(encoded.contains("Title: Creep\n"));
    assert!(encoded.contains("Artist: Radiohead\n"));
    assert!(encoded.contains("Pos: 3\n"));
    assert!(encoded.contains("Id: 42\n"));
}

#[test]
fn sends_both_the_legacy_and_float_duration() {
    let mut out = super::super::protocol::Response::new();
    song().write_into(&mut out);
    let encoded = out.encode();
    assert!(encoded.contains("Time: 238\n"));
    assert!(encoded.contains("duration: 238.000\n"));
}

#[test]
fn omits_absent_tags_rather_than_sending_them_blank() {
    let mut bare = song();
    bare.album_title = None;
    bare.track_number = None;
    let mut out = super::super::protocol::Response::new();
    bare.write_into(&mut out);
    let encoded = out.encode();
    assert!(!encoded.contains("Album:"), "{encoded}");
    assert!(!encoded.contains("Track:"), "{encoded}");
    // AlbumArtist is a distinct tag and must survive Album going away.
    assert!(encoded.contains("AlbumArtist: Radiohead\n"));
}

#[test]
fn last_modified_reads_the_column_as_milliseconds() {
    // `track.file_modified` is epoch millis, not seconds. Treating
    // it as seconds would put every file tens of thousands of years
    // out — this test is the guard on that unit.
    assert_eq!(format_last_modified(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        format_last_modified(1_700_000_000_000),
        "2023-11-14T22:13:20Z"
    );
}
