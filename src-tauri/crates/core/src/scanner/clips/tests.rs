use super::*;
use std::io::Cursor;

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// A track of `handler` whose first sample entry is `codec`.
fn trak(handler: &[u8; 4], codec: &[u8; 4]) -> Vec<u8> {
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(handler);
    hdlr.extend_from_slice(&[0u8; 12]);
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(boxed(codec, &[0u8; 16]));
    let stbl = boxed(b"stbl", &boxed(b"stsd", &stsd));
    let minf = boxed(b"minf", &stbl);
    let mut mdia = boxed(b"mdhd", &[0u8; 24]);
    mdia.extend(boxed(b"hdlr", &hdlr));
    mdia.extend(minf);
    boxed(b"trak", &boxed(b"mdia", &mdia))
}

/// An ISO-BMFF file holding these tracks, its index after the media the
/// way a file written without "fast start" has it.
fn mp4(tracks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = boxed(b"ftyp", b"isom\0\0\0\0isommp42");
    out.extend(boxed(b"mdat", &[0u8; 4096]));
    let mut moov = boxed(b"mvhd", &[0u8; 100]);
    for track in tracks {
        moov.extend_from_slice(track);
    }
    out.extend(boxed(b"moov", &moov));
    out
}

fn video_mp4() -> Vec<u8> {
    mp4(&[trak(b"soun", b"mp4a"), trak(b"vide", b"avc1")])
}

fn audio_mp4() -> Vec<u8> {
    mp4(&[trak(b"soun", b"mp4a")])
}

fn has_video(bytes: &[u8]) -> bool {
    has_video_track(&mut Cursor::new(bytes)).unwrap()
}

#[test]
fn a_music_video_has_a_video_track() {
    assert!(has_video(&video_mp4()));
    assert!(has_video(&mp4(&[trak(b"vide", b"hvc1")])));
}

#[test]
fn an_audio_mp4_has_none() {
    assert!(!has_video(&audio_mp4()));
}

/// An audiobook's chapter pictures sit in a `vide` track of still images.
#[test]
fn chapter_images_are_not_a_video() {
    assert!(!has_video(&mp4(&[
        trak(b"soun", b"mp4a"),
        trak(b"vide", b"jpeg")
    ])));
}

#[test]
fn a_truncated_or_hostile_file_is_no_video() {
    let whole = video_mp4();
    assert!(!has_video(&whole[..whole.len() / 2]));
    assert!(!has_video(&[]));
    // A box claiming to be larger than the file.
    let mut lying = boxed(b"moov", &[0u8; 8]);
    lying[3] = 0xff;
    assert!(!has_video(&lying));
    // Thousands of empty tracks end at the budget, not the stack.
    let many: Vec<Vec<u8>> = (0..MAX_BOXES + 10).map(|_| boxed(b"trak", &[])).collect();
    let mut with_video = many;
    with_video.push(trak(b"vide", b"avc1"));
    assert!(!has_video(&mp4(&with_video)));
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn candidates(names: &[&str]) -> Vec<String> {
    let files = paths(names);
    clip_candidates(&files)
        .into_iter()
        .map(|i| names[i].to_string())
        .collect()
}

#[test]
fn an_mp4_beside_its_song_is_a_candidate() {
    assert_eq!(
        candidates(&["/m/A/Song.flac", "/m/A/Song.mp4", "/m/A/Other.mp4"]),
        ["/m/A/Song.mp4"]
    );
}

#[test]
fn the_name_is_compared_without_case() {
    assert_eq!(
        candidates(&["/m/A/01 Song.FLAC", "/m/A/01 song.MP4"]),
        ["/m/A/01 song.MP4"]
    );
}

#[test]
fn an_mp4_in_a_clips_folder_matches_the_song_above_it() {
    assert_eq!(
        candidates(&["/m/A/Song.flac", "/m/A/clips/Song.mp4"]),
        ["/m/A/clips/Song.mp4"]
    );
    // A `Clips` folder of another album, or a song elsewhere, is not it.
    assert!(candidates(&["/m/B/Song.flac", "/m/A/Clips/Song.mp4"]).is_empty());
}

/// A lone video, and two `.mp4` files of one name, stay tracks.
#[test]
fn an_mp4_with_no_song_beside_it_is_not_a_candidate() {
    assert!(candidates(&["/m/A/Song.mp4"]).is_empty());
    assert!(candidates(&["/m/A/Song.mp4", "/m/A/Song.m4a.mp4"]).is_empty());
    // Another directory with the same name is not a neighbour.
    assert!(candidates(&["/m/A/Song.flac", "/m/B/Song.mp4"]).is_empty());
}

/// Share locators split the same way as local paths.
#[test]
fn share_locators_are_judged_the_same_way() {
    assert_eq!(
        candidates(&[
            "wfnet://0b6a/Music/A/Song.flac",
            "wfnet://0b6a/Music/A/Song.mp4",
        ]),
        ["wfnet://0b6a/Music/A/Song.mp4"]
    );
}

/// The three cases the walk must get right, on real files: a clip leaves
/// the list, an audio `.mp4` beside a song of its name stays, and a lone
/// music video stays a track.
#[test]
fn drop_clips_keeps_audio_and_lone_videos() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut files = vec![
        write(dir, "A/Song.flac", b"fLaC"),
        write(dir, "A/Song.mp4", &video_mp4()),
        write(dir, "B/Track.flac", b"fLaC"),
        write(dir, "B/Track.mp4", &audio_mp4()),
        write(dir, "C/Lone.mp4", &video_mp4()),
        write(dir, "D/Tune.flac", b"fLaC"),
        write(dir, "D/Clips/Tune.mp4", &video_mp4()),
    ];
    let expected: Vec<PathBuf> = [0, 2, 3, 4, 5].iter().map(|&i| files[i].clone()).collect();
    assert_eq!(drop_clips(&mut files, file_has_video_track), 2);
    assert_eq!(files, expected);
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn find_clip_prefers_the_sibling_then_the_clips_folder_then_the_file_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let song = write(dir, "Song.flac", b"fLaC");
    assert_eq!(find_clip(&song), None);

    let in_clips = write(dir, "CLIPS/song.webm", &EBML_MAGIC);
    assert_eq!(find_clip(&song), Some((in_clips, ClipOrigin::ClipsDir)));

    let beside = write(dir, "Song.mp4", &video_mp4());
    assert_eq!(
        find_clip(&song),
        Some((beside.clone(), ClipOrigin::Sibling))
    );

    // The video `.mp4`, scanned as a track when it is alone, is its own clip.
    let lone = write(dir, "Lone.mp4", &video_mp4());
    assert_eq!(find_clip(&lone), Some((lone.clone(), ClipOrigin::Itself)));
}

#[test]
fn an_audio_mp4_beside_a_song_is_not_its_clip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let song = write(dir, "Song.flac", b"fLaC");
    let audio = write(dir, "Song.mp4", &audio_mp4());
    assert_eq!(find_clip(&song), None);
    // Nor its own.
    assert_eq!(find_clip(&audio), None);
}

#[test]
fn a_track_in_a_clips_folder_is_not_offered_its_neighbours() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let kept = write(dir, "Clips/Song.mp4", &audio_mp4());
    write(dir, "Clips/Song.webm", &EBML_MAGIC);
    assert_eq!(find_clip(&kept), None);
}

/// A file named like a video is checked before it reaches the webview's
/// asset scope.
#[test]
fn a_webm_that_is_not_one_is_not_a_clip() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let song = write(dir, "Song.flac", b"fLaC");
    write(dir, "Song.webm", b"not a video");
    assert_eq!(find_clip(&song), None);
}

#[cfg(unix)]
#[test]
fn a_link_named_like_a_clip_is_not_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let song = write(dir, "Song.flac", b"fLaC");
    let target = write(dir, "elsewhere/real.webm", &EBML_MAGIC);
    std::os::unix::fs::symlink(&target, dir.join("Song.webm")).unwrap();
    assert_eq!(find_clip(&song), None);
}
