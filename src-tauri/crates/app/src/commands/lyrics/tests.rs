use lofty::tag::ItemKey;
use waveflow_core::plugin::runtime::{
    AssociatedKind as PluginAssociatedKind, LyricsDocFormat as PluginLyricsFormat,
};
use waveflow_syncedlyrics::Provider;

use super::*;
use crate::commands::lyrics_providers;

/// A document is accepted only if it looks like what it says it is.
///
/// This is the whole of the host's trust in a plugin's lyrics, so
/// both directions matter: a correct document must not be refused,
/// and a mislabelled one must not get through.
#[test]
fn a_document_must_look_like_the_format_it_declares() {
    let lrc = "[00:12.00]Hello\n[00:15.00]World";
    let enhanced = "[00:12.00]<00:12.00>Hello <00:13.50>world";
    let ttml = "<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p>Hi</p></div></body></tt>";

    assert!(document_is_what_it_claims(lrc, LyricsFormat::Lrc));
    assert!(document_is_what_it_claims(
        enhanced,
        LyricsFormat::EnhancedLrc
    ));
    assert!(document_is_what_it_claims(ttml, LyricsFormat::Ttml));
    assert!(document_is_what_it_claims(
        "just words",
        LyricsFormat::Plain
    ));

    // The mislabelling that matters: a provider that served an error
    // page, or changed format without saying.
    assert!(!document_is_what_it_claims(ttml, LyricsFormat::Lrc));
    assert!(!document_is_what_it_claims(
        "just words",
        LyricsFormat::Ttml
    ));
    assert!(!document_is_what_it_claims(lrc, LyricsFormat::EnhancedLrc));

    // Empty is never a document, whatever it claims.
    assert!(!document_is_what_it_claims("", LyricsFormat::Plain));
    assert!(!document_is_what_it_claims("   \n  ", LyricsFormat::Lrc));
}

/// A profile pool with the real `app` schema attached, the way the
/// application attaches it.
///
/// The point of the tests below is the foreign key and its cascade,
/// so fixtures that recreate the tables by hand would prove nothing
/// — they would omit the very constraint under test. Hence the real
/// migrations, and two details copied from `db::profile_db::open`
/// rather than invented here:
///
/// `ATTACH` is per-CONNECTION, so it belongs in `after_connect`. Run
/// once through the pool it lands on whichever connection served it
/// and the next query gets `no such table: app.lyrics` — which is
/// exactly how the first version of this harness failed.
///
/// Both databases are files. A `sqlite::memory:` pool gives each
/// connection its own empty database, so the profile schema would
/// have been just as absent, one connection later.
///
/// `foreign_keys` is set explicitly: it is per-connection and off by
/// default, and without it every assertion below passes for the
/// wrong reason.
async fn pool_with_app_schema(dir: &std::path::Path) -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let opts_for = |path: &std::path::Path| {
        SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true)
    };

    let app_path = dir.join("app.db");
    let app_pool = sqlx::SqlitePool::connect_with(opts_for(&app_path))
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&app_pool)
        .await
        .unwrap();
    app_pool.close().await;

    let attach = format!(
        "ATTACH DATABASE '{}' AS app",
        app_path.display().to_string().replace('\'', "''")
    );
    let pool = SqlitePoolOptions::new()
        .after_connect(move |conn, _meta| {
            let attach = attach.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(attach))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(opts_for(&dir.join("profile.db")))
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

async fn seed_bundle(pool: &sqlx::SqlitePool, file_hash: &str) {
    upsert_lyrics(
        pool,
        file_hash,
        "[00:12.00]Original",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("apple-lyrics"),
    )
    .await
    .unwrap();
    for (kind, lang) in [("translation", Some("fr")), ("pronunciation", None)] {
        sqlx::query(
            "INSERT INTO app.lyrics_associated
                    (file_hash, kind, language, content, format, fetched_at)
                 VALUES (?, ?, ?, '[00:12.00]Companion', 'lrc', 0)",
        )
        .bind(file_hash)
        .bind(kind)
        .bind(lang)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// One library, one artist, one album, one track pointing at `path`.
///
/// Built from the real migrations by `pool_with_app_schema`, so the
/// NOT NULL columns and the foreign keys are the ones the app runs
/// with — a looser fixture would let a broken insert pass.
async fn seed_track(pool: &sqlx::SqlitePool, path: &std::path::Path, file_hash: &str) {
    sqlx::query(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, ?, ?, 1, 0, 'T', 300000, 0, 1, 0, 0, 0, 0)",
    )
    .bind(path.display().to_string())
    .bind(file_hash)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_destination(pool: &sqlx::SqlitePool, value: &str) {
    // `value_type` and `updated_at` are NOT NULL, and `value_type`
    // carries a CHECK -- the same shape `set_lyrics_default_destination`
    // writes. A fixture that omitted them would fail only at runtime.
    sqlx::query(
        "INSERT INTO app.app_setting (key, value, value_type, updated_at)
             VALUES (?, ?, 'string', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(LYRICS_DEFAULT_DESTINATION_KEY)
    .bind(value)
    .execute(pool)
    .await
    .unwrap();
}

/// The ISRC a v3 plugin receives comes from the scanner's `track_tag`
/// rows: trimmed, found whatever case the key was stored in, and
/// absent rather than empty when the file has none.
#[tokio::test]
async fn the_isrc_comes_from_the_track_tags() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    seed_track(&pool, &audio, "h1").await;
    let track_id: i64 = sqlx::query_scalar("SELECT id FROM track WHERE file_hash = 'h1'")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(track_isrc(&pool, track_id).await, None);

    sqlx::query("INSERT INTO track_tag (track_id, key, value) VALUES (?, 'isrc', '  ')")
        .bind(track_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(track_isrc(&pool, track_id).await, None);

    sqlx::query("UPDATE track_tag SET value = ' USUM72409273 ' WHERE track_id = ?")
        .bind(track_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        track_isrc(&pool, track_id).await.as_deref(),
        Some("USUM72409273")
    );
}

/// Lyrics fetched from a provider land next to the music when that is
/// where the user said lyrics go (#695). Before this, the destination
/// governed only what the user typed, so a library set to "sidecar
/// file" still kept everything fetched inside WaveFlow's database.
#[tokio::test]
async fn fetched_lyrics_land_next_to_the_music() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "[00:12.00]Hello",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    let sidecar = dir.path().join("Song.lrc");
    assert_eq!(
        std::fs::read_to_string(&sidecar).unwrap(),
        "[00:12.00]Hello"
    );
}

/// Invented TTML: two lines, the first timed word by word.
const WORD_TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="2s"><span begin="1s" end="1.5s">la</span> <span begin="1.5s" end="2s">lu</span></p><p begin="2s" end="3s">li lo</p></div></body></tt>"#;
/// Invented TTML timed by line only.
const LINE_TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="2s">la lu</p><p begin="2s" end="3s">li lo</p></div></body></tt>"#;
/// Invented TTML with no timing at all, as Apple serves unsynced lyrics.
const UNTIMED_TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="None"><body><div><p>la lu</p><p>li lo</p></div></body></tt>"#;

/// A fetch that comes back as TTML lands as `{stem}.ttml`: it used to
/// be skipped, because the reader took neither extension it could ride.
#[tokio::test]
async fn fetched_ttml_lands_next_to_the_music_as_ttml() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        WORD_TTML,
        &LyricsFormat::Ttml,
        &LyricsSource::Api,
        Some("plugin"),
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.path().join("Song.ttml")).unwrap(),
        WORD_TTML
    );
    assert!(!dir.path().join("Song.lrc").exists());
    // And the reader takes it back.
    assert_eq!(read_sidecar_lyrics(&audio).as_deref(), Some(WORD_TTML));
}

/// The export never writes over a sidecar of another extension: a
/// `.lrc` the user keeps beside the track already answers for it.
#[tokio::test]
async fn fetched_ttml_does_not_join_an_existing_lrc() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    std::fs::write(dir.path().join("song.LRC"), "[00:01.00]la").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        WORD_TTML,
        &LyricsFormat::Ttml,
        &LyricsSource::Api,
        Some("plugin"),
    )
    .await
    .unwrap();

    assert!(!dir.path().join("Song.ttml").exists());
}

/// The editor's sidecar write takes TTML too, to `{stem}.ttml`.
#[test]
fn the_editor_writes_ttml_to_a_ttml_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    write_lyrics_sidecar(&audio, LINE_TTML, &LyricsFormat::Ttml).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Song.ttml")).unwrap(),
        LINE_TTML
    );
    write_lyrics_sidecar(&audio, "la lu", &LyricsFormat::Plain).unwrap();
    write_lyrics_sidecar(&audio, "[00:01.00]la", &LyricsFormat::Lrc).unwrap();
    assert!(dir.path().join("Song.txt").exists());
    assert!(dir.path().join("Song.lrc").exists());
}

/// A `.ttml` added after a miss was cached is found the way a `.lrc`
/// is: the folder's watcher rescans, and the next read checks the miss
/// against the sidecar.
#[tokio::test]
async fn a_cached_miss_gives_way_to_a_ttml_added_since() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    cache_lyrics_miss(&pool, 1, &meta_for("h1"), false)
        .await
        .unwrap();

    std::fs::write(dir.path().join("Song.ttml"), LINE_TTML).unwrap();
    let found = read_cached_or_new_sidecar(&pool, 1)
        .await
        .unwrap()
        .expect("the sidecar is found");
    assert_eq!(found.format, LyricsFormat::Ttml);
    assert_eq!(found.content, LINE_TTML);
    let cached = read_cached(&pool, 1).await.unwrap().expect("now cached");
    assert!(matches!(cached.source, LyricsSource::LrcFile));
    assert_eq!(cached.format, LyricsFormat::Ttml);
}

/// The refusal that matters most: `tag` must NOT make a background
/// fetch rewrite the audio file. That write pauses playback and
/// re-hashes, which is the tag editor's job, on the user's command --
/// never a side effect of a track starting.
#[tokio::test]
async fn a_fetch_never_rewrites_the_audio_file() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "tag").await;

    upsert_lyrics(
        &pool,
        "h1",
        "[00:12.00]Hello",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    assert!(!dir.path().join("Song.lrc").exists());
    assert_eq!(std::fs::read(&audio).unwrap(), b"audio");
}

/// An empty sidecar is a file nobody else wrote, even though the
/// reader reports it as a miss. Truncating it would be the overwrite
/// this code refuses to do.
#[tokio::test]
async fn an_empty_sidecar_is_still_someone_else_s_file() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    let sidecar = dir.path().join("Song.lrc");
    std::fs::write(&sidecar, "").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "[00:12.00]Theirs",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "");
}

/// A sidecar the user spelled differently is still the user's file.
/// The guard asks the waterfall's own reader, which matches the stem
/// case-insensitively, so this only has something to prove on a
/// case-sensitive filesystem -- the Linux CI runner, where writing
/// `Song.lrc` beside `song.LRC` would leave two files and let ours
/// supersede theirs.
#[tokio::test]
async fn a_sidecar_spelled_differently_still_counts() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    std::fs::write(dir.path().join("song.LRC"), "[00:01.00]Mine").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "[00:12.00]Theirs",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    // Listed, not `exists()`: on a case-insensitive filesystem (APFS,
    // NTFS) `Song.lrc` resolves to the user's `song.LRC`.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|name| name == "Song.lrc"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("song.LRC")).unwrap(),
        "[00:01.00]Mine"
    );
}

/// A sidecar already on disk is the user's file. The export adds one
/// where there was none; it never edits one that is already there.
#[tokio::test]
async fn an_existing_sidecar_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    let sidecar = dir.path().join("Song.lrc");
    std::fs::write(&sidecar, "[00:01.00]Mine").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "[00:12.00]Theirs",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "[00:01.00]Mine");
}

/// Lyrics read out of the file itself have nowhere to go: writing a
/// sidecar from an embedded tag would fill the library with copies of
/// what is already in the music.
#[tokio::test]
async fn lyrics_that_came_off_the_disk_are_not_written_back() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "Hello",
        &LyricsFormat::Plain,
        &LyricsSource::Embedded,
        None,
    )
    .await
    .unwrap();

    assert!(!dir.path().join("Song.txt").exists());
    assert!(!dir.path().join("Song.lrc").exists());
}

/// An instrumental verdict is an empty row, not lyrics: it must not
/// leave an empty file beside the track.
#[tokio::test]
async fn an_instrumental_verdict_writes_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    set_destination(&pool, "sidecar").await;

    upsert_lyrics(
        &pool,
        "h1",
        "",
        &LyricsFormat::Plain,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    assert!(!dir.path().join("Song.txt").exists());
}

async fn retry_after_of(pool: &sqlx::SqlitePool, file_hash: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT retry_after FROM app.lyrics WHERE file_hash = ?")
        .bind(file_hash)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn meta_for(file_hash: &str) -> TrackMeta {
    TrackMeta {
        file_path: String::new(),
        file_hash: file_hash.to_string(),
        title: "T".into(),
        artist_name: None,
        album_title: None,
        duration_ms: 300_000,
    }
}

/// #720: a miss where a provider was not heard is cached, so the next
/// open does not replay the chain — but only until its date, after
/// which the track reads as uncached and is looked up again.
#[tokio::test]
async fn a_partial_miss_is_served_until_its_date_then_looked_up_again() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_track(&pool, &dir.path().join("Song.flac"), "h1").await;

    cache_lyrics_miss(&pool, 1, &meta_for("h1"), true)
        .await
        .unwrap();
    let due = retry_after_of(&pool, "h1")
        .await
        .expect("a partial miss expires");
    assert!(due > now_ms());
    let served = read_cached(&pool, 1)
        .await
        .unwrap()
        .expect("served as a miss");
    assert!(served.content.is_empty());

    sqlx::query("UPDATE app.lyrics SET retry_after = ? WHERE file_hash = 'h1'")
        .bind(now_ms() - 1)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        read_cached(&pool, 1).await.unwrap().is_none(),
        "a partial miss past its date must read as not cached"
    );
}

/// A complete miss keeps the behaviour it always had: cached for good.
#[tokio::test]
async fn a_complete_miss_never_expires() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_track(&pool, &dir.path().join("Song.flac"), "h1").await;

    cache_lyrics_miss(&pool, 1, &meta_for("h1"), false)
        .await
        .unwrap();
    assert_eq!(retry_after_of(&pool, "h1").await, None);
    assert!(read_cached(&pool, 1).await.unwrap().is_some());
}

/// #910: a GB18030 sidecar was unreadable, so the track got a miss
/// cached for good. Once the sidecar can be read, the miss must give
/// way to it, or the fix never reaches a track already opened.
#[tokio::test]
async fn a_cached_miss_gives_way_to_a_sidecar_readable_since() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"audio").unwrap();
    seed_track(&pool, &audio, "h1").await;
    cache_lyrics_miss(&pool, 1, &meta_for("h1"), false)
        .await
        .unwrap();

    let still_a_miss = read_cached_or_new_sidecar(&pool, 1)
        .await
        .unwrap()
        .expect("the miss is served while there is no sidecar");
    assert!(still_a_miss.content.is_empty());

    let lyrics = "[00:01.00]今天的天气很好，我们一起去公园散步吧";
    let (bytes, _, _) = encoding_rs::GB18030.encode(lyrics);
    std::fs::write(dir.path().join("Song.lrc"), &bytes).unwrap();

    let found = read_cached_or_new_sidecar(&pool, 1)
        .await
        .unwrap()
        .expect("the sidecar is found");
    assert_eq!(found.content, lyrics);
    let cached = read_cached(&pool, 1).await.unwrap().expect("now cached");
    assert_eq!(cached.content, lyrics);
    assert!(matches!(cached.source, LyricsSource::LrcFile));
}

/// The sidecar replaces a miss, never what was written over the miss
/// while the sidecar was being read.
#[tokio::test]
async fn a_sidecar_does_not_replace_lyrics_written_since_the_miss() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_track(&pool, &dir.path().join("Song.flac"), "h1").await;
    cache_lyrics_miss(&pool, 1, &meta_for("h1"), false)
        .await
        .unwrap();
    upsert_lyrics(
        &pool,
        "h1",
        "[00:01.00]Found",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();

    let sidecar = sidecar_payload(1, "[00:01.00]Sidecar".into());
    assert!(!replace_miss(&pool, "h1", &sidecar).await.unwrap());
    let cached = read_cached(&pool, 1).await.unwrap().expect("cached");
    assert_eq!(cached.content, "[00:01.00]Found");
}

/// Lyrics found after a partial miss replace it for good. Were the
/// expiry carried over, they would be thrown away a week later.
#[tokio::test]
async fn lyrics_that_replace_a_partial_miss_do_not_inherit_its_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_track(&pool, &dir.path().join("Song.flac"), "h1").await;

    cache_lyrics_miss(&pool, 1, &meta_for("h1"), true)
        .await
        .unwrap();
    upsert_lyrics(
        &pool,
        "h1",
        "[00:01.00]Found",
        &LyricsFormat::Lrc,
        &LyricsSource::Api,
        Some("lrclib"),
    )
    .await
    .unwrap();
    assert_eq!(retry_after_of(&pool, "h1").await, None);
}

async fn associated_count(pool: &sqlx::SqlitePool, file_hash: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM app.lyrics_associated WHERE file_hash = ?")
        .bind(file_hash)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Writing a new primary document ends the bundle the old one
/// belonged to.
///
/// Without this, switching provider leaves the previous one's
/// translation under the new lyrics — two documents that were never
/// published together, with nothing downstream able to tell. The
/// invariant lives in `upsert_lyrics` precisely so every tier gets
/// it; this test is what stops someone adding a tier that writes
/// the row directly.
#[tokio::test]
async fn replacing_the_primary_document_clears_its_companions() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_bundle(&pool, "hash-a").await;
    assert_eq!(associated_count(&pool, "hash-a").await, 2);

    // Any other tier answering for the same file — here a manual save.
    upsert_lyrics(
        &pool,
        "hash-a",
        "Typed by hand",
        &LyricsFormat::Plain,
        &LyricsSource::Manual,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        associated_count(&pool, "hash-a").await,
        0,
        "a new primary document must not inherit the previous bundle's companions"
    );
}

/// Deleting the lyrics row takes its companions with it.
///
/// `read_cached` deletes a cached service credit in place, and the
/// bundle has to follow. That is the foreign key's job rather than
/// the caller's — which only holds while `foreign_keys` is on, so
/// this asserts the behaviour rather than the schema text.
#[tokio::test]
async fn companions_cascade_when_the_lyrics_row_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_bundle(&pool, "hash-b").await;

    sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
        .bind("hash-b")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        associated_count(&pool, "hash-b").await,
        0,
        "companions must not outlive the lyrics they belong to"
    );
}

fn plugin_doc(
    content: &str,
    language: Option<&str>,
) -> waveflow_core::plugin::runtime::LyricsDocument {
    waveflow_core::plugin::runtime::LyricsDocument {
        content: content.to_string(),
        format: PluginLyricsFormat::Lrc,
        language: language.map(str::to_string),
    }
}

/// What `cache_lyrics_bundle` returns must be what it stored.
///
/// The insert is `OR IGNORE`, so a provider sending two documents for
/// one slot has the second dropped by the database whatever the host
/// does. If the payload were built from the unfiltered list, the
/// panel would show both until the next reload and one after it, and
/// nothing would explain the difference. This pins the two together.
#[tokio::test]
async fn a_returned_bundle_matches_what_was_cached() {
    use waveflow_core::plugin::runtime::{AssociatedDocument, LyricsBundle};

    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;

    let bundle = LyricsBundle {
        primary: plugin_doc("[00:12.00]Original", Some("ja")),
        associated: vec![
            AssociatedDocument {
                kind: PluginAssociatedKind::Translation,
                document: plugin_doc("[00:12.00]Premiere", Some("fr")),
            },
            // Same slot as the one above — the provider contradicting
            // itself, which is the case the two rules have to agree on.
            AssociatedDocument {
                kind: PluginAssociatedKind::Translation,
                document: plugin_doc("[00:12.00]Seconde", Some("fr")),
            },
            AssociatedDocument {
                kind: PluginAssociatedKind::Pronunciation,
                document: plugin_doc("[00:12.00]Romaji", None),
            },
        ],
    };

    let payload = cache_lyrics_bundle(&pool, 1, "hash-d", bundle, "apple-lyrics")
        .await
        .unwrap()
        .expect("the primary document is valid LRC");

    assert_eq!(
        payload.associated.len(),
        2,
        "the duplicate slot must be dropped before the payload is built"
    );
    assert_eq!(
        payload.associated.len() as i64,
        associated_count(&pool, "hash-d").await,
        "the payload and the cache must agree on what was stored"
    );
    assert_eq!(
        payload.associated[0].content, "[00:12.00]Premiere",
        "the first document for a slot is the one kept"
    );

    // Provenance is namespaced so `Provider::from_id` can never take a
    // plugin id for one of its own.
    assert_eq!(payload.provider.as_deref(), Some("plugin:apple-lyrics"));
}

/// One document per (kind, language) — and NULL is one slot, not a
/// fresh one each time.
///
/// SQLite treats NULLs as distinct in a UNIQUE index, so the index
/// is written over `COALESCE(language, '')`. Without that, a
/// provider returning two untagged pronunciations would store both
/// and the panel would have no way to choose.
#[tokio::test]
async fn a_bundle_holds_one_document_per_kind_and_language() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool_with_app_schema(dir.path()).await;
    seed_bundle(&pool, "hash-c").await;

    let second_untagged = sqlx::query(
        "INSERT INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, 'pronunciation', NULL, 'Another', 'plain', 0)",
    )
    .bind("hash-c")
    .execute(&pool)
    .await;
    assert!(
        second_untagged.is_err(),
        "a second untagged pronunciation must collide with the first"
    );

    // A different language is a different slot, and allowed.
    sqlx::query(
        "INSERT INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, 'translation', 'ja', '[00:12.00]Companion', 'lrc', 0)",
    )
    .bind("hash-c")
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(associated_count(&pool, "hash-c").await, 3);
}

/// The chain must consult LRCLIB before providers that answer for
/// almost any query. Tier 5 already asked LRCLIB, but through the
/// exact-match `/api/get`; this pass is the fuzzy one, and dropping
/// it is what let Genius answer for tracks LRCLIB actually carries
/// (issue #463).
#[test]
fn fallback_chain_asks_lrclib_before_the_catch_all_providers() {
    let chain = lyrics_providers::CHAIN;
    let pos = |p: Provider| chain.iter().position(|c| *c == p);

    let lrclib = pos(Provider::Lrclib).expect("lrclib is in the chain");
    for catch_all in [Provider::NetEase, Provider::Megalobiz, Provider::Genius] {
        let other = pos(catch_all).expect("provider is in the chain");
        assert!(
            lrclib < other,
            "lrclib must precede {catch_all:?} in the fallback chain"
        );
    }

    // Musixmatch stays out: the dedicated enhanced tier owns it, and
    // listing it here would re-issue the identical request.
    assert!(
        pos(Provider::Musixmatch).is_none(),
        "musixmatch belongs to its own tier, not this chain"
    );
}

/// The cached half of the same bug. The waterfall never refetches
/// once a row exists, so correcting the reader alone would have
/// left every already-affected track showing the credit forever —
/// including the reporter's.
///
/// The recogniser is the same one the reader uses, which is what
/// keeps "what we refuse to store" and "what we refuse to serve"
/// from drifting apart.
#[test]
fn a_cached_blurb_is_recognised_by_the_same_test_that_refuses_it() {
    let blurb = "Provided to YouTube by Some Label\n\nSong · Artist\n\nAlbum";
    assert!(is_service_blurb(blurb), "refused on the way in");
    // …and therefore recognised on the way out, by definition.
    // The deletion itself needs a database and is covered by the
    // app-crate suite; what matters here is that one predicate
    // governs both directions.
    assert!(!is_service_blurb("Real lyrics\nsecond line"));
}

/// Reported on discussion #519: a `.m4a` pulled with `yt-dlp`
/// carries the auto-generated YouTube credit in `description`,
/// which is several lines long and was therefore read as lyrics —
/// ahead of the `.lrc` the user had placed next to the file.
///
/// Two things had to be wrong at once: a description field counted
/// as a lyrics source, and it was consulted before the sidecar.
/// The blurb is now refused outright, and the tier that remains
/// sits behind the sidecar.
#[test]
fn a_youtube_credit_is_not_lyrics() {
    let blurb = "Provided to YouTube by RCA Records Label\n\n\
         Sorry · Nothing But Thieves\n\n\
         Broken Machine (Deluxe)\n\n\
         ℗ 2017 Sony Music Entertainment UK Limited\n\n\
         Released on: 2017-09-08";
    assert!(is_service_blurb(blurb));
    assert!(is_service_blurb(
        "Auto-generated by YouTube.\nfoo\nbar\nbaz"
    ));

    // And the check stays narrow: anything it does not recognise
    // goes through, because a false positive here costs someone
    // their real lyrics.
    assert!(!is_service_blurb(
        "I found a love for me\nDarling just dive right in\nAnd follow my lead"
    ));
    assert!(!is_service_blurb(""));

    // A song that happens to say the words much later is not
    // boilerplate: the markers are looked for near the top, where
    // the credit always sits.
    let long_song = "la la la\n".repeat(200) + "provided to youtube by nobody";
    assert!(!is_service_blurb(&long_song));
}

#[test]
fn embedded_lyrics_resolution_prioritizes_synced_and_skips_blanks() {
    let s = |x: &str| Some(x.to_string());

    // Synced wins over an unsynced standard tag (the Antra case, #378).
    assert_eq!(
        resolve_embedded_lyrics([s("[00:01.00]timed"), s("plain body"), None]),
        Some("[00:01.00]timed".to_string())
    );
    // A whitespace-only standard tag is treated as absent, so a valid
    // custom TXXX/Vorbis value is selected instead of returning nothing
    // (the whitespace-masks-fallback bug from the CR).
    assert_eq!(
        resolve_embedded_lyrics([None, s("   \n\t  "), s("from txxx")]),
        Some("from txxx".to_string())
    );
    // Unsynced-only falls back to the standard key.
    assert_eq!(
        resolve_embedded_lyrics([None, s("just words"), None]),
        Some("just words".to_string())
    );
    // Every candidate blank/absent → None.
    assert_eq!(resolve_embedded_lyrics([None, s("  "), s("\t")]), None);
    // The synced body is detected as timed LRC downstream.
    assert_eq!(detect_format("[00:01.00]timed"), LyricsFormat::Lrc);
}

#[test]
fn known_key_lyrics_falls_through_blank_uslt_to_lyrics() {
    use lofty::tag::{Tag, TagType};

    // A whitespace-only USLT must not mask a valid LYRICS on the same
    // tag (the deeper case of the blank-tag bug).
    let mut tag = Tag::new(TagType::VorbisComments);
    tag.insert_text(ItemKey::UnsyncLyrics, "   \n\t ".to_string());
    tag.insert_text(ItemKey::Lyrics, "real body".to_string());
    assert_eq!(known_key_lyrics(&tag), Some("real body".to_string()));

    // USLT present and non-blank wins over LYRICS.
    let mut primary = Tag::new(TagType::VorbisComments);
    primary.insert_text(ItemKey::UnsyncLyrics, "from uslt".to_string());
    primary.insert_text(ItemKey::Lyrics, "from lyrics".to_string());
    assert_eq!(known_key_lyrics(&primary), Some("from uslt".to_string()));

    // Both blank → None.
    let mut blank = Tag::new(TagType::VorbisComments);
    blank.insert_text(ItemKey::UnsyncLyrics, "  ".to_string());
    blank.insert_text(ItemKey::Lyrics, "\t".to_string());
    assert_eq!(known_key_lyrics(&blank), None);
}

#[test]
fn synced_keys_match_vorbis_comments_case_insensitively() {
    use lofty::ogg::tag::VorbisComments;

    // Validates the reader mechanism: our key constants resolve a real
    // VorbisComments case-insensitively, and a plain-only file yields
    // nothing for the synced keys but hits the unsynced set.
    let mut synced = VorbisComments::default();
    synced.push("SyncedLyrics".to_string(), "[00:01.00]timed".to_string());
    assert_eq!(
        SYNCED_LYRICS_KEYS.iter().find_map(|k| synced.get(k)),
        Some("[00:01.00]timed")
    );

    let mut plain = VorbisComments::default();
    plain.push("UNSYNCEDLYRICS".to_string(), "just words".to_string());
    assert!(SYNCED_LYRICS_KEYS
        .iter()
        .find_map(|k| plain.get(k))
        .is_none());
    assert_eq!(
        UNSYNCED_LYRICS_KEYS.iter().find_map(|k| plain.get(k)),
        Some("just words")
    );
}

#[test]
fn detect_format_plain() {
    let sample = "This is just\nsome text without any timestamps.";
    assert_eq!(detect_format(sample), LyricsFormat::Plain);
}

#[test]
fn detect_format_lrc() {
    let sample = "[ar:Some Artist]\n[ti:Some Title]\n[00:01.00]First line\n[00:05.50]Second line";
    assert_eq!(detect_format(sample), LyricsFormat::Lrc);
}

#[test]
fn detect_format_enhanced_lrc() {
    let sample =
        "[00:01.00]<00:01.00>Hello <00:01.50>world\n[00:03.00]<00:03.00>Another <00:03.40>line";
    assert_eq!(detect_format(sample), LyricsFormat::EnhancedLrc);
}

#[test]
fn detect_format_enhanced_lrc_no_colon_frac() {
    let sample = "[00:01.00]<00:01>plain stamps still count";
    assert_eq!(detect_format(sample), LyricsFormat::EnhancedLrc);
}

#[test]
fn detect_format_ttml_xml_decl() {
    let sample = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body>
    <div>
      <p begin="00:00:01.000" end="00:00:03.000">
        <span begin="00:00:01.000" end="00:00:01.500">Hello</span>
        <span begin="00:00:01.500" end="00:00:03.000">world</span>
      </p>
    </div>
  </body>
</tt>"#;
    assert_eq!(detect_format(sample), LyricsFormat::Ttml);
}

#[test]
fn detect_format_ttml_no_decl() {
    let sample =
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="0s">x</p></div></body></tt>"#;
    assert_eq!(detect_format(sample), LyricsFormat::Ttml);
}

// #668: whether an answer can be followed decides if it ends the
// waterfall, so these mirror the renderer's own rules. The untimed
// shape is Apple's; the text is placeholder.
#[test]
fn untimed_apple_ttml_is_not_synced() {
    let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="None" xml:lang="en"><head><metadata/></head><body><div><p>line one</p><p>line two</p></div></body></tt>"#;
    assert!(!lyrics_are_synced(&LyricsFormat::Ttml, sample));
}
#[test]
fn ttml_with_a_timed_line_is_synced() {
    let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="00:01.000" end="00:02.000">line</p></div></body></tt>"#;
    assert!(lyrics_are_synced(&LyricsFormat::Ttml, sample));
}
#[test]
fn ttml_timed_only_on_spans_is_not_synced() {
    let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p><span begin="1s">word</span></p></div></body></tt>"#;
    assert!(!lyrics_are_synced(&LyricsFormat::Ttml, sample));
}
#[test]
fn prefixed_ttml_line_with_begin_is_synced() {
    let sample = r#"<tt:tt xmlns:tt="http://www.w3.org/ns/ttml"><tt:body><tt:div><tt:p begin="1s">line</tt:p></tt:div></tt:body></tt:tt>"#;
    assert!(lyrics_are_synced(&LyricsFormat::Ttml, sample));
}
#[test]
fn unparseable_ttml_is_not_synced() {
    assert!(!lyrics_are_synced(
        &LyricsFormat::Ttml,
        "<tt><p begin=\"1s\">"
    ));
}
#[test]
fn ttml_begin_the_renderer_cannot_read_is_untimed() {
    for begin in ["", "   ", "soon", "1:2:3:4", "-5s"] {
        let sample = format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}">line</p></div></body></tt>"#
        );
        assert!(
            !lyrics_are_synced(&LyricsFormat::Ttml, &sample),
            "{begin:?}"
        );
    }
    for begin in ["12.5s", "1500ms", "01:02.5", "01:02:03.4", "5"] {
        let sample = format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}">line</p></div></body></tt>"#
        );
        assert!(lyrics_are_synced(&LyricsFormat::Ttml, &sample), "{begin:?}");
    }
}
#[test]
fn ttml_with_a_malformed_attribute_is_not_synced() {
    // `DOMParser` rejects the document, so the renderer shows nothing
    // timed however valid the rest of it is.
    let duplicate = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" begin="2s">line</p></div></body></tt>"#;
    assert!(!lyrics_are_synced(&LyricsFormat::Ttml, duplicate));
    let elsewhere = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div lang="en" lang="fr"><p begin="1s">line</p></div></body></tt>"#;
    assert!(!lyrics_are_synced(&LyricsFormat::Ttml, elsewhere));
}

#[test]
fn a_ttml_line_held_by_cdata_or_a_reference_is_a_line() {
    let wrap = |p: &str| {
        format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">{p}</p></div></body></tt>"#
        )
    };
    assert!(ttml_has_a_line(&wrap("<![CDATA[line]]>")));
    assert!(!ttml_has_a_line(&wrap("<![CDATA[  ]]>")));
    assert!(ttml_has_a_line(&wrap("&amp;")));
    assert!(ttml_has_a_line(&wrap("&#x4E2D;")));
}

#[test]
fn a_ttml_document_with_an_undefined_entity_shows_no_line() {
    // `DOMParser` rejects it whole, even outside the lines.
    let in_line = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">a&nbsp;b</p></div></body></tt>"#;
    assert!(!ttml_has_a_line(in_line));
    let elsewhere = r#"<tt xmlns="http://www.w3.org/ns/ttml"><head><title>&nbsp;</title></head><body><div><p begin="1s">line</p></div></body></tt>"#;
    assert!(!ttml_has_a_line(elsewhere));
    let bad_number = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">&#xZZ;</p></div></body></tt>"#;
    assert!(!ttml_has_a_line(bad_number));
    let nul = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">a&#0;</p></div></body></tt>"#;
    assert!(!ttml_has_a_line(nul));
    let control = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">a&#1;</p></div></body></tt>"#;
    assert!(!ttml_has_a_line(control));
    let tab = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s">a&#9;b</p></div></body></tt>"#;
    assert!(ttml_has_a_line(tab));
}

#[test]
fn lrc_with_every_line_on_one_time_is_untimed() {
    let all_zero = "[00:00.00]first\n[00:00.00]second\n[00:00.00]third";
    assert!(!lyrics_are_synced(&LyricsFormat::Lrc, all_zero));
    // A title card at 0 and real timing after it stays synced.
    let mostly_timed =
        "[00:00.00]Title\n[00:00.00]Artist\n[00:12.40]one\n[00:15.10]two\n[00:18.00]three";
    assert!(lyrics_are_synced(&LyricsFormat::Lrc, mostly_timed));
    // More than half on one time is untimed, whichever time it is.
    let piled = "[00:30.00]a\n[00:30.00]b\n[00:30.00]c\n[00:31.00]d";
    assert!(!lyrics_are_synced(&LyricsFormat::Lrc, piled));
    // The same instant written with different precision is one time.
    let same_instant = "[00:01.5]a\n[00:01.50]b\n[00:01.500]c";
    assert!(!lyrics_are_synced(&LyricsFormat::Lrc, same_instant));
    // Word stamps carry timing even when every line sits on 0.
    let words = "[00:00.00]<00:01.00>a <00:01.50>b\n[00:00.00]<00:03.00>c <00:03.40>d";
    assert!(lyrics_are_synced(&LyricsFormat::EnhancedLrc, words));
    // A stamp closing a line times no word: words piled on one time
    // stay untimed whatever their end markers say.
    let closers = "[00:00.00]<00:00.00>a <00:00.00>b <00:05.00>\n[00:00.00]<00:00.00>c <00:00.00>d <00:06.00>";
    assert!(!lyrics_are_synced(&LyricsFormat::EnhancedLrc, closers));
    // …while real word timing with the same end markers stays synced.
    let timed_with_closers = "[00:00.00]<00:01.00>a <00:01.50>b <00:02.00>\n[00:00.00]<00:03.00>c <00:03.40>d <00:04.00>";
    assert!(lyrics_are_synced(
        &LyricsFormat::EnhancedLrc,
        timed_with_closers
    ));
    // Text before the first word stamp is sung at the line's time, as
    // the frontend reads it: lines on 0 with words at 5 s are timed.
    let prefixed = "[00:00.00]a <00:05.00>b\n[00:00.00]c <00:05.00>d";
    assert!(lyrics_are_synced(&LyricsFormat::EnhancedLrc, prefixed));
    // Exactly half is not more than half.
    assert!(lyrics_are_synced(
        &LyricsFormat::Lrc,
        "[00:01.00]a\n[00:01.00]b\n[00:02.00]c\n[00:03.00]d"
    ));
}

#[test]
fn lrc_needs_a_complete_line_stamp() {
    assert!(lyrics_are_synced(&LyricsFormat::Lrc, "[00:01.00]x"));
    assert!(lyrics_are_synced(
        &LyricsFormat::Lrc,
        "[ar:Someone]\n[01:02]x"
    ));
    assert!(lyrics_are_synced(
        &LyricsFormat::EnhancedLrc,
        "[00:01.00]<00:01.00>x"
    ));
    assert!(!lyrics_are_synced(&LyricsFormat::Lrc, "[00:01x\nline"));
    assert!(!lyrics_are_synced(&LyricsFormat::Lrc, "[ar:Someone]\nline"));
    // Stamps with no words after them time nothing.
    assert!(!lyrics_are_synced(
        &LyricsFormat::Lrc,
        "[00:01.00]\n[00:02.00]"
    ));
    assert!(!lyrics_are_synced(&LyricsFormat::Plain, "x"));
}

#[test]
fn detect_format_brackets_but_no_timestamp_stays_plain() {
    // A line starting with `[foo]` (LRC metadata header) without
    // any actual time-stamped line should NOT be classified as
    // synchronized.
    let sample = "[ar:Artist]\n[ti:Title]\nVerse without timestamps.";
    assert_eq!(detect_format(sample), LyricsFormat::Plain);
}

#[test]
fn word_stamp_present_basic() {
    assert!(word_stamp_present("<00:01.50>word"));
    assert!(word_stamp_present("plain<5:00>more"));
    assert!(!word_stamp_present("nothing here"));
    assert!(!word_stamp_present("<not:a:stamp>"));
}

#[test]
fn sidecar_finds_same_folder_lrc() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("01 Track.mp3");
    std::fs::write(&audio, b"fake audio").unwrap();
    std::fs::write(dir.path().join("01 Track.lrc"), "[00:01.00]Hello world").unwrap();
    let content = read_sidecar_lyrics(&audio).expect("sidecar should be found");
    assert!(content.contains("Hello world"));
}

#[test]
fn sidecar_prefers_lrc_over_txt_in_same_folder() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"").unwrap();
    std::fs::write(dir.path().join("Song.txt"), "plain content").unwrap();
    std::fs::write(dir.path().join("Song.lrc"), "[00:01.00]synced").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert!(content.contains("synced"), "got: {content}");
    assert!(!content.contains("plain content"));
}

#[test]
fn sidecar_falls_back_to_txt() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"").unwrap();
    std::fs::write(dir.path().join("Song.txt"), "plain content").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert_eq!(content, "plain content");
}

#[test]
fn sidecar_finds_lyrics_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Track.mp3");
    std::fs::write(&audio, b"").unwrap();
    let sub = dir.path().join("Lyrics");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Track.lrc"), "[00:00.00]from subfolder").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert!(content.contains("from subfolder"));
}

#[test]
fn sidecar_subfolder_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Track.mp3");
    std::fs::write(&audio, b"").unwrap();
    // lowercase variant — common on Linux rips.
    let sub = dir.path().join("lyrics");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Track.lrc"), "[00:00.00]lower").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert!(content.contains("lower"));
}

#[test]
fn sidecar_stem_match_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.MP3");
    std::fs::write(&audio, b"").unwrap();
    // Stem differs in casing — should still match.
    std::fs::write(dir.path().join("song.LRC"), "[00:00.00]ok").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert!(content.contains("ok"));
}

#[test]
fn sidecar_same_folder_beats_lyrics_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Track.mp3");
    std::fs::write(&audio, b"").unwrap();
    std::fs::write(dir.path().join("Track.lrc"), "primary").unwrap();
    let sub = dir.path().join("Lyrics");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Track.lrc"), "secondary").unwrap();
    let content = read_sidecar_lyrics(&audio).unwrap();
    assert_eq!(content, "primary");
}

#[test]
fn sidecar_returns_none_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Lonely.mp3");
    std::fs::write(&audio, b"").unwrap();
    assert!(read_sidecar_lyrics(&audio).is_none());
}

#[test]
fn sidecar_empty_lrc_falls_back_to_txt() {
    // Some low-quality rips ship a stub empty `.lrc` alongside a
    // valid plain `.txt`. The empty `.lrc` must NOT short-circuit
    // the waterfall — the `.txt` should win.
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.mp3");
    std::fs::write(&audio, b"").unwrap();
    std::fs::write(dir.path().join("Song.lrc"), "   \n  \n").unwrap();
    std::fs::write(dir.path().join("Song.txt"), "plain backup").unwrap();
    let content = read_sidecar_lyrics(&audio).expect("should fall back to txt");
    assert_eq!(content, "plain backup");
}

#[test]
fn sidecar_skips_directory_named_like_a_sidecar() {
    // A directory named `Song.lrc` must NOT shadow a real
    // `Song.txt` sidecar in the same folder. Before the fix,
    // the directory was selected into `lrc_match`,
    // `read_to_string` failed, and the txt was silently lost.
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.mp3");
    std::fs::write(&audio, b"").unwrap();
    std::fs::create_dir(dir.path().join("Song.lrc")).unwrap();
    std::fs::write(dir.path().join("Song.txt"), "fallback ok").unwrap();
    let content = read_sidecar_lyrics(&audio).expect("should fall back to txt");
    assert_eq!(content, "fallback ok");
}

/// #910: lyrics saved by a Chinese Windows tool are GB18030, and a
/// UTF-8-only read lost them without a word.
#[test]
fn sidecar_in_gb18030_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"").unwrap();
    let lyrics = "[00:01.00]今天的天气很好，我们一起去公园散步吧\n\
         [00:05.50]阳光照在湖面上，风吹过树林的声音";
    let (bytes, _, _) = encoding_rs::GB18030.encode(lyrics);
    assert!(std::str::from_utf8(&bytes).is_err());
    std::fs::write(dir.path().join("Song.lrc"), &bytes).unwrap();
    assert_eq!(read_sidecar_lyrics(&audio).as_deref(), Some(lyrics));
}

/// Write the named sidecars beside an empty `Song.flac`, all with the
/// same modification time, and return what the reader picks.
fn pick_among(files: &[(&str, &str)]) -> Option<String> {
    let dated: Vec<(&str, &str, u64)> = files.iter().map(|&(n, c)| (n, c, 0)).collect();
    pick_among_dated(&dated)
}

/// [`pick_among`] with each file's modification time, in seconds after
/// an arbitrary fixed date.
fn pick_among_dated(files: &[(&str, &str, u64)]) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.flac");
    std::fs::write(&audio, b"").unwrap();
    let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    for (name, content, offset) in files {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(base + std::time::Duration::from_secs(*offset))
            .unwrap();
    }
    read_sidecar_lyrics(&audio)
}

#[test]
fn sidecar_as_rich_and_newer_wins_over_the_extension() {
    // An export left a word-timed `.ttml`; the editor then saved a
    // word-timed `.lrc`. The edit is what reads back.
    let enhanced = "[00:01.00]<00:01.00>la <00:01.50>lu\n[00:02.00]<00:02.00>li <00:02.50>lo";
    let picked = pick_among_dated(&[("Song.ttml", WORD_TTML, 0), ("Song.lrc", enhanced, 60)]);
    assert_eq!(picked.as_deref(), Some(enhanced));
    let picked = pick_among_dated(&[("Song.ttml", WORD_TTML, 60), ("Song.lrc", enhanced, 0)]);
    assert_eq!(picked.as_deref(), Some(WORD_TTML));
}

#[test]
fn sidecar_a_ttml_that_cannot_be_shown_is_no_candidate() {
    let cut_short = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p>la lu"#;
    let picked = pick_among(&[("Song.ttml", cut_short), ("Song.txt", "la lu")]);
    assert_eq!(picked.as_deref(), Some("la lu"));
    let no_line = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p> </p></div></body></tt>"#;
    let picked = pick_among(&[("Song.ttml", no_line), ("Song.lrc", "la lu")]);
    assert_eq!(picked.as_deref(), Some("la lu"));
    assert_eq!(pick_among(&[("Song.ttml", cut_short)]), None);
    let duplicate = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" begin="2s">la lu</p></div></body></tt>"#;
    let picked = pick_among(&[("Song.ttml", duplicate), ("Song.txt", "la lu")]);
    assert_eq!(picked.as_deref(), Some("la lu"));
}

#[test]
fn sidecar_reads_a_ttml_alone() {
    assert_eq!(
        pick_among(&[("Song.TTML", LINE_TTML)]).as_deref(),
        Some(LINE_TTML)
    );
}

#[test]
fn sidecar_word_timed_ttml_beats_line_timed_lrc() {
    let picked = pick_among(&[("Song.lrc", "[00:01.00]la lu"), ("Song.ttml", WORD_TTML)]);
    assert_eq!(picked.as_deref(), Some(WORD_TTML));
}

#[test]
fn sidecar_untimed_ttml_does_not_hide_a_synced_lrc() {
    let picked = pick_among(&[("Song.ttml", UNTIMED_TTML), ("Song.lrc", "[00:01.00]la lu")]);
    assert_eq!(picked.as_deref(), Some("[00:01.00]la lu"));
}

#[test]
fn sidecar_word_timed_lrc_beats_line_timed_ttml() {
    let enhanced = "[00:01.00]<00:01.00>la <00:01.50>lu\n[00:02.00]<00:02.00>li <00:02.50>lo";
    let picked = pick_among(&[("Song.ttml", LINE_TTML), ("Song.lrc", enhanced)]);
    assert_eq!(picked.as_deref(), Some(enhanced));
}

#[test]
fn sidecar_equal_timing_goes_to_ttml_then_lrc_then_txt() {
    let lrc = "[00:01.00]la lu\n[00:02.00]li lo";
    let picked = pick_among(&[("Song.lrc", lrc), ("Song.ttml", LINE_TTML)]);
    assert_eq!(picked.as_deref(), Some(LINE_TTML));
    let picked = pick_among(&[("Song.txt", "la lu"), ("Song.ttml", UNTIMED_TTML)]);
    assert_eq!(picked.as_deref(), Some(UNTIMED_TTML));
}

#[test]
fn sidecar_txt_holding_synced_lrc_beats_an_unsynced_lrc() {
    let synced = "[00:01.00]la lu\n[00:02.00]li lo";
    let picked = pick_among(&[("Song.lrc", "la lu"), ("Song.txt", synced)]);
    assert_eq!(picked.as_deref(), Some(synced));
}

#[test]
fn sidecar_empty_ttml_falls_back_to_lrc() {
    let picked = pick_among(&[("Song.ttml", "   "), ("Song.lrc", "[00:01.00]la")]);
    assert_eq!(picked.as_deref(), Some("[00:01.00]la"));
}

#[test]
fn sidecar_finds_a_ttml_in_the_lyrics_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Track.mp3");
    std::fs::write(&audio, b"").unwrap();
    let sub = dir.path().join("Lyrics");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("Track.ttml"), LINE_TTML).unwrap();
    assert_eq!(read_sidecar_lyrics(&audio).as_deref(), Some(LINE_TTML));
}

#[test]
fn sidecar_returns_none_for_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("Song.mp3");
    std::fs::write(&audio, b"").unwrap();
    std::fs::write(dir.path().join("Song.lrc"), "   \n  \n").unwrap();
    // Whitespace-only payload is treated as a miss so we fall
    // through to the next tier instead of caching an empty hit.
    assert!(read_sidecar_lyrics(&audio).is_none());
}
