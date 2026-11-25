//! Whose cover a playlist has (#1173): a user playlist and a custom smart
//! playlist take the user's, and an auto-cover made of their first tracks
//! that follows them; a generated family keeps the one its generator
//! wrote. On the migrated schema with foreign keys on, with real image
//! files in a profile's artwork cache.

use super::*;
use crate::error::ShownKind;
use crate::smart_playlists::custom::{self, CustomRules, CustomSort, RuleNode};
use crate::smart_playlists::SmartPlaylistRules;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const PROFILE: i64 = 1;

struct Fixture {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
    paths: AppPaths,
}

impl Fixture {
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
        let paths = AppPaths::from_root(dir.path().join("app"), None);
        std::fs::create_dir_all(paths.profile_artwork_dir(PROFILE)).unwrap();
        std::fs::create_dir_all(&paths.metadata_artwork_dir).unwrap();
        Self {
            _dir: dir,
            pool,
            paths,
        }
    }

    /// A track added at `added_at`, with a cover of its own: a solid
    /// image of `shade`, stored under the profile's artwork cache.
    async fn track(&self, id: i64, added_at: i64, shade: u8) {
        let hash = format!("art{id}");
        let image = image::RgbImage::from_pixel(64, 64, image::Rgb([shade, 255 - shade, 90]));
        image
            .save(
                self.paths
                    .profile_artwork_dir(PROFILE)
                    .join(format!("{hash}.jpg")),
            )
            .unwrap();
        sqlx::query(
            "INSERT INTO artwork (id, hash, format, source, created_at)
             VALUES (?, ?, 'jpg', 'embedded', 0)",
        )
        .bind(id)
        .bind(&hash)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available, artwork_id,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (?, 1, ?, ?, 1, 0, ?, 1000, ?, 1, ?, 0, 0, 0, 0)",
        )
        .bind(id)
        .bind(format!("/m/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(format!("T{id}"))
        .bind(added_at)
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn playlist(&self, is_smart: i64, smart_rules: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO playlist
                 (name, color_id, icon_id, is_smart, smart_rules, position,
                  created_at, updated_at)
             VALUES ('P', 'violet', 'sparkles', ?, ?, 0, 0, 0)",
        )
        .bind(is_smart)
        .bind(smart_rules)
        .execute(&self.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Every available track, newest first: what a new track changes the
    /// head of.
    fn newest_first() -> CustomRules {
        CustomRules {
            tree: RuleNode::All { children: vec![] },
            sort: Some(CustomSort::AddedDesc),
            limit: None,
        }
    }

    /// A custom smart playlist made the way the editor makes one, its
    /// tracks materialized once.
    async fn custom_playlist(&self) -> i64 {
        let rules = Self::newest_first();
        let json = SmartPlaylistRules::Custom {
            rules: rules.clone(),
        }
        .to_json()
        .unwrap();
        let id = self.playlist(1, Some(&json)).await;
        custom::materialize(&self.pool, id, &rules).await.unwrap();
        id
    }

    /// A generated Daily Mix, its cover written by its generator.
    async fn daily_mix(&self) -> i64 {
        let json = r#"{"kind":"daily_mix","slot":1,"seed_artist_ids":[],"seed_genre_ids":[]}"#;
        let id = self.playlist(1, Some(json)).await;
        sqlx::query("UPDATE playlist SET cover_hash = 'generated' WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        id
    }

    async fn cover(&self, playlist_id: i64) -> (Option<String>, i64) {
        sqlx::query_as("SELECT cover_hash, cover_is_auto FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn regen(&self, playlist_id: i64) {
        maybe_regen_auto_cover(&self.pool, &self.paths, PROFILE, playlist_id).await;
    }
}

#[tokio::test]
async fn a_custom_smart_playlist_takes_a_cover_and_a_generated_one_refuses() {
    let fx = Fixture::new().await;
    let user = fx.playlist(0, None).await;
    let custom = fx.custom_playlist().await;
    let daily = fx.daily_mix().await;

    ensure_user_cover(&fx.pool, user)
        .await
        .expect("user playlist");
    ensure_user_cover(&fx.pool, custom)
        .await
        .expect("custom smart playlist");
    match ensure_user_cover(&fx.pool, daily).await {
        Err(AppError::Shown { kind, .. }) => assert_eq!(kind, ShownKind::GeneratedPlaylistCover),
        other => panic!("a Daily Mix must be refused in the user's language, got {other:?}"),
    }
    assert!(matches!(
        ensure_user_cover(&fx.pool, 999).await,
        Err(AppError::Other(_))
    ));
}

#[tokio::test]
async fn a_smart_row_whose_rules_are_not_custom_ones_is_refused() {
    let fx = Fixture::new().await;
    let unreadable = fx.playlist(1, Some("{not json")).await;
    let empty = fx.playlist(1, None).await;
    for id in [unreadable, empty] {
        assert!(matches!(
            ensure_user_cover(&fx.pool, id).await,
            Err(AppError::Shown {
                kind: ShownKind::GeneratedPlaylistCover,
                ..
            })
        ));
    }
}

#[tokio::test]
async fn a_custom_smart_playlist_gets_an_auto_cover_from_its_tracks() {
    let fx = Fixture::new().await;
    fx.track(1, 10, 20).await;
    fx.track(2, 20, 200).await;
    let custom = fx.custom_playlist().await;
    assert_eq!(fx.cover(custom).await, (None, 1), "born auto, no cover yet");

    fx.regen(custom).await;

    let (hash, auto) = fx.cover(custom).await;
    let hash = hash.expect("the auto-cover was written");
    assert_eq!(auto, 1);
    assert!(fx
        .paths
        .metadata_artwork_dir
        .join(format!("{hash}.jpg"))
        .exists());
}

#[tokio::test]
async fn neither_a_manual_cover_nor_a_generated_one_is_composed_over() {
    let fx = Fixture::new().await;
    fx.track(1, 10, 20).await;
    let custom = fx.custom_playlist().await;
    update_cover(&fx.pool, custom, Some("mine"), 0)
        .await
        .unwrap();
    let daily = fx.daily_mix().await;
    custom::materialize(&fx.pool, daily, &Fixture::newest_first())
        .await
        .unwrap();

    fx.regen(custom).await;
    fx.regen(daily).await;

    assert_eq!(fx.cover(custom).await, (Some("mine".into()), 0));
    assert_eq!(fx.cover(daily).await.0.as_deref(), Some("generated"));
}

#[tokio::test]
async fn the_auto_cover_follows_a_refresh_that_changed_the_first_tracks() {
    let fx = Fixture::new().await;
    fx.track(1, 10, 20).await;
    let custom = fx.custom_playlist().await;
    let user = fx.playlist(0, None).await;
    fx.regen(custom).await;
    let before = fx.cover(custom).await.0.expect("first cover");

    // A newer track: the rules put it first.
    fx.track(2, 20, 200).await;
    let mut changed = Vec::new();
    custom::refresh_all(&fx.pool, &mut changed).await.unwrap();
    assert_eq!(changed, vec![custom]);
    regen_auto_covers(&fx.pool, &fx.paths, PROFILE, &changed).await;

    let (after, auto) = fx.cover(custom).await;
    assert_ne!(after.as_deref(), Some(before.as_str()), "a new mosaic");
    assert_eq!(auto, 1);
    assert_eq!(fx.cover(user).await, (None, 1), "only what was written");
}

#[tokio::test]
async fn an_upload_that_lands_while_the_auto_cover_is_composed_is_kept() {
    let fx = Fixture::new().await;
    let custom = fx.custom_playlist().await;
    // The upload committed after the hook read `cover_is_auto = 1`.
    update_cover(&fx.pool, custom, Some("mine"), 0)
        .await
        .unwrap();

    assert!(!write_auto_cover(&fx.pool, custom, "mosaic", false)
        .await
        .unwrap());
    assert_eq!(fx.cover(custom).await, (Some("mine".into()), 0));

    // The explicit regenerate command replaces it, as it always did.
    assert!(write_auto_cover(&fx.pool, custom, "mosaic", true)
        .await
        .unwrap());
    assert_eq!(fx.cover(custom).await, (Some("mosaic".into()), 1));
}
