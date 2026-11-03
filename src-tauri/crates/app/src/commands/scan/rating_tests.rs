//! What a scan does to `track.rating`, through the real scan on the
//! migrated schema, with real files: the column has one thing no other
//! column the scan rewrites has, a value the user set that the file may
//! never hold.

use super::scan_folder_inner;
use crate::commands::edit::{patch_file, rehash_track_file, TagPatch, TrackEdit};
use crate::commands::scan::read_file_rating;
use crate::commands::track::write_rating_and_rehash;
use lofty::config::WriteOptions;
use lofty::ogg::tag::VorbisComments;
use lofty::tag::{Accessor, TagExt};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TWO_STARS: i64 = 102;
const FOUR_STARS: i64 = 204;

struct Library {
    dir: tempfile::TempDir,
    pool: SqlitePool,
}

impl Library {
    async fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("data.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir(&music).unwrap();
        sqlx::raw_sql(
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (1, 1, ?, 0, 0)",
        )
        .bind(music.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        Self { dir, pool }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join("music").join(name)
    }

    /// A real FLAC with no rating, under `name`.
    fn add_flac(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tags/silence.flac");
        std::fs::copy(fixture, &path).unwrap();
        path
    }

    /// `(added, updated, skipped)`, with nothing allowed to have failed.
    async fn scan(&self) -> (u32, u32, u32) {
        self.scan_with(false).await
    }

    async fn scan_deep(&self) -> (u32, u32, u32) {
        self.scan_with(true).await
    }

    async fn scan_with(&self, deep: bool) -> (u32, u32, u32) {
        let artwork = self.dir.path().join("artwork");
        let summary = scan_folder_inner(&self.pool, &artwork, 1, None, deep)
            .await
            .expect("scan");
        assert_eq!(summary.errors, 0, "every file here is readable");
        (summary.added, summary.updated, summary.skipped)
    }

    async fn id(&self, path: &Path) -> i64 {
        sqlx::query_scalar("SELECT id FROM track WHERE file_path = ?")
            .bind(path.to_string_lossy().to_string())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// The rating as a star in the app leaves it in the database,
    /// whatever became of the write to the file.
    async fn rate(&self, path: &Path, rating: Option<i64>) {
        sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
            .bind(rating)
            .bind(self.id(path).await)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn rating(&self, path: &Path) -> Option<i64> {
        sqlx::query_scalar("SELECT rating FROM track WHERE id = ?")
            .bind(self.id(path).await)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn title(&self, path: &Path) -> String {
        sqlx::query_scalar("SELECT title FROM track WHERE id = ?")
            .bind(self.id(path).await)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

/// What another tagger leaves behind: this title and these comments and
/// no others, with a new modification time, so the scan's fast path
/// cannot take the file for the one it knows.
fn retag_with_another_tool(path: &Path, title: &str, comments: &[(&str, &str)]) {
    let mut tag = VorbisComments::default();
    tag.set_title(title.to_string());
    for (key, value) in comments {
        tag.insert((*key).to_string(), (*value).to_string());
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("write comments");
    move_mtime_forward(path);
}

fn move_mtime_forward(path: &Path) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_modified(modified + Duration::from_secs(2))
        .unwrap();
}

/// A real DSF: the chunk chain, and a silence nothing decodes here.
fn dsf_bytes() -> Vec<u8> {
    let audio = vec![0x69u8; 8192];
    let audio_end = 28u64 + 52 + 12 + audio.len() as u64;
    let mut bytes = b"DSD ".to_vec();
    bytes.extend_from_slice(&28u64.to_le_bytes());
    bytes.extend_from_slice(&audio_end.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&52u64.to_le_bytes());
    for field in [1u32, 0, 2, 2, 2_822_400, 1] {
        bytes.extend_from_slice(&field.to_le_bytes());
    }
    bytes.extend_from_slice(&32_768u64.to_le_bytes());
    bytes.extend_from_slice(&4096u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(12 + audio.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&audio);
    bytes
}

/// A real DFF: `FRM8` around the version, the properties and the audio.
fn dff_bytes() -> Vec<u8> {
    let mut prop = b"SND ".to_vec();
    prop.extend_from_slice(b"FS  ");
    prop.extend_from_slice(&4u64.to_be_bytes());
    prop.extend_from_slice(&2_822_400u32.to_be_bytes());
    prop.extend_from_slice(b"CHNL");
    prop.extend_from_slice(&10u64.to_be_bytes());
    prop.extend_from_slice(&2u16.to_be_bytes());
    prop.extend_from_slice(b"SLFTSRGT");

    let mut chunks = b"FVER".to_vec();
    chunks.extend_from_slice(&4u64.to_be_bytes());
    chunks.extend_from_slice(&0x0105_0000u32.to_be_bytes());
    chunks.extend_from_slice(b"PROP");
    chunks.extend_from_slice(&(prop.len() as u64).to_be_bytes());
    chunks.extend_from_slice(&prop);
    chunks.extend_from_slice(b"DSD ");
    chunks.extend_from_slice(&8192u64.to_be_bytes());
    chunks.extend_from_slice(&[0x69u8; 8192]);

    let mut bytes = b"FRM8".to_vec();
    bytes.extend_from_slice(&(4 + chunks.len() as u64).to_be_bytes());
    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&chunks);
    bytes
}

/// The case the column has to survive by itself: a `.dff` has no tag block
/// to write to, so the database holds the only copy, and the next change to
/// the file used to empty it.
#[tokio::test]
async fn a_rating_only_the_database_holds_survives_a_changed_file() {
    let library = Library::new().await;
    let path = library.path("db-only.dff");
    std::fs::write(&path, dff_bytes()).unwrap();
    assert_eq!(library.scan().await, (1, 0, 0));

    assert!(
        patch_file(&path, &TagPatch::Rating(Some(FOUR_STARS as u8))).is_err(),
        "the premise: this container refuses the write"
    );
    library.rate(&path, Some(FOUR_STARS)).await;

    // A chunk at the end, with the FRM8 size following it.
    let mut bytes = dff_bytes();
    bytes.extend_from_slice(b"COMT");
    bytes.extend_from_slice(&2u64.to_be_bytes());
    bytes.extend_from_slice(&[0, 0]);
    let form_size = bytes.len() as u64 - 12;
    bytes[4..12].copy_from_slice(&form_size.to_be_bytes());
    std::fs::write(&path, bytes).unwrap();
    move_mtime_forward(&path);

    assert_eq!(library.scan().await, (0, 1, 0), "the rewriting branch");
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));
}

/// No other tool at all. The rating reaches a `.dsf`, whose rehash stores a
/// whole-file digest where the scan keeps its own, so the next scan takes
/// the rewriting branch; and the DSD reader does not read a rating back.
/// One star in the app and one scan used to be enough to lose it.
#[tokio::test]
async fn a_dsf_rated_in_the_app_keeps_it_through_the_next_scan() {
    let library = Library::new().await;
    let path = library.path("rated.dsf");
    std::fs::write(&path, dsf_bytes()).unwrap();
    assert_eq!(library.scan().await, (1, 0, 0));

    patch_file(&path, &TagPatch::Rating(Some(FOUR_STARS as u8))).expect("the DSF writer");
    let written = std::fs::read(&path).unwrap();
    assert!(
        written.windows(4).any(|window| window == b"POPM"),
        "the premise: the rating is in the file"
    );
    let id = library.id(&path).await;
    rehash_track_file(&library.pool, id, &path).await.unwrap();
    library.rate(&path, Some(FOUR_STARS)).await;
    move_mtime_forward(&path);

    assert_eq!(library.scan().await, (0, 1, 0), "the rewriting branch");
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));
}

/// The other direction, which has to keep working: a file that carries a
/// rating has its say, at the first import and over one already stored.
#[tokio::test]
async fn a_rating_another_tool_writes_is_taken() {
    let library = Library::new().await;
    let path = library.add_flac("rated-elsewhere.flac");
    retag_with_another_tool(&path, "first", &[("RATING", "40")]);
    assert_eq!(library.scan().await, (1, 0, 0));
    assert_eq!(library.rating(&path).await, Some(TWO_STARS), "the import");

    retag_with_another_tool(&path, "second", &[("RATING", "80")]);
    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS), "the rescan");
    assert_eq!(library.title(&path).await, "second");
}

/// The decision, stated as a test: a file without a rating says nothing
/// about the rating. It cannot tell "removed on purpose" from "never written
/// here" or from a tagger that keeps only the tags it knows, and the rating
/// is the user's, so the library keeps it. Clearing it is done in the app.
#[tokio::test]
async fn a_rating_another_tool_removes_stays_in_the_library() {
    let library = Library::new().await;
    let path = library.add_flac("stripped.flac");
    retag_with_another_tool(&path, "rated", &[("RATING", "80")]);
    library.scan().await;
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));

    retag_with_another_tool(&path, "stripped", &[]);
    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));
    assert_eq!(
        library.title(&path).await,
        "stripped",
        "the rest is re-read"
    );
}

/// A zero is the same silence, spelled out: `0` is what ID3 itself calls
/// unknown, and nothing a star widget can set.
#[tokio::test]
async fn a_zero_in_the_file_is_no_rating() {
    let library = Library::new().await;
    let path = library.add_flac("zero.flac");
    retag_with_another_tool(&path, "zero", &[("RATING", "0")]);
    library.scan().await;
    assert_eq!(library.rating(&path).await, None, "the import");

    library.rate(&path, Some(FOUR_STARS)).await;
    retag_with_another_tool(&path, "still zero", &[("RATING", "0")]);
    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS), "the rescan");
}

/// Keeping what the file does not mention must not bring back what the user
/// took away: a rating cleared in the app is gone from both.
#[tokio::test]
async fn a_rating_cleared_in_the_app_stays_cleared() {
    let library = Library::new().await;
    let path = library.add_flac("cleared.flac");
    library.scan().await;
    library.rate(&path, Some(FOUR_STARS)).await;
    library.rate(&path, None).await;

    retag_with_another_tool(&path, "retagged", &[]);
    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, None);
}

/// The twin the same line served: replacing a file keeps the track, its
/// plays and its likes, and it lost the rating when the better copy carried
/// none. This is the state `replace_track_file` leaves for its rescan.
#[tokio::test]
async fn a_replaced_file_keeps_the_rating_of_the_track() {
    let library = Library::new().await;
    let old = library.add_flac("old.flac");
    library.scan().await;
    let id = library.id(&old).await;
    library.rate(&old, Some(FOUR_STARS)).await;

    std::fs::remove_file(&old).unwrap();
    let target = library.add_flac("target.flac");
    sqlx::query("UPDATE track SET file_path = ?, file_modified = 0 WHERE id = ?")
        .bind(target.to_string_lossy().to_string())
        .bind(id)
        .execute(&library.pool)
        .await
        .unwrap();

    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.id(&target).await, id);
    assert_eq!(library.rating(&target).await, Some(FOUR_STARS));
}

/// Everything the star widget can set, halves included.
const EVERY_STAR: [u8; 10] = [26, 51, 77, 102, 128, 153, 179, 204, 230, 255];

/// One real file of each container a rating has a route into.
const CONTAINERS: [&str; 6] = ["mp3", "flac", "m4a", "ogg", "opus", "dsf"];

fn add_file(library: &Library, name: &str) -> PathBuf {
    let path = library.path(name);
    let ext = path.extension().unwrap().to_str().unwrap();
    if ext == "dsf" {
        std::fs::write(&path, dsf_bytes()).unwrap();
    } else {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tags")
            .join(format!("silence.{ext}"));
        std::fs::copy(fixture, &path).unwrap();
    }
    path
}

fn holds(path: &Path, needle: &[u8]) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// What `set_track_rating` does, minus the Tauri state around it.
async fn rate_in_the_app(library: &Library, path: &Path, rating: Option<u8>) {
    let id = library.id(path).await;
    assert!(
        write_rating_and_rehash(&library.pool, id, path, rating)
            .await
            .unwrap(),
        "{} did not take the rating",
        path.display()
    );
    library.rate(path, rating.map(i64::from)).await;
    move_mtime_forward(path);
}

fn save_title(path: &Path, title: &str) {
    let edit = TrackEdit {
        title: Some(title.to_string()),
        ..Default::default()
    };
    patch_file(path, &TagPatch::Fields(&edit)).unwrap();
    move_mtime_forward(path);
}

/// The contract, in every container, on every star: written, in the file,
/// and the same value after the scan that reads it back. A rating used to
/// stop at the database on four of these, and to come back a step higher
/// on the MP3.
#[tokio::test]
async fn every_star_survives_the_file_and_the_next_scan() {
    let library = Library::new().await;
    let mut files = Vec::new();
    for ext in CONTAINERS {
        for star in EVERY_STAR {
            files.push((add_file(&library, &format!("{star}.{ext}")), star));
        }
    }
    let (added, _, _) = library.scan().await;
    assert_eq!(added as usize, files.len());

    for (path, star) in &files {
        rate_in_the_app(&library, path, Some(*star)).await;
        assert_eq!(
            read_file_rating(path).unwrap(),
            Some(*star),
            "{} as the writer left it",
            path.display()
        );
        // Emptied here, so what the assertion below finds came from the file.
        library.rate(path, None).await;
    }
    let (_, updated, _) = library.scan().await;
    assert_eq!(updated as usize, files.len(), "the rewriting branch");
    for (path, star) in &files {
        assert_eq!(
            library.rating(path).await,
            Some(i64::from(*star)),
            "{} after the scan",
            path.display()
        );
    }
}

/// What other software sees, without going through the reader above: the
/// key and the scale, as bytes in the file.
#[tokio::test]
async fn the_file_holds_the_format_other_software_reads() {
    let library = Library::new().await;
    let flac = add_file(&library, "a.flac");
    let ogg = add_file(&library, "a.ogg");
    let opus = add_file(&library, "a.opus");
    let m4a = add_file(&library, "a.m4a");
    let mp3 = add_file(&library, "a.mp3");
    library.scan().await;
    for path in [&flac, &ogg, &opus, &m4a, &mp3] {
        rate_in_the_app(&library, path, Some(FOUR_STARS as u8)).await;
    }

    for path in [&flac, &ogg, &opus] {
        assert!(holds(path, b"RATING=80"), "{}", path.display());
    }
    assert!(holds(&m4a, b"com.apple.iTunes"));
    assert!(holds(&m4a, b"RATING"));
    assert!(holds(&mp3, b"POPM"));
}

/// A title saved over a rated file. The tag round trip used to drop the
/// comment and the atom, and to rewrite the frame as five stars under
/// another program's email.
#[tokio::test]
async fn a_saved_title_leaves_the_rating_in_the_file() {
    let library = Library::new().await;
    for ext in CONTAINERS {
        for star in [102u8, 230] {
            let path = add_file(&library, &format!("{star}.{ext}"));
            library.scan().await;
            rate_in_the_app(&library, &path, Some(star)).await;
            save_title(&path, "a new title");
            assert_eq!(
                read_file_rating(&path).unwrap(),
                Some(star),
                "{}",
                path.display()
            );
            if ext == "mp3" {
                assert_eq!(popm_frames(&path), vec![(String::new(), star)]);
            }
        }
    }
}

fn popm_frames(path: &Path) -> Vec<(String, u8)> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    let mut file = std::fs::File::open(path).unwrap();
    let mpeg = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()).unwrap();
    mpeg.id3v2()
        .map(|tag| {
            tag.into_iter()
                .filter_map(|frame| match frame {
                    lofty::id3::v2::Frame::Popularimeter(popm) => {
                        Some((popm.email.to_string(), popm.rating))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Another program's frame, written by hand so no writer of ours is involved.
fn write_popm(path: &Path, email: &str, byte: u8) {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::id3::v2::{Frame, PopularimeterFrame};
    use lofty::tag::TagExt;
    let mut file = std::fs::File::open(path).unwrap();
    let mpeg = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()).unwrap();
    drop(file);
    let mut tag = mpeg.id3v2().cloned().unwrap_or_default();
    tag.insert(Frame::Popularimeter(PopularimeterFrame::new(
        email.to_string(),
        byte,
        7,
    )));
    tag.save_to_path(path, WriteOptions::default()).unwrap();
    move_mtime_forward(path);
}

/// A zero from another program, often just the frame around its play count,
/// is no rating: it used to arrive as one star.
#[tokio::test]
async fn a_popm_zero_from_another_program_is_no_rating() {
    let library = Library::new().await;
    let path = add_file(&library, "zero.mp3");
    write_popm(&path, "someone@example.com", 0);
    assert_eq!(library.scan().await, (1, 0, 0));
    assert_eq!(library.rating(&path).await, None, "the import");

    library.rate(&path, Some(FOUR_STARS)).await;
    save_title(&path, "changed");
    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS), "the rescan");
}

/// Another program's frame keeps its own scale, on which 1 is one star.
#[tokio::test]
async fn a_popm_from_another_program_is_read_on_its_scale() {
    let library = Library::new().await;
    let mut files = Vec::new();
    for (byte, stars) in [(1u8, 1i64), (64, 2), (128, 3), (196, 4), (255, 5)] {
        let path = add_file(&library, &format!("{byte}.mp3"));
        write_popm(&path, "someone@example.com", byte);
        files.push((path, stars));
    }
    library.scan().await;
    for (path, stars) in &files {
        assert_eq!(library.rating(path).await, Some(stars * 51), "{stars}");
    }
}

/// A `.dsf` from elsewhere brings its rating, by the same rule.
#[tokio::test]
async fn a_rating_in_a_dsf_is_read_at_the_import() {
    let library = Library::new().await;
    let path = add_file(&library, "elsewhere.dsf");
    patch_file(&path, &TagPatch::Rating(Some(230))).unwrap();
    library.scan().await;
    assert_eq!(library.rating(&path).await, Some(230));
}

fn write_ilst(path: &Path, atoms: &[(lofty::mp4::AtomIdent<'static>, &str)]) {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;
    use lofty::mp4::{Atom, AtomData};
    use lofty::tag::TagExt;
    let mut file = std::fs::File::open(path).unwrap();
    let mp4 = lofty::mp4::Mp4File::read_from(&mut file, ParseOptions::new()).unwrap();
    drop(file);
    let mut ilst = mp4.ilst().cloned().unwrap_or_default();
    for (ident, value) in atoms {
        ilst.insert(Atom::new(
            ident.clone(),
            AtomData::UTF8((*value).to_string()),
        ));
    }
    ilst.save_to_path(path, WriteOptions::default()).unwrap();
    move_mtime_forward(path);
}

fn rate_atom() -> lofty::mp4::AtomIdent<'static> {
    lofty::mp4::AtomIdent::Fourcc(*b"rate")
}

fn freeform_atom() -> lofty::mp4::AtomIdent<'static> {
    lofty::mp4::AtomIdent::Freeform {
        mean: "com.apple.iTunes".into(),
        name: "RATING".into(),
    }
}

/// `rate` is read, half stars included, and the app's own atom wins beside it.
#[tokio::test]
async fn an_m4a_rating_is_read_from_either_atom() {
    let library = Library::new().await;
    let rate = add_file(&library, "rate.m4a");
    write_ilst(&rate, &[(rate_atom(), "90")]);
    let both = add_file(&library, "both.m4a");
    write_ilst(&both, &[(rate_atom(), "20"), (freeform_atom(), "80")]);
    library.scan().await;
    assert_eq!(library.rating(&rate).await, Some(230));
    assert_eq!(library.rating(&both).await, Some(FOUR_STARS));
}

/// Clearing has to take both atoms: the rating would otherwise come back
/// from the one left behind at the next scan.
#[tokio::test]
async fn clearing_an_m4a_takes_both_atoms() {
    let library = Library::new().await;
    let path = add_file(&library, "both.m4a");
    write_ilst(&path, &[(rate_atom(), "20"), (freeform_atom(), "80")]);
    library.scan().await;
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));

    rate_in_the_app(&library, &path, None).await;
    assert_eq!(read_file_rating(&path).unwrap(), None);
    assert!(!holds(&path, b"rate"), "the older atom went too");
    assert!(!holds(&path, b"RATING"));

    assert_eq!(library.scan().await, (0, 1, 0));
    assert_eq!(library.rating(&path).await, None);
}

/// A clear in the other containers, every form of the field included.
#[tokio::test]
async fn clearing_takes_every_form_of_the_field() {
    let library = Library::new().await;
    let flac = add_file(&library, "a.flac");
    retag_with_another_tool(
        &flac,
        "rated twice",
        &[("RATING", "80"), ("RATING:someone@example.com", "40")],
    );
    let mp3 = add_file(&library, "a.mp3");
    write_popm(&mp3, "someone@example.com", 196);
    library.scan().await;

    for path in [&flac, &mp3] {
        rate_in_the_app(&library, path, None).await;
        assert_eq!(read_file_rating(path).unwrap(), None, "{}", path.display());
    }
    library.scan().await;
    assert_eq!(library.rating(&flac).await, None);
    assert_eq!(library.rating(&mp3).await, None);
}

/// A plain number may be stars: some players write 1 to 5.
#[tokio::test]
async fn a_rating_of_one_to_five_is_stars() {
    let library = Library::new().await;
    let path = library.add_flac("stars.flac");
    retag_with_another_tool(&path, "stars", &[("RATING", "4")]);
    library.scan().await;
    assert_eq!(library.rating(&path).await, Some(FOUR_STARS));
}

/// The rating is no custom column: the library has its own for it, and a
/// second copy here would go stale with the first star changed.
#[tokio::test]
async fn the_rating_atom_is_not_offered_as_a_custom_tag() {
    let library = Library::new().await;
    let path = add_file(&library, "a.m4a");
    library.scan().await;
    rate_in_the_app(&library, &path, Some(FOUR_STARS as u8)).await;
    library.scan().await;
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_tag WHERE key = 'RATING' COLLATE NOCASE")
            .fetch_one(&library.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
}

/// A file that cannot take the rating is reported, not passed over, and the
/// library has it all the same.
#[tokio::test]
async fn a_write_that_fails_is_reported() {
    let library = Library::new().await;
    let path = library.path("no-tags.dff");
    std::fs::write(&path, dff_bytes()).unwrap();
    library.scan().await;
    let id = library.id(&path).await;
    let written = write_rating_and_rehash(&library.pool, id, &path, Some(204))
        .await
        .unwrap();
    assert!(!written);
}

/// A rating an older build moved up a step: it stored lofty's whole star
/// while the file kept the byte. Only a deep rescan, which the user asks for,
/// goes back to the file; a normal one has no reason to open it.
#[tokio::test]
async fn a_deep_rescan_brings_back_the_byte_in_the_file() {
    let library = Library::new().await;
    let path = add_file(&library, "moved-up.mp3");
    let db_only = library.add_flac("db-only.flac");
    library.scan().await;
    rate_in_the_app(&library, &path, Some(TWO_STARS as u8)).await;
    library.scan().await;
    assert_eq!(library.rating(&path).await, Some(TWO_STARS));

    // What the older scan left: the bytes as they were, a star more here.
    library.rate(&path, Some(153)).await;
    library.rate(&db_only, Some(FOUR_STARS)).await;

    assert_eq!(library.scan().await, (0, 0, 2), "the fast path");
    assert_eq!(library.rating(&path).await, Some(153));

    assert_eq!(library.scan_deep().await, (0, 0, 2), "unchanged files");
    assert_eq!(library.rating(&path).await, Some(TWO_STARS), "the byte");
    assert_eq!(
        library.rating(&db_only).await,
        Some(FOUR_STARS),
        "a file with no rating says nothing, on this branch too"
    );
}

/// The command takes any byte. One under half a star stays a small rating in
/// the text containers, rather than none at all, or five stars.
#[tokio::test]
async fn a_byte_under_half_a_star_stays_a_small_rating() {
    let library = Library::new().await;
    for byte in [1u8, 3, 13, 15] {
        let path = add_file(&library, &format!("{byte}.flac"));
        library.scan().await;
        rate_in_the_app(&library, &path, Some(byte)).await;
        assert_eq!(read_file_rating(&path).unwrap(), Some(15), "{byte}");
    }
}
