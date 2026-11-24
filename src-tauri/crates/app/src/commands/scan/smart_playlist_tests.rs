//! A custom smart playlist follows the library a scan changes (#1169):
//! a track scanned in after the playlist was made joins it, a track
//! retagged out of its rules leaves it. Through the real scan, on the
//! migrated schema with foreign keys on, with real files.

use super::scan_folder_inner;
use lofty::config::WriteOptions;
use lofty::ogg::tag::VorbisComments;
use lofty::tag::TagExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::time::Duration;
use waveflow_core::smart_playlists::custom::{self, CustomRules, CustomSort, Predicate, RuleNode};
use waveflow_core::smart_playlists::SmartPlaylistRules;

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
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let root = dir.path().join("music");
        std::fs::create_dir_all(&root).unwrap();
        sqlx::query(
            "INSERT INTO library_folder (id, library_id, path, last_scanned_at, is_watched)
             VALUES (1, 1, ?, 0, 0)",
        )
        .bind(root.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        Self { dir, pool }
    }

    /// A real FLAC under the folder, carrying this title and no other tag.
    fn flac(&self, name: &str, title: &str) -> PathBuf {
        let path = self.dir.path().join("music").join(name);
        std::fs::copy(fixture("silence.flac"), &path).unwrap();
        retitle(&path, title);
        path
    }

    async fn scan(&self) {
        self.scan_with(false).await;
    }

    /// `deep`: every file read again, the unchanged ones included.
    async fn scan_with(&self, deep: bool) {
        let artwork = self.dir.path().join("artwork");
        let summary = scan_folder_inner(&self.pool, &artwork, 1, None, deep)
            .await
            .expect("scan");
        assert_eq!(summary.errors, 0, "every file here is readable");
    }

    async fn id(&self, path: &Path) -> i64 {
        sqlx::query_scalar("SELECT id FROM track WHERE file_path = ?")
            .bind(path.to_string_lossy().to_string())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// A custom smart playlist made the way the editor makes one: the
    /// rules stored, the tracks materialized once.
    async fn smart_playlist(&self, rules: CustomRules) -> i64 {
        let json = SmartPlaylistRules::Custom {
            rules: rules.clone(),
        }
        .to_json()
        .unwrap();
        let id = sqlx::query(
            "INSERT INTO playlist
                 (name, color_id, icon_id, is_smart, smart_rules, position,
                  created_at, updated_at)
             VALUES ('Smart', 'violet', 'sparkles', 1, ?, 0, 0, 0)",
        )
        .bind(json)
        .execute(&self.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        custom::materialize(&self.pool, id, &rules).await.unwrap();
        id
    }

    /// One refresh pass on its own, outside any scan: the playlists written.
    async fn refresh(&self) -> Vec<i64> {
        let mut changed = Vec::new();
        custom::refresh_all(&self.pool, &mut changed).await.unwrap();
        changed
    }

    async fn members(&self, playlist_id: i64) -> Vec<i64> {
        sqlx::query_scalar(
            "SELECT track_id FROM playlist_track WHERE playlist_id = ? ORDER BY position",
        )
        .bind(playlist_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn updated_at(&self, playlist_id: i64) -> i64 {
        sqlx::query_scalar("SELECT updated_at FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tags")
        .join(name)
}

/// Replace the file's comments with this title alone, and move its
/// modification time on so the next scan reads it again.
fn retitle(path: &Path, title: &str) {
    let mut tag = VorbisComments::default();
    tag.push("TITLE".to_string(), title.to_string());
    tag.save_to_path(path, WriteOptions::default())
        .expect("write comments");
    let file = std::fs::File::options().write(true).open(path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_modified(modified + Duration::from_secs(2))
        .unwrap();
}

fn title_contains(value: &str) -> CustomRules {
    CustomRules {
        tree: RuleNode::Leaf {
            predicate: Predicate::TitleContains {
                value: value.to_string(),
            },
        },
        sort: Some(CustomSort::TitleAsc),
        limit: None,
    }
}

/// The issue as reported: a file that matches the rules, copied in and
/// scanned after the playlist was made, is in the playlist — without the
/// playlist being deleted and made again.
#[tokio::test]
async fn a_track_scanned_in_after_the_playlist_joins_it() {
    let lib = Library::new().await;
    let first = lib.flac("a.flac", "Keep A");
    lib.flac("other.flac", "Something else");
    lib.scan().await;
    let playlist = lib.smart_playlist(title_contains("Keep")).await;
    let first = lib.id(&first).await;
    assert_eq!(lib.members(playlist).await, vec![first]);

    let second = lib.flac("b.flac", "Keep B");
    lib.scan().await;
    let second = lib.id(&second).await;
    assert_eq!(
        lib.members(playlist).await,
        vec![first, second],
        "the new match joined, in the rules' order"
    );
}

/// The other way: a track retagged so the rules no longer match it
/// leaves the playlist on the scan that reads the new tag.
#[tokio::test]
async fn a_track_retagged_out_of_the_rules_leaves_it() {
    let lib = Library::new().await;
    let a = lib.flac("a.flac", "Keep A");
    let b = lib.flac("b.flac", "Keep B");
    lib.scan().await;
    let playlist = lib.smart_playlist(title_contains("Keep")).await;
    let (a_id, b_id) = (lib.id(&a).await, lib.id(&b).await);
    assert_eq!(lib.members(playlist).await, vec![a_id, b_id]);

    retitle(&a, "Drop A");
    lib.scan().await;
    assert_eq!(lib.members(playlist).await, vec![b_id]);
}

/// A file deleted from the folder leaves the playlist with the scan that
/// finds it gone, and a playlist at its limit takes the next match.
#[tokio::test]
async fn a_track_gone_from_the_folder_makes_room_for_the_next_match() {
    let lib = Library::new().await;
    let a = lib.flac("a.flac", "Keep A");
    let b = lib.flac("b.flac", "Keep B");
    lib.scan().await;
    let mut rules = title_contains("Keep");
    rules.limit = Some(1);
    let playlist = lib.smart_playlist(rules).await;
    let (a_id, b_id) = (lib.id(&a).await, lib.id(&b).await);
    assert_eq!(lib.members(playlist).await, vec![a_id]);

    std::fs::remove_file(&a).unwrap();
    lib.scan().await;
    assert_eq!(lib.members(playlist).await, vec![b_id]);
}

/// A pass that finds every playlist as it should be writes nothing: no
/// row rewritten, no `updated_at` bumped, no playlist reported changed —
/// which is what keeps a watcher's rescans from rewriting playlists. A
/// track kept through a pass that does write keeps the date it was added.
#[tokio::test]
async fn a_pass_writes_only_what_changed() {
    let lib = Library::new().await;
    let a = lib.flac("a.flac", "Keep A");
    lib.scan().await;
    let playlist = lib.smart_playlist(title_contains("Keep")).await;
    let a_id = lib.id(&a).await;
    sqlx::query("UPDATE playlist_track SET added_at = 42 WHERE track_id = ?")
        .bind(a_id)
        .execute(&lib.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE playlist SET updated_at = 7 WHERE id = ?")
        .bind(playlist)
        .execute(&lib.pool)
        .await
        .unwrap();

    assert!(lib.refresh().await.is_empty());
    assert_eq!(lib.updated_at(playlist).await, 7, "nothing was written");

    let b = lib.flac("b.flac", "Keep B");
    lib.scan().await;
    let b_id = lib.id(&b).await;
    assert_eq!(lib.members(playlist).await, vec![a_id, b_id]);
    assert_ne!(
        lib.updated_at(playlist).await,
        7,
        "the view refetches on it"
    );
    let kept_since: i64 =
        sqlx::query_scalar("SELECT added_at FROM playlist_track WHERE track_id = ?")
            .bind(a_id)
            .fetch_one(&lib.pool)
            .await
            .unwrap();
    assert_eq!(kept_since, 42, "a kept track keeps its date");
}

/// Only custom playlists are the pass's to rewrite: a manual playlist and
/// a generated mix keep what they hold.
#[tokio::test]
async fn other_playlists_are_left_alone() {
    let lib = Library::new().await;
    let a = lib.flac("a.flac", "Keep A");
    lib.scan().await;
    let a_id = lib.id(&a).await;
    sqlx::raw_sql(
        "INSERT INTO playlist (id, name, color_id, icon_id, is_smart, smart_rules, position,
                               created_at, updated_at)
         VALUES (100, 'Manual', 'violet', 'music', 0, NULL, 0, 0, 0),
                (101, 'On Repeat', 'violet', 'music', 1, '{\"kind\":\"on_repeat\"}', 0, 0, 0)",
    )
    .execute(&lib.pool)
    .await
    .unwrap();
    // Both empty, and both stay empty although every track would match
    // an empty rule tree.
    assert!(lib.refresh().await.is_empty());
    assert!(lib.members(100).await.is_empty());
    assert!(lib.members(101).await.is_empty());

    let everything = lib
        .smart_playlist(CustomRules {
            tree: RuleNode::All { children: vec![] },
            sort: None,
            limit: None,
        })
        .await;
    assert_eq!(lib.members(everything).await, vec![a_id]);
}

/// A file read again although it has not changed (a deep rescan, a
/// backfill) counts as skipped, yet the fields it rewrites are fields a
/// rule reads: the playlist follows them too. Here a custom tag the
/// database lost comes back from the file.
#[tokio::test]
async fn a_field_rewritten_from_an_unchanged_file_moves_the_playlist() {
    let lib = Library::new().await;
    let path = lib.flac("a.flac", "Tagged");
    let mut tag = VorbisComments::default();
    tag.push("TITLE".to_string(), "Tagged".to_string());
    tag.push("RIPPER".to_string(), "EAC".to_string());
    tag.save_to_path(&path, WriteOptions::default())
        .expect("write comments");
    lib.scan().await;
    let id = lib.id(&path).await;
    let playlist = lib
        .smart_playlist(CustomRules {
            tree: RuleNode::Leaf {
                predicate: Predicate::TagPresent {
                    key: "Ripper".to_string(),
                },
            },
            sort: None,
            limit: None,
        })
        .await;
    assert_eq!(lib.members(playlist).await, vec![id]);

    sqlx::query("DELETE FROM track_tag WHERE track_id = ?")
        .bind(id)
        .execute(&lib.pool)
        .await
        .unwrap();
    assert_eq!(lib.refresh().await, vec![playlist]);
    assert!(lib.members(playlist).await.is_empty());

    lib.scan_with(true).await;
    assert_eq!(
        lib.members(playlist).await,
        vec![id],
        "the deep rescan wrote the tag back, and the playlist took the track back"
    );
}
