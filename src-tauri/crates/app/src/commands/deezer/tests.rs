use super::{is_public_http_url, metadata_album_cache_complete};

#[test]
fn album_cache_completeness_gates_refetch() {
    // Art-less album (`has_local_art = false`): only a cached cover makes
    // the row usable. A cover-less fresh row must NOT be served — it has to
    // re-fetch (art was removed, another profile shares the cache, or a
    // prior download failed), which is the #493 regression this guards.
    assert!(metadata_album_cache_complete(Some("hash"), false));
    assert!(!metadata_album_cache_complete(None, false));

    // Art-having album never needs the Deezer cover, so a cover-less row is
    // a complete hit — no wasteful re-download every time the page opens.
    assert!(metadata_album_cache_complete(None, true));
    assert!(metadata_album_cache_complete(Some("hash"), true));
}

/// The URLs this downloader is handed come from Deezer and
/// TheAudioDB. A host inside the user's machine or network is one
/// neither of them can reach on their own — which is the point of
/// naming it.
#[test]
fn a_third_party_cannot_name_a_host_on_our_side() {
    for url in [
        "http://localhost/x.jpg",
        "https://LOCALHOST./x.jpg",
        "https://printer.local/x.jpg",
        "http://127.0.0.1/x.jpg",
        "http://127.1.2.3/x.jpg",
        "http://10.0.0.5/x.jpg",
        "http://192.168.1.1/x.jpg",
        "http://172.16.0.9/x.jpg",
        "http://169.254.169.254/latest/meta-data",
        "http://0.0.0.0/x.jpg",
        "http://[::1]/x.jpg",
        "http://[fd00::1]/x.jpg",
        "http://[fe80::1]/x.jpg",
        // Not HTTP at all: the local filesystem is the other way in.
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "not a url",
    ] {
        assert!(!is_public_http_url(url), "should be refused: {url}");
    }

    for url in [
        "https://www.theaudiodb.com/images/media/artist/fanart/x.jpg",
        "https://e-cdns-images.dzcdn.net/images/artist/x/500x500.jpg",
        "http://203.0.113.7/x.jpg",
        "https://[2001:db8::1]/x.jpg",
    ] {
        assert!(is_public_http_url(url), "should be allowed: {url}");
    }
}

mod artist_row {
    use super::super::{upsert_artist_row, ArtistRow, BioAnswer, CACHE_TTL_MS, RETRY_AFTER_MS};

    /// A pool whose connections see the real `app` schema as `app`,
    /// attached per connection the way the application attaches it
    /// (see the harness in `lyrics/tests.rs` for why each detail matters).
    async fn pool_with_app_schema(dir: &std::path::Path) -> sqlx::SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let opts_for = |path: &std::path::Path| {
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
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
        SqlitePoolOptions::new()
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
            .unwrap()
    }

    fn row<'a>(now: i64, reached: bool, full: Option<&'a str>) -> ArtistRow<'a> {
        ArtistRow {
            deezer_id: 464,
            name: "Björk",
            picture_url: None,
            picture_hash: None,
            fans_count: Some(1),
            bio: BioAnswer {
                reached,
                short: full,
                full,
                source: "lastfm",
                language: None,
            },
            background_url: None,
            background_hash: None,
            background_urls_json: None,
            background_fetched_at: None,
            now,
        }
    }

    async fn stored(pool: &sqlx::SqlitePool) -> (Option<String>, Option<String>, i64) {
        sqlx::query_as(
            "SELECT bio_full, bio_source, expires_at FROM app.metadata_artist WHERE deezer_id = 464",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A refused key or a network failure is not "this artist has no
    /// biography": the one already cached stays, is what the caller
    /// shows, and the row expires within the hour so a later visit
    /// asks again — not for the TTL, and not on the very next open.
    #[tokio::test]
    async fn a_failure_to_ask_keeps_the_biography_and_is_retried_soon() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        upsert_artist_row(&pool, &row(1_000, true, Some("Born in Reykjavík.")))
            .await
            .unwrap();

        let (_, shown) = upsert_artist_row(&pool, &row(2_000, false, None))
            .await
            .unwrap();

        assert_eq!(shown.as_deref(), Some("Born in Reykjavík."));
        let (bio, source, expires_at) = stored(&pool).await;
        assert_eq!(bio.as_deref(), Some("Born in Reykjavík."));
        assert_eq!(source.as_deref(), Some("lastfm"));
        assert_eq!(
            expires_at,
            2_000 + RETRY_AFTER_MS,
            "a failure is retried after the window, not the TTL"
        );
    }

    /// The other half of the rule, so it cannot be met with a
    /// COALESCE: an answer that carries nothing still replaces the
    /// biography, and is cached for the whole TTL.
    #[tokio::test]
    async fn an_empty_answer_still_replaces_the_biography() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        upsert_artist_row(&pool, &row(1_000, true, Some("Born in Reykjavík.")))
            .await
            .unwrap();

        let (_, shown) = upsert_artist_row(&pool, &row(2_000, true, None))
            .await
            .unwrap();

        assert_eq!(shown, None);
        let (bio, _, expires_at) = stored(&pool).await;
        assert_eq!(bio, None);
        assert_eq!(expires_at, 2_000 + CACHE_TTL_MS);
    }

    /// A first visit that failed to ask records no source, and is
    /// retried after the same window.
    #[tokio::test]
    async fn a_first_failure_records_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;

        upsert_artist_row(&pool, &row(1_000, false, None))
            .await
            .unwrap();

        let (bio, source, expires_at) = stored(&pool).await;
        assert_eq!(
            (bio, source, expires_at),
            (None, None, 1_000 + RETRY_AFTER_MS)
        );
    }

    /// Kept through a failure is not the same as asked for: a biography
    /// fetched under the source the user has since left stays in the
    /// row, and is not shown as the answer to the new one.
    #[tokio::test]
    async fn a_kept_biography_from_another_source_is_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let mut before = row(1_000, true, Some("Née à Reykjavík."));
        before.bio.source = "theaudiodb";
        before.bio.language = Some("fr");
        upsert_artist_row(&pool, &before).await.unwrap();

        let (_, shown) = upsert_artist_row(&pool, &row(2_000, false, None))
            .await
            .unwrap();

        assert_eq!(shown, None);
        let (bio, source, _) = stored(&pool).await;
        assert_eq!(bio.as_deref(), Some("Née à Reykjavík."));
        assert_eq!(source.as_deref(), Some("theaudiodb"));
    }
}
