use super::*;
use sqlx::SqlitePool;
use std::fs;

/// Minimal slice of the profile schema — only the columns
/// `refresh_folder_covers` actually reads or writes. Kept hand-rolled
/// (rather than running the real migrations) because those live in
/// the app crate, out of reach from `waveflow-core`.
async fn fixture_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE artwork (
                 id INTEGER PRIMARY KEY,
                 hash TEXT NOT NULL UNIQUE,
                 format TEXT NOT NULL,
                 source TEXT NOT NULL
                     CHECK (source IN ('embedded','folder','deezer','manual')),
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE album (
                 id INTEGER PRIMARY KEY,
                 artwork_id INTEGER,
                 artwork_source TEXT
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER,
                 folder_id INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 is_available INTEGER NOT NULL DEFAULT 1
             );",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// Content is irrelevant — only its blake3 matters, and two different
/// byte strings give two different hashes.
fn write_cover(dir: &Path, bytes: &[u8]) {
    fs::write(dir.join("cover.jpg"), bytes).unwrap();
}

async fn seed_album(pool: &SqlitePool, album_id: i64, dir: &Path, artwork_id: Option<i64>) {
    sqlx::query(
        "INSERT INTO album (id, artwork_id, artwork_source)
             VALUES (?, ?, (SELECT source FROM artwork WHERE id = ?))",
    )
    .bind(album_id)
    .bind(artwork_id)
    .bind(artwork_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (?, 1, ?, 1)",
    )
    .bind(album_id)
    .bind(dir.join("01.flac").to_string_lossy().to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_artwork(pool: &SqlitePool, id: i64, hash: &str, source: &str) {
    sqlx::query(
        "INSERT INTO artwork (id, hash, format, source, created_at)
             VALUES (?, ?, 'jpg', ?, 0)",
    )
    .bind(id)
    .bind(hash)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn album_artwork_hash(pool: &SqlitePool, album_id: i64) -> Option<String> {
    sqlx::query_scalar(
        "SELECT aw.hash FROM album al
               JOIN artwork aw ON aw.id = al.artwork_id
              WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The regression from issue #366: the audio files are untouched, so
/// the scanner's `(mtime, size)` fast path skips them — but the
/// sidecar next to them was replaced and the album must follow.
#[tokio::test]
async fn replacing_a_sidecar_updates_the_album_cover() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    write_cover(music.path(), b"original cover bytes");
    let old_hash = blake3::hash(b"original cover bytes").to_hex().to_string();
    seed_artwork(&pool, 1, &old_hash, "folder").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;

    // User swaps the picture; the audio files are not touched.
    write_cover(music.path(), b"a completely different cover");
    let new_hash = blake3::hash(b"a completely different cover")
        .to_hex()
        .to_string();

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 1);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(new_hash));
}

/// An album that already matches its sidecar must not be rewritten —
/// otherwise every scan would churn `album.artwork_id` library-wide.
#[tokio::test]
async fn an_unchanged_sidecar_is_a_no_op() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    write_cover(music.path(), b"steady cover");
    let hash = blake3::hash(b"steady cover").to_hex().to_string();
    seed_artwork(&pool, 1, &hash, "folder").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
}

/// Embedded artwork outranks a sidecar during extraction, so this
/// pass must not invert that precedence behind the user's back.
#[tokio::test]
async fn embedded_artwork_is_never_overridden_by_a_sidecar() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let embedded_hash = blake3::hash(b"from the tag").to_hex().to_string();
    seed_artwork(&pool, 1, &embedded_hash, "embedded").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;
    write_cover(music.path(), b"sidecar that must lose");

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(embedded_hash));
}

/// An album with no artwork at all picks the sidecar up.
#[tokio::test]
async fn an_album_without_artwork_adopts_the_sidecar() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    seed_album(&pool, 10, music.path(), None).await;
    write_cover(music.path(), b"first ever cover");
    let hash = blake3::hash(b"first ever cover").to_hex().to_string();

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 1);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
}

/// Deleting a sidecar must not blank an album — far more likely to be
/// a transient state (files being moved) than a request for no cover.
#[tokio::test]
async fn a_missing_sidecar_leaves_the_existing_cover_alone() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let hash = blake3::hash(b"cover that outlives its file")
        .to_hex()
        .to_string();
    seed_artwork(&pool, 1, &hash, "folder").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;
    // No cover.jpg written at all.

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
}

/// Scoping is by *album touched by the scanned folder*, not by track
/// folder — a multi-folder album deliberately pulls in its other
/// directories (see `a_multi_folder_album_resolves_consistently`).
/// What must stay out is an album with no track in this folder at all.
#[tokio::test]
async fn an_album_outside_the_scanned_folder_is_untouched() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let hash = blake3::hash(b"untouched").to_hex().to_string();
    seed_artwork(&pool, 1, &hash, "folder").await;
    sqlx::query(
        "INSERT INTO album (id, artwork_id, artwork_source)
             VALUES (10, 1, (SELECT source FROM artwork WHERE id = 1))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (10, 2, ?, 1)",
    )
    .bind(music.path().join("01.flac").to_string_lossy().to_string())
    .execute(&pool)
    .await
    .unwrap();
    write_cover(music.path(), b"a new cover in another folder");

    // Scanning folder 1 — the track above lives under folder 2.
    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(hash));
}

/// A manually uploaded cover must survive a sidecar sitting next to
/// the files — the user picked it deliberately. Same for a Deezer
/// enrichment result below; the guard is an allowlist, so any source
/// other than `folder` is left alone.
#[tokio::test]
async fn a_manually_uploaded_cover_is_not_replaced_by_a_sidecar() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let chosen = blake3::hash(b"the cover the user picked")
        .to_hex()
        .to_string();
    seed_artwork(&pool, 1, &chosen, "manual").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;
    write_cover(music.path(), b"a sidecar that must not win");

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(chosen));
}

#[tokio::test]
async fn a_deezer_cover_is_not_replaced_by_a_sidecar() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let fetched = blake3::hash(b"fetched from deezer").to_hex().to_string();
    seed_artwork(&pool, 1, &fetched, "deezer").await;
    seed_album(&pool, 10, music.path(), Some(1)).await;
    write_cover(music.path(), b"a sidecar that must not win");

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(fetched));
}

/// An album split across two library folders must resolve to the same
/// directory whichever folder triggered the scan. Scoping the track
/// query to the scanned folder would make disc 1 win when folder 1 is
/// scanned and disc 2 win when folder 2 is, flipping the album's
/// picture back and forth on every pass.
#[tokio::test]
async fn a_multi_folder_album_resolves_consistently() {
    let root = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    // "disc-1" sorts before "disc-2", so disc 1's cover must win in
    // both directions.
    let disc1 = root.path().join("disc-1");
    let disc2 = root.path().join("disc-2");
    fs::create_dir_all(&disc1).unwrap();
    fs::create_dir_all(&disc2).unwrap();
    write_cover(&disc1, b"disc one cover");
    write_cover(&disc2, b"disc two cover");
    let disc1_hash = blake3::hash(b"disc one cover").to_hex().to_string();

    for scanned_folder in [1_i64, 2] {
        let pool = fixture_pool().await;
        sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, NULL)")
            .execute(&pool)
            .await
            .unwrap();
        for (folder_id, dir) in [(1_i64, &disc1), (2_i64, &disc2)] {
            sqlx::query(
                "INSERT INTO track (album_id, folder_id, file_path, is_available)
                     VALUES (10, ?, ?, 1)",
            )
            .bind(folder_id)
            .bind(dir.join("01.flac").to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        }

        let updated = refresh_folder_covers(&pool, art.path(), scanned_folder)
            .await
            .unwrap();

        assert_eq!(updated, 1, "scanning folder {scanned_folder}");
        assert_eq!(
            album_artwork_hash(&pool, 10).await,
            Some(disc1_hash.clone()),
            "scanning folder {scanned_folder} must pick the first directory",
        );
    }
}

/// The write-time half of the allowlist. `refresh_folder_covers`
/// filters candidates from a read taken before the blocking directory
/// walk, so these cases cover what happens when the album's artwork
/// changes during that window — a race a full-pass test cannot stage
/// deterministically, which is why the guard is exercised directly.
///
/// Each case releases its connection before reading back through the
/// pool. Not strictly required today — sqlx shares one in-memory
/// database across a pool's connections — but the assertions should
/// not quietly depend on that.
#[tokio::test]
async fn the_write_guard_replaces_a_sidecar_cover() {
    let pool = fixture_pool().await;
    seed_artwork(&pool, 1, "old-sidecar", "folder").await;
    seed_artwork(&pool, 2, "new-sidecar", "folder").await;
    sqlx::query(
        "INSERT INTO album (id, artwork_id, artwork_source)
             VALUES (10, 1, (SELECT source FROM artwork WHERE id = 1))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let changed = link_folder_cover_if_eligible(&mut conn, 10, Some(1), 2)
        .await
        .unwrap();
    drop(conn);

    assert!(changed);
    assert_eq!(
        album_artwork_hash(&pool, 10).await,
        Some("new-sidecar".to_string())
    );
}

#[tokio::test]
async fn the_write_guard_adopts_a_cover_when_the_album_has_none() {
    let pool = fixture_pool().await;
    seed_artwork(&pool, 2, "new-sidecar", "folder").await;
    sqlx::query("INSERT INTO album (id, artwork_id) VALUES (10, NULL)")
        .execute(&pool)
        .await
        .unwrap();

    // Observed as NULL — the CAS binds with `IS`, which matches NULL
    // where `=` would not.
    let mut conn = pool.acquire().await.unwrap();
    let changed = link_folder_cover_if_eligible(&mut conn, 10, None, 2)
        .await
        .unwrap();
    drop(conn);

    assert!(changed);
    assert_eq!(
        album_artwork_hash(&pool, 10).await,
        Some("new-sidecar".to_string())
    );
}

/// The race this guard exists for: the user uploaded a cover after the
/// candidate list was built. The update must match no row, and must
/// report that it changed nothing so the summary stays honest.
#[tokio::test]
async fn the_write_guard_refuses_a_cover_that_became_user_owned() {
    for source in ["manual", "deezer", "embedded"] {
        let pool = fixture_pool().await;
        seed_artwork(&pool, 1, "chosen-mid-scan", source).await;
        seed_artwork(&pool, 2, "new-sidecar", "folder").await;
        sqlx::query(
            "INSERT INTO album (id, artwork_id, artwork_source)
             VALUES (10, 1, (SELECT source FROM artwork WHERE id = 1))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let changed = link_folder_cover_if_eligible(&mut conn, 10, Some(1), 2)
            .await
            .unwrap();
        drop(conn);

        assert!(!changed, "source {source} must not be replaced");
        assert_eq!(
            album_artwork_hash(&pool, 10).await,
            Some("chosen-mid-scan".to_string()),
            "source {source} must survive",
        );
    }
}

/// Compare-and-swap, the hazard the source allowlist alone cannot
/// catch: both the old and the new cover are `folder`-sourced, so
/// every source check passes and only the observed identity
/// distinguishes them.
///
/// A concurrent scan of the same album resolved a fresher sidecar
/// while this pass was walking directories. Writing our value would
/// roll that back to what a staler read computed — last writer wins,
/// with no signal that anything was lost.
#[tokio::test]
async fn the_write_guard_refuses_to_roll_back_a_newer_folder_cover() {
    let pool = fixture_pool().await;
    seed_artwork(&pool, 1, "cover-we-observed", "folder").await;
    seed_artwork(&pool, 2, "cover-we-resolved", "folder").await;
    seed_artwork(&pool, 3, "cover-a-concurrent-scan-landed", "folder").await;
    // The album moved on to artwork 3 after our candidate list was
    // built, so our observation of artwork 1 is stale.
    sqlx::query(
        "INSERT INTO album (id, artwork_id, artwork_source)
             VALUES (10, 3, (SELECT source FROM artwork WHERE id = 3))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let changed = link_folder_cover_if_eligible(&mut conn, 10, Some(1), 2)
        .await
        .unwrap();
    drop(conn);

    assert!(!changed, "a stale write must not report success");
    assert_eq!(
        album_artwork_hash(&pool, 10).await,
        Some("cover-a-concurrent-scan-landed".to_string()),
        "the newer cover must survive",
    );
}

/// Issue #401: `artwork` rows dedup on the content hash alone and
/// `source` is only written on INSERT, so it records whoever put the
/// bytes in the library first — not where *this* album got them.
///
/// The setup below is the one that used to freeze an album's cover
/// forever: the same image is embedded in one album's tags and sits
/// as a `cover.jpg` beside another, so there is a single `artwork`
/// row labelled `embedded`. Reading provenance off that row made the
/// sidecar album look untouchable. Reading it off the album's own
/// `artwork_source` gets it right.
#[tokio::test]
async fn a_sidecar_album_stays_refreshable_when_another_album_embeds_the_same_image() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    // Album A embedded these bytes first, so the shared row says
    // `embedded` and there is no second row to disambiguate.
    write_cover(music.path(), b"an image two albums share");
    let shared = blake3::hash(b"an image two albums share")
        .to_hex()
        .to_string();
    seed_artwork(&pool, 1, &shared, "embedded").await;

    // Album B got the very same bytes from a sidecar.
    sqlx::query("INSERT INTO album (id, artwork_id, artwork_source) VALUES (10, 1, 'folder')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (10, 1, ?, 1)",
    )
    .bind(music.path().join("01.flac").to_string_lossy().to_string())
    .execute(&pool)
    .await
    .unwrap();

    // The user replaces album B's sidecar.
    write_cover(music.path(), b"album B gets its own cover at last");
    let replacement = blake3::hash(b"album B gets its own cover at last")
        .to_hex()
        .to_string();

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 1, "the sidecar album must still be refreshable");
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(replacement));
}

/// The mirror image: an album that really did take its cover from a
/// tag is still protected, even when some *other* album later brings
/// the same bytes in from a sidecar and flips nothing about the
/// shared `artwork` row.
#[tokio::test]
async fn an_embedded_album_stays_protected_regardless_of_the_shared_row() {
    let music = tempfile::tempdir().unwrap();
    let art = tempfile::tempdir().unwrap();
    let pool = fixture_pool().await;

    let from_tag = blake3::hash(b"lifted from the tag").to_hex().to_string();
    // The shared row happens to say `folder` — inserted first by some
    // other album's sidecar. The album's own provenance is what counts.
    seed_artwork(&pool, 1, &from_tag, "folder").await;
    sqlx::query("INSERT INTO album (id, artwork_id, artwork_source) VALUES (10, 1, 'embedded')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track (album_id, folder_id, file_path, is_available)
             VALUES (10, 1, ?, 1)",
    )
    .bind(music.path().join("01.flac").to_string_lossy().to_string())
    .execute(&pool)
    .await
    .unwrap();
    write_cover(music.path(), b"a sidecar that must lose to the tag");

    let updated = refresh_folder_covers(&pool, art.path(), 1).await.unwrap();

    assert_eq!(updated, 0);
    assert_eq!(album_artwork_hash(&pool, 10).await, Some(from_tag));
}
