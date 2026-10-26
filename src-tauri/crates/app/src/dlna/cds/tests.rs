use super::*;

#[test]
fn extract_tag_handles_namespace_prefixes() {
    let body = "<u:Browse><ObjectID>0/albums</ObjectID></u:Browse>";
    assert_eq!(extract_tag(body, "ObjectID"), Some("0/albums".into()));
}

#[test]
fn extract_tag_unescapes_xml_entities() {
    let body = "<ObjectID>0/path&amp;weird</ObjectID>";
    assert_eq!(extract_tag(body, "ObjectID"), Some("0/path&weird".into()));
}

#[test]
fn parse_soap_action_recognises_browse() {
    let body = r#"<s:Envelope><s:Body><u:Browse xmlns:u="..."><ObjectID>0</ObjectID><BrowseFlag>BrowseDirectChildren</BrowseFlag><StartingIndex>5</StartingIndex><RequestedCount>20</RequestedCount></u:Browse></s:Body></s:Envelope>"#;
    match parse_soap_action(body) {
        Some(SoapAction::Browse(req)) => {
            assert_eq!(req.object_id, "0");
            assert_eq!(req.starting_index, 5);
            assert_eq!(req.requested_count, 20);
        }
        _ => panic!("expected Browse"),
    }
}

#[test]
fn duration_formats_hms_with_ms_padding() {
    assert_eq!(format_duration_hms(0), "0:00:00.000");
    assert_eq!(format_duration_hms(3_661_000), "1:01:01.000");
}

#[test]
fn root_lists_five_top_level_containers_and_pages_them() {
    let labels = RootLabels::default();
    let r = root_children(&labels, [3, 7, 2, 4, 1], MAX_PAGE_SIZE, 0);
    assert_eq!((r.number_returned, r.total_matches), (5, 5));
    assert_eq!(
        ids(&r.didl),
        [
            "0/artists",
            "0/albums",
            "0/genres",
            "0/playlists",
            "0/folders"
        ]
    );
    assert!(r
        .didl
        .contains(r#"id="0/artists" parentID="0" restricted="1" childCount="3""#));
    assert!(r
        .didl
        .contains(r#"id="0/folders" parentID="0" restricted="1" childCount="1""#));

    let page = root_children(&labels, [3, 7, 2, 4, 1], 2, 3);
    assert_eq!((page.number_returned, page.total_matches), (2, 5));
    assert_eq!(ids(&page.didl), ["0/playlists", "0/folders"]);

    let past = root_children(&labels, [3, 7, 2, 4, 1], 10, 5);
    assert_eq!((past.number_returned, past.total_matches), (0, 5));
}

#[test]
fn top_level_titles_follow_the_pushed_labels() {
    let labels = RootLabels {
        artists: "Artistes".into(),
        albums: "Albums".into(),
        genres: "Genres".into(),
        playlists: "Playlists & co".into(),
        folders: "Dossiers".into(),
    };
    let r = root_children(&labels, [0; 5], MAX_PAGE_SIZE, 0);
    assert!(r.didl.contains("<dc:title>Artistes</dc:title>"));
    assert!(r.didl.contains("<dc:title>Dossiers</dc:title>"));
    assert!(r.didl.contains("<dc:title>Playlists &amp; co</dc:title>"));
}

#[test]
fn object_id_parses_every_id_the_server_emits() {
    assert_eq!(ObjectId::parse(""), Some(ObjectId::Root));
    assert_eq!(ObjectId::parse("0"), Some(ObjectId::Root));
    assert_eq!(ObjectId::parse("0/artists"), Some(ObjectId::Artists));
    assert_eq!(ObjectId::parse("0/artists/12"), Some(ObjectId::Artist(12)));
    assert_eq!(ObjectId::parse("0/albums"), Some(ObjectId::Albums));
    assert_eq!(ObjectId::parse("0/albums/34"), Some(ObjectId::Album(34)));
    assert_eq!(ObjectId::parse("0/genres"), Some(ObjectId::Genres));
    assert_eq!(ObjectId::parse("0/genres/5"), Some(ObjectId::Genre(5)));
    assert_eq!(ObjectId::parse("0/playlists"), Some(ObjectId::Playlists));
    assert_eq!(
        ObjectId::parse("0/playlists/6"),
        Some(ObjectId::Playlist(6))
    );
    assert_eq!(ObjectId::parse("0/folders"), Some(ObjectId::Folders));
    assert_eq!(
        ObjectId::parse("0/folders/7"),
        Some(ObjectId::Folder {
            root: 7,
            rel: vec![]
        })
    );
    assert_eq!(
        ObjectId::parse("0/folders/7/Rock/A%20%26%20B%25"),
        Some(ObjectId::Folder {
            root: 7,
            rel: vec!["Rock".into(), "A & B%".into()]
        })
    );
    assert_eq!(ObjectId::parse("0/track/56"), Some(ObjectId::Track(56)));
}

#[test]
fn every_id_round_trips_through_its_string() {
    for id in [
        ObjectId::Root,
        ObjectId::Artist(1),
        ObjectId::Album(2),
        ObjectId::Genres,
        ObjectId::Genre(3),
        ObjectId::Playlist(4),
        ObjectId::Folders,
        ObjectId::Folder {
            root: 5,
            rel: vec![],
        },
        ObjectId::Folder {
            root: 5,
            rel: vec![
                " lead".into(),
                "trail ".into(),
                "日本語".into(),
                "a<b>\"c\"".into(),
            ],
        },
        ObjectId::Track(6),
    ] {
        let text = id.to_id();
        assert!(
            text.bytes().all(|b| b.is_ascii_graphic()),
            "an id must stay plain, unescaped ASCII: {text}"
        );
        assert_eq!(ObjectId::parse(&text), Some(id), "{text}");
    }
}

#[test]
fn object_id_rejects_what_the_server_never_emits() {
    assert_eq!(ObjectId::parse("0/artists/"), None);
    assert_eq!(ObjectId::parse("0/albums/abc"), None);
    assert_eq!(ObjectId::parse("0/track"), None);
    assert_eq!(ObjectId::parse("0/genre"), None);
    assert_eq!(ObjectId::parse("0/folders/"), None);
    assert_eq!(ObjectId::parse("0/folders/x"), None);
    assert_eq!(ObjectId::parse("0/folders/1/"), None);
    assert_eq!(ObjectId::parse("0/folders/1//a"), None);
    assert_eq!(ObjectId::parse("0/folders/1/.."), None);
    assert_eq!(ObjectId::parse("0/folders/1/%2E%2E"), None);
    assert_eq!(ObjectId::parse("0/folders/1/%2F"), None);
    assert_eq!(ObjectId::parse("0/folders/1/%zz"), None);
    assert_eq!(ObjectId::parse("0/folders/1/%4"), None);
    assert_eq!(ObjectId::parse("0/folders/1/%C3"), None);
    assert_eq!(ObjectId::parse("1"), None);
}

#[test]
fn every_container_names_the_container_that_lists_it() {
    assert_eq!(ObjectId::Root.container_parent(), "-1");
    for top in &TOP_LEVEL {
        assert_eq!(top.container_parent(), "0");
    }
    assert_eq!(ObjectId::Artist(1).container_parent(), "0/artists");
    assert_eq!(ObjectId::Album(1).container_parent(), "0/albums");
    assert_eq!(ObjectId::Genre(1).container_parent(), "0/genres");
    assert_eq!(ObjectId::Playlist(1).container_parent(), "0/playlists");
    let folder = |rel: &[&str]| ObjectId::Folder {
        root: 3,
        rel: rel.iter().map(|s| s.to_string()).collect(),
    };
    assert_eq!(folder(&[]).container_parent(), "0/folders");
    assert_eq!(folder(&["Rock"]).container_parent(), "0/folders/3");
    assert_eq!(
        folder(&["Rock", "A b"]).container_parent(),
        "0/folders/3/Rock"
    );
}

#[test]
fn a_track_names_its_album_or_the_root() {
    assert_eq!(track_parent_id(Some(9)), "0/albums/9");
    assert_eq!(track_parent_id(None), "0");
}

#[test]
fn single_result_counts_one_object_or_none() {
    let one = single_result(Some(container_xml(
        "0/albums/1",
        "0/albums",
        "A & B",
        Some(4),
    )));
    assert_eq!((one.number_returned, one.total_matches), (1, 1));
    assert!(one
        .didl
        .contains(r#"childCount="4"><dc:title>A &amp; B</dc:title>"#));
    let none = single_result(None);
    assert_eq!((none.number_returned, none.total_matches), (0, 0));
}

// ── Browse against a real profile database ──────────────────────

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::PathBuf;
use std::str::FromStr;

/// `ids` of every object in a DIDL document, in document order.
fn ids(didl: &str) -> Vec<String> {
    attr_values(didl, " id=\"")
}

fn attr_values(didl: &str, needle: &str) -> Vec<String> {
    didl.match_indices(needle)
        .map(|(at, _)| {
            let rest = &didl[at + needle.len()..];
            rest[..rest.find('"').unwrap()].to_string()
        })
        .collect()
}

/// The `childCount` of every container, in document order.
fn child_counts(didl: &str) -> Vec<i64> {
    attr_values(didl, " childCount=\"")
        .iter()
        .map(|n| n.parse().unwrap())
        .collect()
}

/// An absolute path built with the separator the scanner stores.
fn path(parts: &[&str]) -> String {
    let sep = std::path::MAIN_SEPARATOR.to_string();
    format!("{sep}{}", parts.join(&sep))
}

/// The real migrator, `foreign_keys` on and the `FOLD` collation
/// registered, as the profile pool has them.
async fn ctx() -> ServerCtx {
    let options = waveflow_core::repository::sqlite::collation::register(
        SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true),
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    seed(&pool).await;
    ServerCtx {
        server_name: "WaveFlow".into(),
        base_url: "http://h".into(),
        pool,
        profile_artwork_dir: PathBuf::new(),
        metadata_artwork_dir: PathBuf::new(),
        labels: Default::default(),
    }
}

/// Two library folders, one of them empty. Under `/m`:
///
/// ```text
/// Rock/A/01.flac      track 1  album 1, genre 1
/// Rock/A/02.flac      track 2  album 1, genre 1
/// Rock/B/x.flac       track 3
/// A & B%/song.flac    track 6
/// Gone/old.flac       track 5  unavailable, genre 2
/// top.flac            track 4
/// ```
///
/// Playlist 1 holds tracks 2, 1 and 5 in that order; playlist 2 is
/// empty.
async fn seed(pool: &SqlitePool) {
    for sql in [
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'Björk', 'bjork')",
        "INSERT INTO album (id, title, canonical_title, artist_id)
             VALUES (1, 'Vespertine', 'vespertine', 1)",
        "INSERT INTO genre (id, name, canonical_name) VALUES (1, 'Electronic', 'electronic')",
        "INSERT INTO genre (id, name, canonical_name) VALUES (2, 'Jazz', 'jazz')",
        "INSERT INTO playlist (id, name, position, created_at, updated_at)
             VALUES (1, 'Mix', 0, 0, 0)",
        "INSERT INTO playlist (id, name, position, created_at, updated_at)
             VALUES (2, 'Empty', 1, 0, 0)",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }
    for (id, root) in [(1, path(&["m"])), (2, path(&["other"]))] {
        sqlx::query("INSERT INTO library_folder (id, library_id, path) VALUES (?, 1, ?)")
            .bind(id)
            .bind(root)
            .execute(pool)
            .await
            .unwrap();
    }
    let tracks: [(i64, &[&str], Option<i64>, i64); 6] = [
        (1, &["m", "Rock", "A", "01.flac"], Some(1), 1),
        (2, &["m", "Rock", "A", "02.flac"], Some(1), 1),
        (3, &["m", "Rock", "B", "x.flac"], None, 1),
        (4, &["m", "top.flac"], None, 1),
        (5, &["m", "Gone", "old.flac"], None, 0),
        (6, &["m", "A & B%", "song.flac"], None, 1),
    ];
    for (id, parts, album, available) in tracks {
        sqlx::query(
            "INSERT INTO track (id, library_id, folder_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, primary_artist, track_number,
                                    duration_ms, added_at, is_available)
                 VALUES (?, 1, 1, ?, 'h', 1, 0, ?, ?, 1, ?, 1000, 0, ?)",
        )
        .bind(id)
        .bind(path(parts))
        .bind(format!("T{id}"))
        .bind(album)
        .bind(id)
        .bind(available)
        .execute(pool)
        .await
        .unwrap();
    }
    for (track, genre) in [(1, 1), (2, 1), (5, 2)] {
        sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, ?)")
            .bind(track)
            .bind(genre)
            .execute(pool)
            .await
            .unwrap();
    }
    for (track, position) in [(2, 0), (1, 1), (5, 2)] {
        sqlx::query(
            "INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
                 VALUES (1, ?, ?, 0)",
        )
        .bind(track)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn children(ctx: &ServerCtx, oid: &str, start: i64, count: i64) -> BrowseResult {
    let req = BrowseRequest {
        object_id: oid.into(),
        browse_flag: "BrowseDirectChildren".into(),
        starting_index: start,
        requested_count: count,
    };
    browse(ctx, "http://h", &req).await.unwrap()
}

async fn metadata(ctx: &ServerCtx, oid: &str) -> BrowseResult {
    let req = BrowseRequest {
        object_id: oid.into(),
        browse_flag: "BrowseMetadata".into(),
        starting_index: 0,
        requested_count: 0,
    };
    browse(ctx, "http://h", &req).await.unwrap()
}

#[tokio::test]
async fn the_root_counts_what_each_folder_lists() {
    let ctx = ctx().await;
    let root = children(&ctx, "0", 0, 0).await;
    assert_eq!(
        ids(&root.didl),
        [
            "0/artists",
            "0/albums",
            "0/genres",
            "0/playlists",
            "0/folders"
        ]
    );
    // The genre whose only track is gone is not counted.
    assert_eq!(child_counts(&root.didl), [1, 1, 1, 2, 2]);
}

#[tokio::test]
async fn a_label_push_retitles_a_running_server() {
    let ctx = ctx().await;
    assert!(children(&ctx, "0", 0, 0)
        .await
        .didl
        .contains("<dc:title>Folders</dc:title>"));
    *ctx.labels.write().unwrap() = RootLabels {
        folders: "Dossiers".into(),
        ..RootLabels::default()
    };
    assert!(children(&ctx, "0", 0, 0)
        .await
        .didl
        .contains("<dc:title>Dossiers</dc:title>"));
    assert!(metadata(&ctx, "0/folders")
        .await
        .didl
        .contains("<dc:title>Dossiers</dc:title>"));
}

#[tokio::test]
async fn genres_list_only_what_still_plays() {
    let ctx = ctx().await;
    let genres = children(&ctx, "0/genres", 0, 0).await;
    assert_eq!(ids(&genres.didl), ["0/genres/1"]);
    assert_eq!(genres.total_matches, 1);

    let tracks = children(&ctx, "0/genres/1", 0, 0).await;
    assert_eq!(ids(&tracks.didl), ["0/track/1", "0/track/2"]);
    assert_eq!(
        attr_values(&tracks.didl, " parentID=\""),
        ["0/genres/1", "0/genres/1"]
    );
    assert_eq!(children(&ctx, "0/genres/2", 0, 0).await.total_matches, 0);
}

/// A genre plays each album through. A compilation under Various
/// Artists that shares a title with an artist's own album stays one
/// block: sorted by the track's artist, its track by Aphex would
/// open the genre, and running order alone would deal the two
/// albums' track 1s, then their track 2s.
#[tokio::test]
async fn a_genre_keeps_each_album_in_one_block() {
    let ctx = ctx().await;
    for sql in [
        "INSERT INTO artist (id, name, canonical_name)
             VALUES (2, 'Various Artists', 'various artists')",
        "INSERT INTO artist (id, name, canonical_name) VALUES (3, 'Aphex', 'aphex')",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (2, 'Vespertine', 'vespertine', 2, 1)",
    ] {
        sqlx::query(sql).execute(&ctx.pool).await.unwrap();
    }
    for (id, number, artist) in [(7, 1, 3), (8, 2, 1)] {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, primary_artist,
                                    track_number, duration_ms, added_at)
                 VALUES (?, 1, ?, 'h', 1, 0, ?, 2, ?, ?, 1000, 0)",
        )
        .bind(id)
        .bind(path(&["m", "Comp", &format!("{number}.flac")]))
        .bind(format!("T{id}"))
        .bind(artist)
        .bind(number)
        .execute(&ctx.pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO track_genre (track_id, genre_id) VALUES (?, 1)")
            .bind(id)
            .execute(&ctx.pool)
            .await
            .unwrap();
    }
    let tracks = children(&ctx, "0/genres/1", 0, 0).await;
    assert_eq!(
        ids(&tracks.didl),
        ["0/track/1", "0/track/2", "0/track/7", "0/track/8"]
    );
}

#[tokio::test]
async fn a_playlist_keeps_its_order_and_pages() {
    let ctx = ctx().await;
    let lists = children(&ctx, "0/playlists", 0, 0).await;
    assert_eq!(ids(&lists.didl), ["0/playlists/1", "0/playlists/2"]);
    // The unavailable track is in the playlist but not counted.
    assert_eq!(child_counts(&lists.didl), [2, 0]);

    let all = children(&ctx, "0/playlists/1", 0, 0).await;
    assert_eq!(ids(&all.didl), ["0/track/2", "0/track/1"]);
    let second = children(&ctx, "0/playlists/1", 1, 1).await;
    assert_eq!(ids(&second.didl), ["0/track/1"]);
    assert_eq!((second.number_returned, second.total_matches), (1, 2));
}

#[tokio::test]
async fn a_folder_lists_its_directories_then_its_files() {
    let ctx = ctx().await;
    let roots = children(&ctx, "0/folders", 0, 0).await;
    assert_eq!(ids(&roots.didl), ["0/folders/1", "0/folders/2"]);
    // `A & B%`, `Rock` and `top.flac`; `Gone` holds nothing that plays.
    assert_eq!(child_counts(&roots.didl), [3, 0]);
    assert!(roots
        .didl
        .contains(&format!("<dc:title>{}</dc:title>", path(&["m"]))));

    let m = children(&ctx, "0/folders/1", 0, 0).await;
    assert_eq!(
        ids(&m.didl),
        [
            "0/folders/1/A%20%26%20B%25",
            "0/folders/1/Rock",
            "0/track/4"
        ]
    );
    assert_eq!(child_counts(&m.didl), [1, 2]);
    assert!(m.didl.contains("<dc:title>A &amp; B%</dc:title>"));
    assert_eq!(
        attr_values(&m.didl, " parentID=\""),
        ["0/folders/1", "0/folders/1", "0/folders/1"]
    );

    let rock = children(&ctx, "0/folders/1/Rock", 0, 0).await;
    assert_eq!(
        ids(&rock.didl),
        ["0/folders/1/Rock/A", "0/folders/1/Rock/B"]
    );
    assert_eq!(child_counts(&rock.didl), [2, 1]);

    let a = children(&ctx, "0/folders/1/Rock/A", 0, 0).await;
    assert_eq!(ids(&a.didl), ["0/track/1", "0/track/2"]);
    assert_eq!(
        attr_values(&a.didl, " parentID=\""),
        ["0/folders/1/Rock/A", "0/folders/1/Rock/A"]
    );

    let odd = children(&ctx, "0/folders/1/A%20%26%20B%25", 0, 0).await;
    assert_eq!(ids(&odd.didl), ["0/track/6"]);
}

#[tokio::test]
async fn a_folder_page_runs_from_its_directories_into_its_files() {
    let ctx = ctx().await;
    let straddle = children(&ctx, "0/folders/1", 1, 2).await;
    assert_eq!(ids(&straddle.didl), ["0/folders/1/Rock", "0/track/4"]);
    assert_eq!((straddle.number_returned, straddle.total_matches), (2, 3));

    let files_only = children(&ctx, "0/folders/1", 2, 10).await;
    assert_eq!(ids(&files_only.didl), ["0/track/4"]);
    assert_eq!(
        (files_only.number_returned, files_only.total_matches),
        (1, 3)
    );

    let dirs_only = children(&ctx, "0/folders/1", 0, 2).await;
    assert_eq!(dirs_only.number_returned, 2);
    assert_eq!(ids(&dirs_only.didl).len(), 2);

    let past = children(&ctx, "0/folders/1", 3, 10).await;
    assert_eq!((past.number_returned, past.total_matches), (0, 3));
}

#[tokio::test]
async fn folder_metadata_names_its_parent_and_misses_what_is_gone() {
    let ctx = ctx().await;
    let rock = metadata(&ctx, "0/folders/1/Rock").await;
    assert_eq!(ids(&rock.didl), ["0/folders/1/Rock"]);
    assert_eq!(attr_values(&rock.didl, " parentID=\""), ["0/folders/1"]);
    assert!(rock.didl.contains("<dc:title>Rock</dc:title>"));

    let root = metadata(&ctx, "0/folders/1").await;
    assert_eq!(attr_values(&root.didl, " parentID=\""), ["0/folders"]);
    // An empty library folder still exists: its row says so.
    assert_eq!(metadata(&ctx, "0/folders/2").await.number_returned, 1);

    for gone in ["0/folders/1/Gone", "0/folders/1/Nowhere", "0/folders/9"] {
        assert_eq!(metadata(&ctx, gone).await.number_returned, 0, "{gone}");
        assert_eq!(children(&ctx, gone, 0, 0).await.total_matches, 0, "{gone}");
    }
}

/// Walk the whole tree from the root: every object a listing
/// returns answers `BrowseMetadata` with itself, and every
/// container's `childCount` — in its parent's listing and in its
/// own metadata — is what browsing it reports. Those three
/// disagreeing is what a controller shows as a folder that claims
/// items and opens empty.
#[tokio::test]
async fn every_listed_object_answers_its_own_metadata() {
    let ctx = ctx().await;
    let mut queue = vec![("0".to_string(), 5)];
    let mut seen = 0;
    while let Some((oid, expected)) = queue.pop() {
        let listing = children(&ctx, &oid, 0, 0).await;
        assert_eq!(listing.total_matches, expected, "childCount of {oid}");
        assert_eq!(listing.number_returned, expected, "{oid}");
        let containers = attr_values(&listing.didl, "<container id=\"");
        let counts = child_counts(&listing.didl);
        assert_eq!(containers.len(), counts.len());
        for (child, count) in containers.into_iter().zip(counts) {
            let own = metadata(&ctx, &child).await;
            assert_eq!(
                ids(&own.didl),
                std::slice::from_ref(&child),
                "metadata of {child}"
            );
            assert_eq!(child_counts(&own.didl), [count], "metadata of {child}");
            queue.push((child, count));
        }
        for item in attr_values(&listing.didl, "<item id=\"") {
            let own = metadata(&ctx, &item).await;
            assert_eq!(
                ids(&own.didl),
                std::slice::from_ref(&item),
                "metadata of {item}"
            );
        }
        seen += 1;
    }
    // The root, its 5 folders, the artist, the album twice (under
    // its artist and under Albums), the genre, 2 playlists, 2
    // library folders and 4 directories.
    assert_eq!(seen, 18);
}
