//! What a scan must not do to a library: hide tracks over a folder it could
//! not read, read one artist out of several, fold unrelated albums into one,
//! or start a moved file over from nothing. Through the real scan, on the
//! migrated schema with foreign keys on, with real files.

use super::rating_tests::dsf_bytes;
use super::{scan_folder_inner, ScanSummary};
use crate::commands::edit::{patch_file, rehash_track_file, TagPatch, TrackEdit};
use lofty::config::WriteOptions;
use lofty::id3::v2::{ExtendedTextFrame, Frame, FrameId, Id3v2Tag, TextInformationFrame};
use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
use lofty::ogg::tag::VorbisComments;
use lofty::tag::{Accessor, TagExt};
use lofty::TextEncoding;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Library {
    dir: tempfile::TempDir,
    pool: SqlitePool,
}

impl Library {
    /// One library holding one folder, `music`, with the id 1.
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
        sqlx::raw_sql(
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0),
                    (2, 'Other', 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let library = Self { dir, pool };
        library.add_folder(1, 1, "music").await;
        library
    }

    async fn add_folder(&self, id: i64, library_id: i64, name: &str) {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        sqlx::query(
            "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (?, ?, ?, 0, 0)",
        )
        .bind(id)
        .bind(library_id)
        .bind(root.to_string_lossy().to_string())
        .execute(&self.pool)
        .await
        .unwrap();
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("music")
    }

    /// Under the folder's root, its parent directories made.
    fn path(&self, relative: &str) -> PathBuf {
        let path = self.root().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    /// A real FLAC at `path` carrying these comments and no others.
    fn flac_at(&self, path: &Path, comments: &[(&str, &str)]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::copy(fixture("silence.flac"), path).unwrap();
        let mut tag = VorbisComments::default();
        for (key, value) in comments {
            tag.push((*key).to_string(), (*value).to_string());
        }
        tag.save_to_path(path, WriteOptions::default())
            .expect("write comments");
    }

    fn flac(&self, relative: &str, comments: &[(&str, &str)]) -> PathBuf {
        let path = self.path(relative);
        self.flac_at(&path, comments);
        path
    }

    async fn try_scan(&self, folder_id: i64, deep: bool) -> crate::error::AppResult<ScanSummary> {
        let artwork = self.dir.path().join("artwork");
        scan_folder_inner(&self.pool, &artwork, folder_id, None, deep).await
    }

    async fn scan(&self) -> ScanSummary {
        self.scan_folder(1).await
    }

    async fn scan_folder(&self, folder_id: i64) -> ScanSummary {
        let summary = self.try_scan(folder_id, false).await.expect("scan");
        assert_eq!(summary.errors, 0, "every file here is readable");
        summary
    }

    async fn id(&self, path: &Path) -> i64 {
        sqlx::query_scalar("SELECT id FROM track WHERE file_path = ?")
            .bind(path.to_string_lossy().to_string())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn available(&self, id: i64) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT is_available FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
            == 1
    }

    async fn track_path(&self, id: i64) -> String {
        sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn track_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM track")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// The credits of a track, in order, and its primary artist.
    async fn credits(&self, path: &Path) -> (Vec<String>, Option<String>) {
        let id = self.id(path).await;
        let names = sqlx::query_scalar(
            "SELECT a.name FROM track_artist ta JOIN artist a ON a.id = ta.artist_id
              WHERE ta.track_id = ? ORDER BY ta.position",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        let primary = sqlx::query_scalar(
            "SELECT a.name FROM track t JOIN artist a ON a.id = t.primary_artist WHERE t.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .unwrap();
        (names, primary)
    }

    async fn names(&self, path: &Path) -> Vec<String> {
        self.credits(path).await.0
    }

    /// `(artist name, is_compilation, track count)` of every album of a
    /// title, by artist name.
    async fn albums(&self, title: &str) -> Vec<(String, bool, i64)> {
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT ar.name, al.is_compilation,
                    (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id)
               FROM album al JOIN artist ar ON ar.id = al.artist_id
              WHERE al.title = ?
              ORDER BY ar.name",
        )
        .bind(title)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(name, compilation, tracks)| (name, compilation == 1, tracks))
        .collect()
    }

    async fn setting(&self, key: &str) -> Option<String> {
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
    }

    async fn clear_setting(&self, key: &str) {
        sqlx::query("DELETE FROM profile_setting WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// What a build that read one artist left behind.
    async fn credit_only_first(&self, path: &Path) {
        sqlx::query("DELETE FROM track_artist WHERE track_id = ? AND position > 0")
            .bind(self.id(path).await)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    /// Everything of the user's that hangs off a track: a rating, a like,
    /// a playlist place and a play.
    async fn make_it_loved(&self, id: i64) {
        sqlx::query("UPDATE track SET rating = 204 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO liked_track (track_id, liked_at) VALUES (?, 1)")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO playlist (id, name, created_at, updated_at)
             VALUES (1, 'P', 0, 0)",
        )
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (1, ?, 0, 0)",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO play_event (track_id, played_at, listened_ms, completed, skipped)
             VALUES (?, 1, 1000, 1, 0)",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// `(rating, likes, playlist places, plays)` of a track.
    async fn love(&self, id: i64) -> (Option<i64>, i64, i64, i64) {
        let rating = sqlx::query_scalar("SELECT rating FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let mut counts = Vec::new();
        for table in ["liked_track", "playlist_track", "play_event"] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE track_id = ?");
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .unwrap();
            counts.push(count);
        }
        (rating, counts[0], counts[1], counts[2])
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tags")
        .join(name)
}

fn move_mtime_forward(path: &Path) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_modified(modified + Duration::from_secs(2))
        .unwrap();
}

/// A real MP3 whose ID3v2.4 tag holds these frames.
fn mp3(path: &Path, frames: Vec<Frame<'static>>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::copy(fixture("silence.mp3"), path).unwrap();
    let mut tag = Id3v2Tag::default();
    tag.set_title("t".to_string());
    for frame in frames {
        tag.insert(frame);
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("write frames");
}

fn tpe1(value: &str) -> Frame<'static> {
    Frame::Text(TextInformationFrame::new(
        FrameId::Valid(Cow::Borrowed("TPE1")),
        TextEncoding::UTF8,
        value.to_string(),
    ))
}

fn txxx_artists(value: &str) -> Frame<'static> {
    Frame::UserText(ExtendedTextFrame::new(
        TextEncoding::UTF8,
        "ARTISTS".to_string(),
        value.to_string(),
    ))
}

// ─── A folder the scan could not read ───────────────────────────────────

/// A drive that is not plugged in: the walk's one error used to read as an
/// empty folder, and the sweep then hid every track of it.
#[tokio::test]
async fn a_missing_root_hides_nothing_and_says_so() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    let b = library.flac("sub/b.flac", &[("TITLE", "b")]);
    library.scan().await;
    let (a_id, b_id) = (library.id(&a).await, library.id(&b).await);

    std::fs::remove_dir_all(library.root()).unwrap();
    let err = library
        .try_scan(1, false)
        .await
        .expect_err("a folder that is not there cannot scan")
        .to_string();
    assert!(
        err.contains(&library.root().to_string_lossy().to_string()),
        "the error names the folder: {err}"
    );
    assert!(library.available(a_id).await);
    assert!(library.available(b_id).await);

    // Plugged back in: nothing was lost, nothing has to be read again.
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    library.flac_at(&a, &[("TITLE", "a")]);
    library.flac_at(&b, &[("TITLE", "b")]);
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert!(library.available(a_id).await);
}

/// The other way a root cannot be listed.
#[tokio::test]
async fn a_root_that_is_no_longer_a_directory_hides_nothing() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&a).await;

    std::fs::remove_dir_all(library.root()).unwrap();
    std::fs::write(library.root(), b"not a folder").unwrap();
    assert!(library.try_scan(1, false).await.is_err());
    assert!(library.available(id).await);
}

/// The folder is there and answers: this is what somebody emptying it looks
/// like, and their tracks do go.
#[tokio::test]
async fn a_folder_really_emptied_still_loses_its_tracks() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    let b = library.flac("sub/b.flac", &[("TITLE", "b")]);
    library.scan().await;
    let (a_id, b_id) = (library.id(&a).await, library.id(&b).await);

    std::fs::remove_file(&a).unwrap();
    std::fs::remove_dir_all(b.parent().unwrap()).unwrap();
    let summary = library.scan().await;
    assert_eq!(summary.removed, 2);
    assert!(!library.available(a_id).await);
    assert!(!library.available(b_id).await);
}

/// One file gone from a folder that is otherwise fine.
#[tokio::test]
async fn a_deleted_file_is_still_swept() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    let b = library.flac("b.flac", &[("TITLE", "b")]);
    library.scan().await;
    let (a_id, b_id) = (library.id(&a).await, library.id(&b).await);

    std::fs::remove_file(&a).unwrap();
    assert_eq!(library.scan().await.removed, 1);
    assert!(!library.available(a_id).await);
    assert!(library.available(b_id).await);
}

/// A subfolder the walk may not enter says nothing about the files in it,
/// and a real deletion beside it is still seen.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_subfolder_keeps_its_tracks() {
    use std::os::unix::fs::PermissionsExt;

    let library = Library::new().await;
    let kept = library.flac("locked/kept.flac", &[("TITLE", "kept")]);
    let gone = library.flac("open/gone.flac", &[("TITLE", "gone")]);
    library.scan().await;
    let (kept_id, gone_id) = (library.id(&kept).await, library.id(&gone).await);

    let locked = kept.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not stop anything here.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    std::fs::remove_file(&gone).unwrap();
    let summary = library.try_scan(1, false).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    let summary = summary.expect("the rest of the folder scans");
    assert!(
        summary.errors >= 1,
        "the user is told something was skipped"
    );
    assert!(library.available(kept_id).await, "still there, just unread");
    assert!(!library.available(gone_id).await, "really deleted");
}

/// The forced passes did not read what the walk could not enter, so they are
/// not done: once it can be read, the fast path would leave those files as
/// they are.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_subfolder_holds_the_forced_passes_back() {
    use std::os::unix::fs::PermissionsExt;

    let library = Library::new().await;
    let path = library.flac(
        "locked/a.flac",
        &[("TITLE", "t"), ("ARTIST", "Beta"), ("ARTIST", "Alpha")],
    );
    library.scan().await;
    library.credit_only_first(&path).await;
    library.clear_setting("scan.artist_backfill_done.1").await;

    let locked = path.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not stop anything here.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let summary = library.try_scan(1, false).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(summary.expect("the scan runs").errors >= 1);
    assert_eq!(library.setting("scan.artist_backfill_done.1").await, None);
    assert_eq!(
        library
            .setting("scan.artist_backfill_attempts.1")
            .await
            .as_deref(),
        Some("1")
    );

    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
    assert!(library
        .setting("scan.artist_backfill_done.1")
        .await
        .is_some());
}

/// A drive that is not mounted leaves an empty directory behind on Linux,
/// which answers like any other. What tells it from a folder somebody emptied
/// is that it was a mount point when it held files, and is not one now. Stood
/// in for here by the stored answer; the real thing was tried by hand with a
/// tmpfs.
#[cfg(unix)]
#[tokio::test]
async fn an_empty_mount_point_hides_nothing() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&a).await;
    assert_ne!(
        library.setting("scan.root_was_mount.1").await.as_deref(),
        Some("1"),
        "a plain directory"
    );

    std::fs::remove_file(&a).unwrap();
    super::write_counter(&library.pool, "scan.root_was_mount.1", 1)
        .await
        .unwrap();
    let err = library.try_scan(1, false).await.expect_err("refused");
    assert!(err.to_string().contains("not mounted"), "{err}");
    assert!(library.available(id).await);

    // Nothing says a drive was ever there: the emptying is real.
    super::write_counter(&library.pool, "scan.root_was_mount.1", 0)
        .await
        .unwrap();
    assert_eq!(library.scan().await.removed, 1);
    assert!(!library.available(id).await);
}

/// What the root of a drive always holds: a directory nobody may read, with no
/// track of ours in it. Nothing to report, and nothing held back, or a scan
/// would report an error and re-read the folder for good.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_directory_with_no_tracks_is_nothing_to_report() {
    use std::os::unix::fs::PermissionsExt;

    let library = Library::new().await;
    library.flac("a.flac", &[("TITLE", "a")]);
    let lost = library.root().join("lost+found");
    std::fs::create_dir(&lost).unwrap();
    std::fs::set_permissions(&lost, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = std::fs::read_dir(&lost).is_err();
    let first = library.try_scan(1, false).await;
    let second = library.try_scan(1, false).await;
    std::fs::set_permissions(&lost, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !denied {
        // Running as root: permissions do not stop anything here.
        return;
    }

    assert_eq!(first.expect("scans").errors, 0);
    let second = second.expect("scans");
    assert_eq!(second.errors, 0);
    assert_eq!(second.skipped, 1, "the fast path is back on the next scan");
    assert!(library
        .setting("scan.artist_backfill_done.1")
        .await
        .is_some());
}

/// A directory that stays unreadable with tracks in it spends the passes'
/// retries and no more: the ReplayGain pass among them, which had no budget and
/// would have re-read every track without gains at every scan.
#[cfg(unix)]
#[tokio::test]
async fn a_directory_that_stays_unreadable_does_not_cost_the_fast_path_for_good() {
    use std::os::unix::fs::PermissionsExt;

    let library = Library::new().await;
    library.flac("a.flac", &[("TITLE", "a")]);
    let locked = library.flac("locked/b.flac", &[("TITLE", "b")]);
    library.scan().await;
    for pass in ["rg_backfill", "artist_backfill"] {
        library.clear_setting(&format!("scan.{pass}_done.1")).await;
    }
    let dir = locked.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&dir).is_ok() {
        // Running as root: permissions do not stop anything here.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let mut skipped = Vec::new();
    for _ in 0..4 {
        match library.try_scan(1, false).await {
            Ok(summary) => skipped.push(summary.skipped),
            Err(err) => {
                std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
                panic!("{err}");
            }
        }
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(library.setting("scan.rg_backfill_done.1").await.is_some());
    assert!(library
        .setting("scan.artist_backfill_done.1")
        .await
        .is_some());
    assert_eq!(skipped.len(), 4);
}

// ─── Several artists in one file ────────────────────────────────────────

/// Repeated Vorbis comments, one artist each.
#[tokio::test]
async fn repeated_artist_comments_are_all_credited_in_order() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[("TITLE", "t"), ("ARTIST", "Beta"), ("ARTIST", "Alpha")],
    );
    library.scan().await;
    let (names, primary) = library.credits(&path).await;
    assert_eq!(names, ["Beta", "Alpha"]);
    assert_eq!(primary.as_deref(), Some("Beta"), "the first one leads");
}

/// `ARTISTS` lists them where `ARTIST` is the credit as printed.
#[tokio::test]
async fn the_artists_tag_says_more_than_the_printed_credit() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta feat. Alpha"),
            ("ARTISTS", "Beta"),
            ("ARTISTS", "Alpha"),
        ],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
}

/// One name there says nothing `ARTIST` does not, and is what a tagger that
/// changed `ARTIST` and knows nothing of this one leaves behind.
#[tokio::test]
async fn a_single_artists_value_does_not_overrule_the_artist() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[("TITLE", "t"), ("ARTIST", "New"), ("ARTISTS", "Old")],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["New"]);
}

/// With no `ARTIST` at all it is the only credit there is.
#[tokio::test]
async fn the_artists_tag_alone_is_read() {
    let library = Library::new().await;
    let path = library.flac("a.flac", &[("TITLE", "t"), ("ARTISTS", "Only")]);
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Only"]);
}

/// A comma is still part of a name, multi-value or not.
#[tokio::test]
async fn a_comma_stays_inside_a_name() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Tyler, The Creator"),
            ("ARTIST", "Beta"),
        ],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Tyler, The Creator", "Beta"]);
}

/// The joined form that was already read still is, beside the new ones.
#[tokio::test]
async fn a_joined_value_still_splits() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta; Alpha"),
            ("ARTIST", "Gamma"),
        ],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha", "Gamma"]);
}

/// `track_artist` holds an artist once per role: the same name twice used
/// to abort the whole scan transaction on the second insert.
#[tokio::test]
async fn the_same_artist_twice_is_one_credit() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[("TITLE", "t"), ("ARTIST", "Beta"), ("ARTIST", "beta")],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta"]);
    let joined = library.flac("b.flac", &[("TITLE", "u"), ("ARTIST", "Beta; BETA")]);
    library.scan().await;
    assert_eq!(library.names(&joined).await, ["Beta"]);
}

/// ID3v2.4's multi-value frame.
#[tokio::test]
async fn a_multi_value_tpe1_is_read() {
    let library = Library::new().await;
    let path = library.path("a.mp3");
    mp3(&path, vec![tpe1("Beta\0Alpha")]);
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
}

/// The frame the common taggers write `ARTISTS` to on an MP3.
#[tokio::test]
async fn a_txxx_artists_frame_is_read() {
    let library = Library::new().await;
    let path = library.path("a.mp3");
    mp3(
        &path,
        vec![tpe1("Beta feat. Alpha"), txxx_artists("Beta\0Alpha")],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
}

/// The freeform atom on an M4A, one value per data entry.
#[tokio::test]
async fn an_mp4_artists_atom_is_read() {
    let library = Library::new().await;
    let path = library.path("a.m4a");
    std::fs::copy(fixture("silence.m4a"), &path).unwrap();
    let mut tag = Ilst::default();
    tag.set_title("t".to_string());
    tag.set_artist("Beta feat. Alpha".to_string());
    tag.insert(Atom::new(
        AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: "ARTISTS".into(),
        },
        AtomData::UTF8("Beta".to_string()),
    ));
    tag.insert(Atom::new(
        AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: "ARTISTS".into(),
        },
        AtomData::UTF8("Alpha".to_string()),
    ));
    tag.save_to_path(&path, WriteOptions::default()).unwrap();
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
}

/// A library read before this holds the first value alone, and the fast
/// path never reopens an unchanged file: one forced pass repairs it, and
/// only one.
#[tokio::test]
async fn an_unchanged_file_gets_its_other_artists_once() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[("TITLE", "t"), ("ARTIST", "Beta"), ("ARTIST", "Alpha")],
    );
    library.scan().await;

    library.credit_only_first(&path).await;
    library.clear_setting("scan.artist_backfill_done.1").await;
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
    assert_eq!(
        library
            .setting("scan.artist_backfill_done.1")
            .await
            .as_deref(),
        Some("true")
    );

    // Marked: the fast path is back, and leaves the row alone.
    library.credit_only_first(&path).await;
    assert_eq!(library.scan().await.skipped, 1);
    assert_eq!(library.names(&path).await, ["Beta"]);
}

/// A split the user made on a comma-joined artist, against every way back
/// to the phantom: the forced pass, a deep rescan and a changed file. The
/// last is what any rating or tag written from the app causes, since the
/// editor stores a hash the scan does not keep.
#[tokio::test]
async fn a_split_the_user_made_survives_every_rescan() {
    let library = Library::new().await;
    let path = library.flac("a.flac", &[("TITLE", "t"), ("ARTIST", "Beta, Alpha")]);
    library.scan().await;
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);

    library.clear_setting("scan.artist_backfill_done.1").await;
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"], "forced pass");

    library.try_scan(1, true).await.unwrap();
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"], "deep rescan");

    move_mtime_forward(&path);
    assert_eq!(library.scan().await.updated, 1, "the rewriting branch");
    assert_eq!(
        library.names(&path).await,
        ["Beta", "Alpha"],
        "changed file"
    );
    let (_, primary) = library.credits(&path).await;
    assert_eq!(primary.as_deref(), Some("Beta"));
}

/// The album a split track belongs to stays where it is when the file is
/// rewritten: it is keyed on the name the split did not touch.
#[tokio::test]
async fn a_kept_split_does_not_move_the_track_to_another_album() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta, Alpha"),
            ("ALBUM", "Record"),
        ],
    );
    library.scan().await;
    let album = |pool: SqlitePool, id: i64| async move {
        sqlx::query_scalar::<_, i64>("SELECT album_id FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let id = library.id(&path).await;
    let before = album(library.pool.clone(), id).await;
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();
    move_mtime_forward(&path);
    assert_eq!(library.scan().await.updated, 1);
    assert_eq!(album(library.pool.clone(), id).await, before);
    assert_eq!(library.albums("Record").await.len(), 1);
}

/// The same, on a file with several values one of which the user split.
/// The count of names in the file no longer tells a split from a stale row.
#[tokio::test]
async fn a_split_of_one_of_several_values_survives() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta, Alpha"),
            ("ARTIST", "Gamma"),
        ],
    );
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta, Alpha", "Gamma"]);
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();
    let split = ["Beta", "Alpha", "Gamma"];
    assert_eq!(library.names(&path).await, split);

    library.clear_setting("scan.artist_backfill_done.1").await;
    library.scan().await;
    assert_eq!(library.names(&path).await, split, "forced pass");
    move_mtime_forward(&path);
    library.scan().await;
    assert_eq!(library.names(&path).await, split, "changed file");
}

/// Two joined names in one file, one of them split: the other is a name with
/// a comma in it, and stays whole.
#[tokio::test]
async fn a_split_of_one_joined_name_leaves_the_other_whole() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta, Alpha; Tyler, The Creator"),
        ],
    );
    library.scan().await;
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();
    let split = ["Beta", "Alpha", "Tyler, The Creator"];
    assert_eq!(library.names(&path).await, split);

    library.clear_setting("scan.artist_backfill_done.1").await;
    library.scan().await;
    assert_eq!(library.names(&path).await, split, "forced pass");
    move_mtime_forward(&path);
    library.scan().await;
    assert_eq!(library.names(&path).await, split, "changed file");
}

/// The first name leads the track, so the same names in another order are
/// another credit.
#[tokio::test]
async fn a_file_that_reorders_a_split_credit_wins() {
    let library = Library::new().await;
    let path = library.flac("a.flac", &[("TITLE", "t"), ("ARTIST", "Beta, Alpha")]);
    library.scan().await;
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);

    library.flac_at(&path, &[("TITLE", "t"), ("ARTIST", "Alpha, Beta")]);
    move_mtime_forward(&path);
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Alpha, Beta"]);
}

/// A file that does change its mind wins over a stored split: this is not
/// the same credit any more.
#[tokio::test]
async fn a_retagged_file_replaces_the_credits() {
    let library = Library::new().await;
    let path = library.flac("a.flac", &[("TITLE", "t"), ("ARTIST", "Beta, Alpha")]);
    library.scan().await;
    let phantom: i64 = sqlx::query_scalar("SELECT id FROM artist WHERE name = 'Beta, Alpha'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    crate::commands::artist_split::split_artist_inner(&library.pool, phantom)
        .await
        .unwrap();

    library.flac_at(&path, &[("TITLE", "t"), ("ARTIST", "Delta")]);
    move_mtime_forward(&path);
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Delta"]);
}

fn edit_artist(path: &Path, artist: &str) {
    let edit = TrackEdit {
        artist: Some(artist.to_string()),
        ..Default::default()
    };
    patch_file(path, &TagPatch::Fields(&edit)).expect("patch");
}

/// An artist edited in WaveFlow has to win at the next scan: an `ARTISTS`
/// left behind would bring the old names back over it. One container per
/// shape of the tag.
#[tokio::test]
async fn editing_the_artist_takes_a_stale_artists_tag_with_it() {
    let library = Library::new().await;

    let flac = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Old"),
            ("ARTIST", "Older"),
            ("ARTISTS", "Old"),
            ("ARTISTS", "Older"),
            ("ARTISTS", "Oldest"),
        ],
    );
    let mp3_path = library.path("b.mp3");
    mp3(
        &mp3_path,
        vec![tpe1("Old"), txxx_artists("Old\0Older\0Oldest")],
    );
    let m4a = library.path("c.m4a");
    std::fs::copy(fixture("silence.m4a"), &m4a).unwrap();
    let mut ilst = Ilst::default();
    ilst.set_title("t".to_string());
    ilst.set_artist("Old".to_string());
    for name in ["Old", "Older", "Oldest"] {
        ilst.insert(Atom::new(
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "ARTISTS".into(),
            },
            AtomData::UTF8(name.to_string()),
        ));
    }
    ilst.save_to_path(&m4a, WriteOptions::default()).unwrap();

    library.scan().await;
    for path in [&flac, &mp3_path, &m4a] {
        assert_eq!(
            library.names(path).await,
            ["Old", "Older", "Oldest"],
            "{path:?}"
        );
        edit_artist(path, "New");
        move_mtime_forward(path);
    }
    library.scan().await;
    for path in [&flac, &mp3_path, &m4a] {
        assert_eq!(library.names(path).await, ["New"], "{path:?}");
    }
}

/// An edit that does not name the artist leaves `ARTISTS` where it is.
#[tokio::test]
async fn editing_another_field_keeps_the_artists_tag() {
    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[
            ("TITLE", "t"),
            ("ARTIST", "Beta"),
            ("ARTISTS", "Beta"),
            ("ARTISTS", "Alpha"),
        ],
    );
    let edit = TrackEdit {
        title: Some("u".to_string()),
        ..Default::default()
    };
    patch_file(&path, &TagPatch::Fields(&edit)).unwrap();
    library.scan().await;
    assert_eq!(library.names(&path).await, ["Beta", "Alpha"]);
}

/// A real DSF whose ID3v2 block holds this tag.
fn dsf(path: &Path, tag: id3::Tag) {
    let mut bytes = dsf_bytes();
    let offset = bytes.len() as u64;
    tag.write_to(&mut bytes, id3::Version::Id3v24).unwrap();
    let total = bytes.len() as u64;
    bytes[12..20].copy_from_slice(&total.to_le_bytes());
    bytes[20..28].copy_from_slice(&offset.to_le_bytes());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// DSD has its own reader, by the same rule; and its own writer, which has to
/// take the stale frame as well.
#[tokio::test]
async fn a_dsf_credits_every_artist_and_an_edit_replaces_them() {
    use id3::TagLike;

    let library = Library::new().await;
    let joined = library.path("a.dsf");
    let mut tag = id3::Tag::new();
    tag.set_title("t");
    tag.set_artist("Beta\0Alpha");
    dsf(&joined, tag);
    let listed = library.path("b.dsf");
    let mut tag = id3::Tag::new();
    tag.set_title("u");
    tag.set_artist("Beta feat. Alpha");
    tag.add_frame(id3::frame::ExtendedText {
        description: "ARTISTS".to_string(),
        value: "Beta\0Alpha".to_string(),
    });
    dsf(&listed, tag);

    library.scan().await;
    assert_eq!(library.names(&joined).await, ["Beta", "Alpha"]);
    assert_eq!(library.names(&listed).await, ["Beta", "Alpha"]);

    edit_artist(&listed, "New");
    move_mtime_forward(&listed);
    library.scan().await;
    assert_eq!(library.names(&listed).await, ["New"]);
}

/// What the workshop shows, and what its undo writes back, is every name.
#[tokio::test]
async fn the_workshop_reads_every_artist() {
    use crate::commands::tag_workshop::read_values;
    use waveflow_core::tag_workshop::Field;

    let library = Library::new().await;
    let path = library.flac(
        "a.flac",
        &[("TITLE", "t"), ("ARTIST", "Beta"), ("ARTIST", "Alpha")],
    );
    let values = read_values(&path).unwrap();
    assert_eq!(
        values.get(&Field::Artist).map(String::as_str),
        Some("Beta; Alpha")
    );
}

// ─── Albums that only share a title ─────────────────────────────────────

fn album_track<'a>(
    title: &'a str,
    artist: &'a str,
    album: &'a str,
    number: &'a str,
    year: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut comments = vec![("TITLE", title), ("ARTIST", artist), ("ALBUM", album)];
    if !number.is_empty() {
        comments.push(("TRACKNUMBER", number));
    }
    if !year.is_empty() {
        comments.push(("DATE", year));
    }
    comments
}

/// Three bands, three "Greatest Hits", three folders.
#[tokio::test]
async fn three_albums_sharing_a_title_stay_three_albums() {
    let library = Library::new().await;
    for (band, year) in [("Abba", "1992"), ("Queen", "1981"), ("Toto", "1996")] {
        for number in ["1", "2"] {
            library.flac(
                &format!("{band}/{number}.flac"),
                &album_track(number, band, "Greatest Hits", number, year),
            );
        }
    }
    library.scan().await;
    assert_eq!(
        library.albums("Greatest Hits").await,
        [
            ("Abba".to_string(), false, 2),
            ("Queen".to_string(), false, 2),
            ("Toto".to_string(), false, 2),
        ]
    );
}

/// The same year is no proof: their numbers collide.
#[tokio::test]
async fn the_same_year_is_not_enough_when_the_numbers_collide() {
    let library = Library::new().await;
    for band in ["Abba", "Queen", "Toto"] {
        for number in ["1", "2"] {
            library.flac(
                &format!("{band}/{number}.flac"),
                &album_track(number, band, "Greatest Hits", number, "1999"),
            );
        }
    }
    library.scan().await;
    assert_eq!(library.albums("Greatest Hits").await.len(), 3);
}

/// Nor is the same folder: a flat folder of albums collides as well.
#[tokio::test]
async fn the_same_folder_is_not_enough_when_the_numbers_collide() {
    let library = Library::new().await;
    for band in ["Abba", "Queen", "Toto"] {
        for number in ["1", "2"] {
            library.flac(
                &format!("{band}{number}.flac"),
                &album_track(number, band, "Greatest Hits", number, ""),
            );
        }
    }
    library.scan().await;
    assert_eq!(library.albums("Greatest Hits").await.len(), 3);
}

/// No year, no shared folder: nothing ties them but the title.
#[tokio::test]
async fn unnumbered_albums_in_their_own_folders_stay_apart() {
    let library = Library::new().await;
    for band in ["Abba", "Queen", "Toto"] {
        library.flac(
            &format!("{band}/x.flac"),
            &album_track(band, band, "Live", "", "1999"),
        );
    }
    library.scan().await;
    assert_eq!(library.albums("Live").await.len(), 3);
}

/// What the pass is for: one folder, one track per artist, no album artist
/// and no compilation flag.
#[tokio::test]
async fn a_numbered_compilation_in_one_folder_is_still_merged() {
    let library = Library::new().await;
    let names = ["Abba", "Queen", "Toto", "Yes"];
    for (i, band) in names.iter().enumerate() {
        let number = (i + 1).to_string();
        library.flac(
            &format!("Comp/{number}.flac"),
            &album_track(band, band, "Coffee Shop", &number, ""),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Coffee Shop").await,
        [("Various Artists".to_string(), true, 4)]
    );
}

/// The pack with no numbers at all has only its folder to go on.
#[tokio::test]
async fn an_unnumbered_compilation_in_one_folder_is_still_merged() {
    let library = Library::new().await;
    for band in ["Abba", "Queen", "Toto"] {
        library.flac(
            &format!("Pack/{band}.flac"),
            &album_track(band, band, "Soothing Breeze", "", ""),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Soothing Breeze").await,
        [("Various Artists".to_string(), true, 3)]
    );
}

/// One folder per artist, the same year and numbers that follow on.
#[tokio::test]
async fn a_compilation_spread_over_folders_merges_on_its_year() {
    let library = Library::new().await;
    for (i, band) in ["Abba", "Queen", "Toto"].iter().enumerate() {
        let number = (i + 1).to_string();
        library.flac(
            &format!("Pack/{band}/x.flac"),
            &album_track(band, band, "Summer Pack", &number, "2003"),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Summer Pack").await,
        [("Various Artists".to_string(), true, 3)]
    );
}

/// A disc number keeps two track ones apart.
#[tokio::test]
async fn two_discs_do_not_collide() {
    let library = Library::new().await;
    for (band, disc) in [("Abba", "1"), ("Queen", "2"), ("Toto", "3")] {
        library.flac(
            &format!("Box/{band}.flac"),
            &[
                ("TITLE", band),
                ("ARTIST", band),
                ("ALBUM", "The Box"),
                ("TRACKNUMBER", "1"),
                ("DISCNUMBER", disc),
            ],
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("The Box").await,
        [("Various Artists".to_string(), true, 3)]
    );
}

/// A real compilation and an unrelated album of the same title: one does
/// not drag the other in, whichever arrives first.
#[tokio::test]
async fn a_compilation_leaves_an_album_of_the_same_title_alone() {
    let library = Library::new().await;
    for band in ["Abba", "Toto", "Yes"] {
        library.flac(
            &format!("Comp/{band}.flac"),
            &album_track(band, band, "Greatest Hits", "", ""),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Greatest Hits").await,
        [("Various Artists".to_string(), true, 3)]
    );

    // Arrives later, with the compilation row already there: the scan's own
    // "reuse a compilation of this title" used to swallow it, track by track.
    for number in ["1", "2"] {
        library.flac(
            &format!("Queen/{number}.flac"),
            &album_track(number, "Queen", "Greatest Hits", number, "1981"),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Greatest Hits").await,
        [
            ("Queen".to_string(), false, 2),
            ("Various Artists".to_string(), true, 3),
        ]
    );
}

/// A title holds one Various Artists row. A second compilation elsewhere
/// cannot have its own and is not folded into the first: it stays as a scan
/// of its files makes it, one album per artist.
#[tokio::test]
async fn a_second_compilation_of_the_title_stays_as_it_is() {
    let library = Library::new().await;
    for dir in ["One", "Two"] {
        for band in ["Abba", "Queen", "Toto"] {
            let artist = format!("{band} {dir}");
            library.flac(
                &format!("{dir}/{band}.flac"),
                &album_track(band, &artist, "Mixtape", "", ""),
            );
        }
    }
    library.scan().await;
    let albums = library.albums("Mixtape").await;
    assert_eq!(albums.len(), 4, "{albums:?}");
    assert!(albums.contains(&("Various Artists".to_string(), true, 3)));
    // And it stays settled.
    library.scan().await;
    assert_eq!(library.albums("Mixtape").await, albums);
}

/// Files whose artist is literally "Various Artists" make a row under that
/// key, with no compilation flag. It is where the others land: taken for one
/// of the rows to merge, it would have deleted itself, and the record with it.
#[tokio::test]
async fn a_row_already_under_various_artists_is_the_one_merged_into() {
    let library = Library::new().await;
    library.flac(
        "Comp/va.flac",
        &album_track("va", "Various Artists", "Mixtape", "", ""),
    );
    for band in ["Abba", "Queen", "Toto"] {
        library.flac(
            &format!("Comp/{band}.flac"),
            &album_track(band, band, "Mixtape", "", ""),
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Mixtape").await,
        [("Various Artists".to_string(), true, 4)]
    );
    let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE album_id IS NULL")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    assert_eq!(orphans, 0);
}

/// Rows left fragmented beside an existing Various Artists row of the title
/// join it, when they are its neighbours and take no place in it.
#[tokio::test]
async fn neighbours_of_an_existing_compilation_join_it() {
    let library = Library::new().await;
    for (i, band) in ["Abba", "Queen", "Toto"].iter().enumerate() {
        let number = (i + 1).to_string();
        library.flac(
            &format!("Comp/{band}.flac"),
            &[
                ("TITLE", band),
                ("ARTIST", band),
                ("ALBUM", "Mixtape"),
                ("TRACKNUMBER", number.as_str()),
                ("COMPILATION", "1"),
            ],
        );
    }
    library.scan().await;
    assert_eq!(
        library.albums("Mixtape").await,
        [("Various Artists".to_string(), true, 3)]
    );
    // Rows the reuse in `upsert_album` did not catch: an older build, or
    // files that were elsewhere when they were scanned.
    let va: i64 = sqlx::query_scalar("SELECT id FROM album WHERE title = 'Mixtape'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    for (i, band) in ["Yes", "Rush", "Kiss"].iter().enumerate() {
        let number = (i + 4).to_string();
        let path = library.flac(
            &format!("Comp/{band}.flac"),
            &album_track(band, band, "Mixtape", &number, ""),
        );
        let _ = path;
    }
    library.scan().await;
    assert_eq!(
        library.albums("Mixtape").await,
        [("Various Artists".to_string(), true, 6)]
    );
    let still: i64 = sqlx::query_scalar("SELECT id FROM album WHERE title = 'Mixtape'")
        .fetch_one(&library.pool)
        .await
        .unwrap();
    assert_eq!(still, va);
}

/// A rewritten track of a merged compilation stays in it, and a file added
/// beside its tracks joins it, without re-fragmenting anything.
#[tokio::test]
async fn a_merged_compilation_does_not_fall_apart() {
    let library = Library::new().await;
    let mut first = None;
    for (i, band) in ["Abba", "Queen", "Toto"].iter().enumerate() {
        let number = (i + 1).to_string();
        let path = library.flac(
            &format!("Comp/{number}.flac"),
            &album_track(band, band, "Coffee Shop", &number, ""),
        );
        first.get_or_insert(path);
    }
    library.scan().await;

    move_mtime_forward(first.as_ref().unwrap());
    assert_eq!(library.scan().await.updated, 1);
    library.flac(
        "Comp/4.flac",
        &album_track("Yes", "Yes", "Coffee Shop", "4", ""),
    );
    library.scan().await;
    library.try_scan(1, true).await.unwrap();
    assert_eq!(
        library.albums("Coffee Shop").await,
        [("Various Artists".to_string(), true, 4)]
    );
}

/// Albums an older build merged wrongly are left as they are: nothing in the
/// row says the merge was the scanner's guess rather than the tags' word, so
/// neither a scan nor a deep one takes them apart in silence.
#[tokio::test]
async fn an_album_already_merged_is_not_split_by_a_scan() {
    let library = Library::new().await;
    for band in ["Abba", "Queen", "Toto"] {
        library.flac(
            &format!("{band}/1.flac"),
            &album_track(band, band, "Greatest Hits", "1", ""),
        );
    }
    library.scan().await;
    // The state an older build left: one row under Various Artists.
    let mut tx = library.pool.begin().await.unwrap();
    let va = waveflow_core::scanner::upsert_artist(&mut tx, "Various Artists")
        .await
        .unwrap()
        .unwrap();
    let survivor: i64 = sqlx::query_scalar("SELECT MIN(id) FROM album")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE track SET album_id = ?")
        .bind(survivor)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM album WHERE id <> ?")
        .bind(survivor)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE album SET artist_id = ?, album_artist = 'Various Artists', is_compilation = 1",
    )
    .bind(va)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let expected = [("Various Artists".to_string(), true, 3)];
    library.scan().await;
    assert_eq!(library.albums("Greatest Hits").await, expected);
    library.try_scan(1, true).await.unwrap();
    assert_eq!(library.albums("Greatest Hits").await, expected);
}

// ─── A file that moved ──────────────────────────────────────────────────

/// Everything the user gave the track follows it to its new place.
#[tokio::test]
async fn a_moved_file_keeps_everything() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a"), ("ARTIST", "Beta")]);
    library.scan().await;
    let id = library.id(&old).await;
    library.make_it_loved(id).await;

    let new = library.path("Moved/here.flac");
    std::fs::rename(&old, &new).unwrap();
    library.scan().await;

    assert_eq!(library.track_count().await, 1, "no second row");
    assert_eq!(library.track_path(id).await, new.to_string_lossy());
    assert!(library.available(id).await);
    assert_eq!(library.love(id).await, (Some(204), 1, 1, 1));
}

/// A rating written from the app leaves the editor's hash in the row, the
/// whole file's where the scan keeps its own: comparing the two would never
/// recognise it. The very tracks somebody cares about.
#[tokio::test]
async fn a_file_rated_in_the_app_is_recognised_after_a_move() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&old).await;
    patch_file(&old, &TagPatch::Rating(Some(204))).unwrap();
    rehash_track_file(&library.pool, id, &old).await.unwrap();

    let new = library.path("b.flac");
    std::fs::rename(&old, &new).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 1);
    assert_eq!(library.track_path(id).await, new.to_string_lossy());
    assert!(library.available(id).await);
}

/// A write from the app that outgrows the padding changes the file's length:
/// the row has to follow, or a move before the next scan finds no row of that
/// size at all.
#[tokio::test]
async fn a_file_that_grew_in_the_app_is_recognised_after_a_move() {
    let library = Library::new().await;
    let old = library.path("a.mp3");
    mp3(&old, vec![tpe1("Beta")]);
    library.scan().await;
    let id = library.id(&old).await;
    let before = std::fs::metadata(&old).unwrap().len();

    let edit = TrackEdit {
        title: Some("x".repeat(64 * 1024)),
        ..Default::default()
    };
    patch_file(&old, &TagPatch::Fields(&edit)).unwrap();
    assert_ne!(
        std::fs::metadata(&old).unwrap().len(),
        before,
        "the premise"
    );
    rehash_track_file(&library.pool, id, &old).await.unwrap();

    let new = library.path("b.mp3");
    std::fs::rename(&old, &new).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 1);
    assert_eq!(library.track_path(id).await, new.to_string_lossy());
    assert!(library.available(id).await);
}

/// A copy keeps its bytes and gets a new date: the date cannot be required.
#[tokio::test]
async fn a_moved_file_with_a_new_date_is_recognised() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&old).await;

    let new = library.path("b.flac");
    std::fs::rename(&old, &new).unwrap();
    move_mtime_forward(&new);
    library.scan().await;
    assert_eq!(library.track_count().await, 1);
    assert_eq!(library.track_path(id).await, new.to_string_lossy());
}

/// Swept by an earlier scan, and back somewhere else later.
#[tokio::test]
async fn a_file_hidden_earlier_is_recognised_where_it_comes_back() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&old).await;
    let bytes = std::fs::read(&old).unwrap();

    std::fs::remove_file(&old).unwrap();
    library.scan().await;
    assert!(!library.available(id).await);

    let new = library.path("back/a.flac");
    std::fs::write(&new, bytes).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 1);
    assert!(library.available(id).await);
    assert_eq!(library.track_path(id).await, new.to_string_lossy());
}

/// Two copies of a file that vanished: nothing says which is the one.
#[tokio::test]
async fn two_new_copies_link_nothing() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&old).await;
    library.make_it_loved(id).await;

    let (one, two) = (library.path("x/one.flac"), library.path("y/two.flac"));
    std::fs::copy(&old, &one).unwrap();
    std::fs::rename(&old, &two).unwrap();
    library.scan().await;

    assert_eq!(library.track_count().await, 3);
    assert!(!library.available(id).await);
    assert_eq!(library.track_path(id).await, old.to_string_lossy());
    assert_eq!(library.love(library.id(&one).await).await, (None, 0, 0, 0));
    assert_eq!(library.love(library.id(&two).await).await, (None, 0, 0, 0));
}

/// Two identical files, both gone, one new: which one moved?
#[tokio::test]
async fn one_new_file_for_two_lost_rows_links_nothing() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    let b = library.path("b.flac");
    std::fs::copy(&a, &b).unwrap();
    library.scan().await;
    let (a_id, b_id) = (library.id(&a).await, library.id(&b).await);

    let new = library.path("c.flac");
    std::fs::rename(&a, &new).unwrap();
    std::fs::remove_file(&b).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 3);
    assert!(!library.available(a_id).await);
    assert!(!library.available(b_id).await);
}

/// The twin still in its place is no candidate: only the one that left is.
#[tokio::test]
async fn a_twin_that_stayed_does_not_stop_the_move() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    let b = library.path("b.flac");
    std::fs::copy(&a, &b).unwrap();
    library.scan().await;
    let (a_id, b_id) = (library.id(&a).await, library.id(&b).await);

    let new = library.path("moved/b.flac");
    std::fs::rename(&b, &new).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(a_id).await, a.to_string_lossy());
    assert_eq!(library.track_path(b_id).await, new.to_string_lossy());
    assert!(library.available(a_id).await);
    assert!(library.available(b_id).await);
}

/// A copy beside a file that stayed is a copy, not a move.
#[tokio::test]
async fn a_copy_of_a_file_still_there_is_a_new_track() {
    let library = Library::new().await;
    let a = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&a).await;

    let copy = library.path("copy.flac");
    std::fs::copy(&a, &copy).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(id).await, a.to_string_lossy());
    assert!(library.available(id).await);
}

/// Other bytes under the same size are another file: same title, same
/// length, a different comment of the same size.
#[tokio::test]
async fn another_file_of_the_same_size_links_nothing() {
    let library = Library::new().await;
    let old = library.flac("a.flac", &[("TITLE", "a"), ("COMMENT", "11")]);
    library.scan().await;
    let id = library.id(&old).await;
    std::fs::remove_file(&old).unwrap();
    let new = library.flac("b.flac", &[("TITLE", "a"), ("COMMENT", "22")]);
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert!(!library.available(id).await);
    assert_ne!(library.id(&new).await, id);
}

/// From one folder of the library to another, whichever is scanned first.
#[tokio::test]
async fn a_move_between_two_folders_is_seen_either_way() {
    for destination_first in [false, true] {
        let library = Library::new().await;
        library.add_folder(2, 1, "music2").await;
        let old = library.flac("a.flac", &[("TITLE", "a")]);
        library.scan().await;
        let id = library.id(&old).await;
        library.make_it_loved(id).await;

        let new = library.dir.path().join("music2").join("a.flac");
        std::fs::rename(&old, &new).unwrap();
        if destination_first {
            library.scan_folder(2).await;
            library.scan_folder(1).await;
        } else {
            library.scan_folder(1).await;
            library.scan_folder(2).await;
        }
        assert_eq!(library.track_count().await, 1, "first: {destination_first}");
        assert_eq!(library.track_path(id).await, new.to_string_lossy());
        assert!(library.available(id).await);
        assert_eq!(library.love(id).await, (Some(204), 1, 1, 1));
        let folder: i64 = sqlx::query_scalar("SELECT folder_id FROM track WHERE id = ?")
            .bind(id)
            .fetch_one(&library.pool)
            .await
            .unwrap();
        assert_eq!(folder, 2);
    }
}

/// Another library's track is not this library's to take.
#[tokio::test]
async fn another_library_is_left_alone() {
    let library = Library::new().await;
    library.add_folder(2, 2, "elsewhere").await;
    let old = library.flac("a.flac", &[("TITLE", "a")]);
    library.scan().await;
    let id = library.id(&old).await;

    let new = library.dir.path().join("elsewhere").join("a.flac");
    std::fs::rename(&old, &new).unwrap();
    library.scan_folder(2).await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(id).await, old.to_string_lossy());
}

/// The file of a folder whose drive is away is not gone, only out of reach:
/// a copy of it elsewhere is a copy.
#[tokio::test]
async fn a_track_on_an_absent_drive_is_not_taken_for_moved() {
    let library = Library::new().await;
    library.add_folder(2, 1, "usb").await;
    let usb = library.dir.path().join("usb");
    let old = usb.join("a.flac");
    library.flac_at(&old, &[("TITLE", "a")]);
    library.scan_folder(2).await;
    let id = library.id(&old).await;

    let copy = library.path("a.flac");
    std::fs::copy(&old, &copy).unwrap();
    std::fs::remove_dir_all(&usb).unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(id).await, old.to_string_lossy());
    assert!(library.available(id).await);
}

/// The same drive unmounted rather than gone: its mount point is still there,
/// empty, and answers. What the folder was on record gives it away, or the row would
/// follow the copy, with everything on it, and the drive's own file would come
/// back as a stranger.
#[cfg(unix)]
#[tokio::test]
async fn a_track_on_an_unmounted_drive_is_not_taken_for_moved() {
    let library = Library::new().await;
    library.add_folder(2, 1, "usb").await;
    let old = library.dir.path().join("usb").join("a.flac");
    library.flac_at(&old, &[("TITLE", "a")]);
    library.scan_folder(2).await;
    let id = library.id(&old).await;

    let copy = library.path("a.flac");
    std::fs::copy(&old, &copy).unwrap();
    std::fs::remove_file(&old).unwrap();
    super::write_counter(&library.pool, "scan.root_was_mount.2", 1)
        .await
        .unwrap();
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(id).await, old.to_string_lossy());
    assert!(library.available(id).await);
}

/// A share's row is never a candidate: its file does not exist on this disk
/// by construction, which reads as gone.
#[tokio::test]
async fn a_track_on_a_share_is_never_a_candidate() {
    let library = Library::new().await;
    let new = library.flac("a.flac", &[("TITLE", "a")]);
    let size = std::fs::metadata(&new).unwrap().len() as i64;
    let hash = waveflow_core::scanner::hash_file(&new).unwrap();
    std::fs::remove_file(&new).unwrap();
    sqlx::query(
        "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
         VALUES (2, 1, 'wfnet://0f0f/music', 0, 0)",
    )
    .execute(&library.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (id, library_id, folder_id, file_path, file_hash, file_size,
                            file_modified, title, duration_ms, added_at, is_available)
         VALUES (77, 1, 2, 'wfnet://0f0f/music/a.flac', ?, ?, 0, 'a', 1000, 0, 0)",
    )
    .bind(hash)
    .bind(size)
    .execute(&library.pool)
    .await
    .unwrap();

    library.flac_at(&new, &[("TITLE", "a")]);
    library.scan().await;
    assert_eq!(library.track_count().await, 2);
    assert_eq!(library.track_path(77).await, "wfnet://0f0f/music/a.flac");
}
