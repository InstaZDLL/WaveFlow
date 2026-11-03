//! What a scan does to `track.rating`, through the real scan on the
//! migrated schema, with real files: the column has one thing no other
//! column the scan rewrites has, a value the user set that the file may
//! never hold.

use super::scan_folder_inner;
use crate::commands::edit::{patch_file, rehash_track_file, TagPatch};
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
        let artwork = self.dir.path().join("artwork");
        let summary = scan_folder_inner(&self.pool, &artwork, 1, None, false)
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
