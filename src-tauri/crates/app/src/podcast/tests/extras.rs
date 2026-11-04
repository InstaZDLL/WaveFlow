//! Chapters and transcripts against servers that misbehave on purpose, on the really
//! migrated schema with foreign keys on.

use std::path::Path;
use std::sync::atomic::AtomicI64;

use waveflow_core::podcast::chapters::ChapterSource;
use waveflow_core::podcast::feed::parse_feed;
use waveflow_core::podcast::{MAX_CHAPTERS_BYTES, MAX_TRANSCRIPT_BYTES};

use super::super::extras::{chapters, transcript, EpisodeChapters};
use super::*;

const FEED: &str = "https://feeds.example.org/feed";

const CHAPTER_FILE: &str = r#"{"version":"1.2.0","chapters":[
    {"startTime":60,"title":"Second","img":"https://t.example/i.png","url":"https://t.example/"},
    {"startTime":0,"title":"First"},
    {"startTime":30,"title":"Hidden","toc":false},
    {"startTime":-5,"title":"Before the start"},
    {"startTime":1e12,"title":"After the end"}
]}"#;

const VTT: &str = "WEBVTT\n\n00:01.000 --> 00:02.000\n<v Ana>Hello <b>there</b>\n\n\
                   00:03.000 --> 00:04.000\nSecond line\n";

const SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nFrom SRT\n";

/// Where a row's optional sources are written.
#[derive(Default, Clone)]
struct Row {
    chapters_url: Option<String>,
    inline: Option<String>,
    enclosure: Option<String>,
    enclosure_type: Option<String>,
    transcripts: Option<String>,
    feed: Option<String>,
}

async fn row(pool: &SqlitePool, row: Row) -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    let key = NEXT.fetch_add(1, Ordering::SeqCst).to_string();
    let channel: i64 = sqlx::query_scalar(
        "INSERT INTO podcast_channel (feed_key, feed_url, title, created_at, updated_at)
         VALUES (?, ?, 'A channel', 0, 0) RETURNING id",
    )
    .bind(&key)
    .bind(row.feed.as_deref().unwrap_or(FEED))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO podcast_episode
            (channel_id, item_key, enclosure_url, enclosure_type, title, chapters_url,
             inline_chapters, transcripts, first_seen_at, updated_at)
         VALUES (?, ?, ?, ?, 'An episode', ?, ?, ?, 0, 0) RETURNING id",
    )
    .bind(channel)
    .bind(&key)
    .bind(row.enclosure)
    .bind(row.enclosure_type)
    .bind(row.chapters_url)
    .bind(row.inline)
    .bind(row.transcripts)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn titles(answer: &EpisodeChapters) -> Vec<&str> {
    answer.chapters.iter().map(|c| c.title.as_str()).collect()
}

async fn open_chapters(
    pool: &SqlitePool,
    fetcher: &Fetcher,
    episode: i64,
) -> Result<EpisodeChapters, PodcastError> {
    chapters(
        &TestLeases(pool.clone()),
        fetcher,
        Path::new("/nowhere"),
        episode,
    )
    .await
}

async fn cached(pool: &SqlitePool, episode: i64) -> (Option<String>, Option<i64>) {
    sqlx::query_as("SELECT chapters_cache, chapters_checked_at FROM podcast_episode WHERE id = ?")
        .bind(episode)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn offline_fetcher(server: &Server) -> Fetcher {
    Fetcher::with(
        client_builder()
            .no_proxy()
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], server.port).into()),
        || true,
        ROOMY,
    )
    .unwrap()
}

fn synchsafe(value: usize) -> [u8; 4] {
    [
        ((value >> 21) & 0x7f) as u8,
        ((value >> 14) & 0x7f) as u8,
        ((value >> 7) & 0x7f) as u8,
        (value & 0x7f) as u8,
    ]
}

/// The start of an MP3 whose tag holds one chapter called `title`.
fn tagged_mp3(title: &str) -> Vec<u8> {
    let frame = |id: &[u8], body: &[u8]| {
        let mut out = id.to_vec();
        out.extend_from_slice(&synchsafe(body.len()));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(body);
        out
    };
    let mut text = vec![3_u8];
    text.extend_from_slice(title.as_bytes());
    let mut chapter = b"c\0".to_vec();
    chapter.extend_from_slice(&5_000_u32.to_be_bytes());
    chapter.extend_from_slice(&6_000_u32.to_be_bytes());
    chapter.extend_from_slice(&[0xff; 8]);
    chapter.extend(frame(b"TIT2", &text));
    let frames = frame(b"CHAP", &chapter);
    let mut out = b"ID3\x04\0\0".to_vec();
    out.extend_from_slice(&synchsafe(frames.len()));
    out.extend(frames);
    out
}

fn json_reply(body: &str) -> Reply {
    with_status(
        "200 OK",
        "Content-Type: application/json\r\n",
        body.as_bytes(),
    )
}

// ── chapters ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_chapter_file_is_fetched_once_kept_and_stripped_of_what_it_should_not_say() {
    let server = serve(|_| json_reply(CHAPTER_FILE)).await;
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            ..Row::default()
        },
    )
    .await;
    let fetcher = public_fetcher(&server);

    let first = open_chapters(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(first.source, Some(ChapterSource::Json));
    assert_eq!(titles(&first), ["First", "Second"]);
    assert_eq!(first.chapters[1].start_ms, 60_000);
    assert_eq!(server.hits(), 1);

    // Nobody is asked again, whether the player, the page or the menu asks.
    let second = open_chapters(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(second, first);
    assert_eq!(server.hits(), 1);
    // Nothing of a chapter's image or link was kept.
    let (cache, _) = cached(&pool, episode).await;
    assert!(!cache.unwrap().contains("t.example"));
}

async fn set_chapters_url(pool: &SqlitePool, episode: i64, address: &str) {
    sqlx::query("UPDATE podcast_episode SET chapters_url = ? WHERE id = ?")
        .bind(address)
        .bind(episode)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_first_source_with_a_chapter_wins_and_they_are_not_merged() {
    let inline = r#"[{"start_ms":0,"title":"Inline"}]"#;
    let server = serve(|line| {
        if line.contains("/empty.json") {
            json_reply(r#"{"chapters":[{"startTime":3,"title":""}]}"#)
        } else if line.contains("/c.json") {
            json_reply(CHAPTER_FILE)
        } else {
            with_status(
                "200 OK",
                "Content-Type: audio/mpeg\r\n",
                &tagged_mp3("From the tag"),
            )
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let audio = server.public("/a.mp3");

    // The file, over both others.
    let all = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            inline: Some(inline.into()),
            enclosure: Some(audio.clone()),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, all).await.unwrap();
    assert_eq!(answer.source, Some(ChapterSource::Json));
    assert_eq!(titles(&answer), ["First", "Second"]);

    // A file with nothing valid moves on. The enclosure is not asked for what is
    // already known.
    let hits = server.hits();
    let empty = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/empty.json")),
            inline: Some(inline.into()),
            enclosure: Some(audio.clone()),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, empty).await.unwrap();
    assert_eq!(answer.source, Some(ChapterSource::Inline));
    assert_eq!(titles(&answer), ["Inline"]);
    assert_eq!(server.hits(), hits + 1);

    // Nothing above it: the tag, from the start of the stream.
    let tagged = row(
        &pool,
        Row {
            enclosure: Some(audio.clone()),
            enclosure_type: Some("audio/mpeg".into()),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, tagged).await.unwrap();
    assert_eq!(answer.source, Some(ChapterSource::Id3));
    assert_eq!(titles(&answer), ["From the tag"]);
    assert_eq!(answer.chapters[0].start_ms, 5_000);
    let request = server.seen().pop().unwrap().to_lowercase();
    assert!(
        request.contains("range: bytes=0-1048575"),
        "asked for no more than it reads: {request}"
    );
    // Kept: the enclosure is not read twice.
    let hits = server.hits();
    let again = open_chapters(&pool, &fetcher, tagged).await.unwrap();
    assert_eq!(titles(&again), ["From the tag"]);
    assert_eq!(server.hits(), hits);

    // Another format is not read for a tag it cannot hold.
    let hits = server.hits();
    let aac = row(
        &pool,
        Row {
            enclosure: Some(server.public("/a.m4a")),
            enclosure_type: Some("audio/mp4".into()),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, aac).await.unwrap();
    assert_eq!(answer.chapters, Vec::new());
    assert_eq!(answer.failure, None);
    assert_eq!(server.hits(), hits);
}

#[tokio::test]
async fn a_host_that_sends_the_whole_file_is_stopped_at_the_bound() {
    // Starts like an MP3 and never ends. Reading it to the end would take for ever,
    // and the deadline is not what stops this.
    let server = serve(|_| Reply::AudioFlood).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            enclosure: Some(server.public("/a.mp3")),
            ..Row::default()
        },
    )
    .await;
    let started = std::time::Instant::now();
    let answer = open_chapters(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(answer.chapters, Vec::new());
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn a_public_feeds_chapter_file_cannot_lead_to_the_local_network() {
    let private = serve(|_| json_reply(CHAPTER_FILE)).await;
    let local = format!("http://127.0.0.1:{}/c.json", private.port);
    let redirecting = local.clone();
    let server = serve(move |line| {
        if line.contains("/hop.json") {
            redirect(&redirecting)
        } else {
            json_reply(CHAPTER_FILE)
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;

    for address in [server.public("/hop.json"), local] {
        let episode = row(
            &pool,
            Row {
                chapters_url: Some(address),
                ..Row::default()
            },
        )
        .await;
        let answer = open_chapters(&pool, &fetcher, episode).await.unwrap();
        assert_eq!(answer.chapters, Vec::new());
        assert_eq!(answer.failure.as_deref(), Some("address_refused"));
    }
    assert_eq!(private.hits(), 0, "the private address was contacted");
}

#[tokio::test]
async fn a_chapter_file_that_is_too_large_or_no_file_at_all_is_refused_in_a_word() {
    let huge = vec![b' '; MAX_CHAPTERS_BYTES + 1];
    let lying = {
        // Declares little, and sends more than the bound in chunks.
        let mut body = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        let chunk = vec![b' '; 64 * 1024];
        for _ in 0..20 {
            body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            body.extend_from_slice(&chunk);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"0\r\n\r\n");
        Reply::Raw(body)
    };
    let server = serve(move |line| {
        if line.contains("/huge") {
            with_status("200 OK", "", &huge)
        } else if line.contains("/lying") {
            lying.clone()
        } else if line.contains("/page") {
            with_status(
                "200 OK",
                "Content-Type: text/html\r\n",
                b"<!doctype html><title>nope</title>",
            )
        } else if line.contains("/deep") {
            json_reply(&"[".repeat(200_000))
        } else {
            json_reply(r#"{"chapters":"none"}"#)
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    for (path, word) in [
        ("/huge", "too_large"),
        ("/lying", "too_large"),
        ("/page", "invalid_document"),
        ("/deep", "invalid_document"),
        ("/shape", "invalid_document"),
    ] {
        let episode = row(
            &pool,
            Row {
                chapters_url: Some(server.public(path)),
                ..Row::default()
            },
        )
        .await;
        let answer = open_chapters(&pool, &fetcher, episode).await.unwrap();
        assert_eq!(answer.chapters, Vec::new(), "{path}");
        assert_eq!(answer.failure.as_deref(), Some(word), "{path}");
    }
}

#[tokio::test]
async fn a_failure_that_says_something_holds_and_one_that_does_not_is_asked_again() {
    let server = serve(|line| {
        if line.contains("/gone") {
            with_status("404 Not Found", "", b"")
        } else {
            with_status("503 Service Unavailable", "", b"")
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;

    let gone = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/gone")),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, gone).await.unwrap();
    assert_eq!(answer.failure.as_deref(), Some("http_404"));
    assert!(cached(&pool, gone).await.1.is_some());
    let hits = server.hits();
    let again = open_chapters(&pool, &fetcher, gone).await.unwrap();
    assert_eq!(again.failure, None, "an answer that holds: there are none");
    assert_eq!(server.hits(), hits);

    // A day later it is worth asking again.
    sqlx::query("UPDATE podcast_episode SET chapters_checked_at = 0 WHERE id = ?")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
    open_chapters(&pool, &fetcher, gone).await.unwrap();
    assert_eq!(server.hits(), hits + 1);

    let down = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/down")),
            ..Row::default()
        },
    )
    .await;
    let answer = open_chapters(&pool, &fetcher, down).await.unwrap();
    assert_eq!(answer.failure.as_deref(), Some("http_503"));
    assert_eq!(cached(&pool, down).await, (None, None), "nothing to hold");
    let hits = server.hits();
    open_chapters(&pool, &fetcher, down).await.unwrap();
    assert_eq!(server.hits(), hits + 1);
}

#[tokio::test]
async fn off_or_offline_asks_nobody_and_reads_what_is_here() {
    let server = serve(|_| json_reply(CHAPTER_FILE)).await;
    let online_fetcher = public_fetcher(&server);
    let offline = offline_fetcher(&server);
    let inline = r#"[{"start_ms":0,"title":"Inline"}]"#;

    let pool = pool().await;
    switch(&pool, false).await;
    let with_inline = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            inline: Some(inline.into()),
            enclosure: Some(server.public("/a.mp3")),
            ..Row::default()
        },
    )
    .await;
    let bare = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            enclosure: Some(server.public("/a.mp3")),
            ..Row::default()
        },
    )
    .await;

    // Off.
    let answer = open_chapters(&pool, &online_fetcher, with_inline)
        .await
        .unwrap();
    assert_eq!(titles(&answer), ["Inline"]);
    let answer = open_chapters(&pool, &online_fetcher, bare).await.unwrap();
    assert_eq!(answer.failure.as_deref(), Some("disabled"));

    // On, and offline.
    switch(&pool, true).await;
    let answer = open_chapters(&pool, &offline, with_inline).await.unwrap();
    assert_eq!(titles(&answer), ["Inline"]);
    let answer = open_chapters(&pool, &offline, bare).await.unwrap();
    assert_eq!(answer.failure.as_deref(), Some("offline"));
    assert_eq!(server.hits(), 0, "somebody was contacted");

    // Neither answer was kept: a better source has not been asked.
    assert_eq!(cached(&pool, with_inline).await, (None, None));
    assert_eq!(cached(&pool, bare).await, (None, None));

    // Asked once on, it stays readable off and offline.
    let answer = open_chapters(&pool, &online_fetcher, bare).await.unwrap();
    assert_eq!(answer.source, Some(ChapterSource::Json));
    assert_eq!(server.hits(), 1);
    let off_pool_answer = open_chapters(&pool, &offline, bare).await.unwrap();
    assert_eq!(titles(&off_pool_answer), ["First", "Second"]);
    switch(&pool, false).await;
    let off_answer = open_chapters(&pool, &online_fetcher, bare).await.unwrap();
    assert_eq!(titles(&off_answer), ["First", "Second"]);
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn a_downloaded_files_tag_needs_nobody_whatever_the_switches_say() {
    let server = serve(|_| Reply::Silence).await;
    let pool = pool().await;
    switch(&pool, false).await;
    let episode = row(
        &pool,
        Row {
            enclosure: Some(server.public("/a.mp3")),
            ..Row::default()
        },
    )
    .await;
    let folder = tempfile::tempdir().unwrap();
    let name = format!("{episode}.mp3");
    std::fs::write(folder.path().join(&name), tagged_mp3("In the file")).unwrap();
    sqlx::query("UPDATE podcast_episode SET download_file = ? WHERE id = ?")
        .bind(&name)
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();

    let answer = chapters(
        &TestLeases(pool.clone()),
        &offline_fetcher(&server),
        folder.path(),
        episode,
    )
    .await
    .unwrap();
    assert_eq!(answer.source, Some(ChapterSource::Id3));
    assert_eq!(titles(&answer), ["In the file"]);
    assert_eq!(server.hits(), 0);

    // Another episode's name in the column opens nothing, whatever is on the disk.
    sqlx::query(
        "UPDATE podcast_episode SET download_file = ?, chapters_cache = NULL,
                chapters_checked_at = NULL WHERE id = ?",
    )
    .bind(format!("{}.mp3", episode + 1))
    .bind(episode)
    .execute(&pool)
    .await
    .unwrap();
    std::fs::write(
        folder.path().join(format!("{}.mp3", episode + 1)),
        tagged_mp3("Not this one"),
    )
    .unwrap();
    let answer = chapters(
        &TestLeases(pool.clone()),
        &offline_fetcher(&server),
        folder.path(),
        episode,
    )
    .await
    .unwrap();
    assert_eq!(answer.chapters, Vec::new());
}

#[tokio::test]
async fn a_cache_that_is_empty_or_not_ours_is_found_again() {
    let server = serve(|_| json_reply(CHAPTER_FILE)).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            ..Row::default()
        },
    )
    .await;

    for stored in [
        Some("not a cache"),
        Some(""),
        Some(r#"{"source":"json","chapters":[]}"#),
        // What an import leaves.
        None,
    ] {
        // Written just now: the stamp must not make garbage an answer.
        let stamp = stored.map(|_| chrono::Utc::now().timestamp_millis());
        sqlx::query(
            "UPDATE podcast_episode SET chapters_cache = ?, chapters_checked_at = ? WHERE id = ?",
        )
        .bind(stored)
        .bind(stamp)
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
        let hits = server.hits();
        let answer = open_chapters(&pool, &fetcher, episode).await.unwrap();
        assert_eq!(titles(&answer), ["First", "Second"], "{stored:?}");
        assert_eq!(server.hits(), hits + 1, "{stored:?}");
    }

    // Hostile, and shaped right: it is put back in order, and drawn as text anyway.
    sqlx::query("UPDATE podcast_episode SET chapters_cache = ? WHERE id = ?")
        .bind(r#"{"source":"json","chapters":[{"start_ms":9,"title":"B"},{"start_ms":1,"title":"A"}]}"#)
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
    let hits = server.hits();
    let answer = open_chapters(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(titles(&answer), ["A", "B"]);
    assert_eq!(server.hits(), hits);
}

#[tokio::test]
async fn an_episode_the_user_removed_is_gone() {
    let server = serve(|_| json_reply(CHAPTER_FILE)).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            transcripts: Some("[]".into()),
            ..Row::default()
        },
    )
    .await;
    sqlx::query("UPDATE podcast_episode SET deleted_at = 1 WHERE id = ?")
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        open_chapters(&pool, &fetcher, episode).await,
        Err(PodcastError::EpisodeGone)
    ));
    assert!(matches!(
        transcript(&TestLeases(pool.clone()), &fetcher, episode).await,
        Err(PodcastError::EpisodeGone)
    ));
    assert!(store::episode(&pool, episode).await.unwrap().is_none());
    assert_eq!(server.hits(), 0);
}

// ── what a refresh does to what was kept ────────────────────────────

fn feed_with(chapters: &str, transcript: &str) -> waveflow_core::podcast::Feed {
    let body = format!(
        r#"<?xml version="1.0"?>
        <rss version="2.0" xmlns:podcast="https://podcastindex.org/namespace/1.0">
        <channel><title>Show</title>
          <item><guid>one</guid><title>One</title>
            <enclosure url="https://cdn.example.org/one.mp3" type="audio/mpeg" length="1"/>
            <podcast:chapters url="{chapters}" type="application/json+chapters"/>
            <podcast:transcript url="{transcript}" type="text/vtt"/>
          </item>
        </channel></rss>"#
    );
    parse_feed(body.as_bytes(), &url(FEED)).unwrap()
}

#[tokio::test]
async fn a_feed_that_names_other_sources_makes_what_was_kept_stale() {
    let pool = enabled_pool().await;
    let feed = feed_with(
        "https://x.example.org/c.json",
        "https://x.example.org/t.vtt",
    );
    let channel = store::apply_feed(&pool, "key", FEED, &feed, &Validators::default())
        .await
        .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM podcast_episode WHERE channel_id = ?")
        .bind(channel)
        .fetch_one(&pool)
        .await
        .unwrap();
    let keep = || async {
        sqlx::query(
            "UPDATE podcast_episode SET chapters_cache = 'c', chapters_checked_at = 1,
                    transcript_cache = 't', transcript_kind = 'vtt', transcript_checked_at = 1
              WHERE id = ?",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    };
    let kept = || async {
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT chapters_cache, transcript_cache FROM podcast_episode WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    // The same feed again keeps them.
    keep().await;
    store::refresh_feed(&pool, channel, &feed, &Validators::default())
        .await
        .unwrap();
    assert_eq!(kept().await, (Some("c".into()), Some("t".into())));

    // Another chapter file drops the chapters and only them.
    let moved = feed_with(
        "https://x.example.org/other.json",
        "https://x.example.org/t.vtt",
    );
    store::refresh_feed(&pool, channel, &moved, &Validators::default())
        .await
        .unwrap();
    assert_eq!(kept().await, (None, Some("t".into())));

    // Another transcript drops the transcript and only it.
    keep().await;
    let moved = feed_with(
        "https://x.example.org/other.json",
        "https://x.example.org/other.vtt",
    );
    store::refresh_feed(&pool, channel, &moved, &Validators::default())
        .await
        .unwrap();
    assert_eq!(kept().await, (Some("c".into()), None));
}

#[tokio::test]
async fn a_refresh_during_a_fetch_is_not_undone_by_what_it_found() {
    // The row names another file by the time the first one's answer is written.
    let pool = enabled_pool().await;
    let server = serve(|_| Reply::Late(json_body(CHAPTER_FILE))).await;
    let fetcher = public_fetcher(&server);
    let episode = row(
        &pool,
        Row {
            chapters_url: Some(server.public("/c.json")),
            ..Row::default()
        },
    )
    .await;
    let opening = {
        let pool = pool.clone();
        tokio::spawn(async move { open_chapters(&pool, &fetcher, episode).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    set_chapters_url(&pool, episode, "https://x.example.org/other.json").await;
    let answer = opening.await.unwrap().unwrap();
    assert_eq!(titles(&answer), ["First", "Second"]);
    assert_eq!(cached(&pool, episode).await, (None, None));
}

fn json_body(body: &str) -> Vec<u8> {
    match json_reply(body) {
        Reply::Raw(bytes) => bytes,
        _ => unreachable!(),
    }
}

// ── transcripts ─────────────────────────────────────────────────────

fn refs(entries: &[(&str, &str)]) -> Option<String> {
    let list: Vec<serde_json::Value> = entries
        .iter()
        .map(
            |(kind, address)| serde_json::json!({ "url": address, "kind": kind, "language": null }),
        )
        .collect();
    Some(serde_json::Value::Array(list).to_string())
}

async fn open_transcript(
    pool: &SqlitePool,
    fetcher: &Fetcher,
    episode: i64,
) -> Result<super::super::extras::EpisodeTranscript, PodcastError> {
    transcript(&TestLeases(pool.clone()), fetcher, episode).await
}

fn word(err: PodcastError) -> String {
    match err {
        PodcastError::Disabled => "disabled".into(),
        PodcastError::Offline => "offline".into(),
        PodcastError::Failed(failure) => failure.code(),
        other => format!("{other:?}"),
    }
}

async fn kept_transcript(pool: &SqlitePool, episode: i64) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT transcript_kind, transcript_cache FROM podcast_episode WHERE id = ?")
        .bind(episode)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_best_transcript_is_fetched_once_kept_and_has_no_markup_left() {
    let server = serve(|line| {
        if line.contains(".vtt") {
            with_status("200 OK", "Content-Type: text/vtt\r\n", VTT.as_bytes())
        } else {
            with_status("200 OK", "", SRT.as_bytes())
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&[
                ("srt", &server.public("/t.srt")),
                ("vtt", &server.public("/t.vtt")),
            ]),
            ..Row::default()
        },
    )
    .await;

    let answer = open_transcript(&pool, &fetcher, episode).await.unwrap();
    assert!(answer.available && answer.timed);
    assert_eq!(answer.lines.len(), 2);
    assert_eq!(answer.lines[0].text, "Hello there");
    assert_eq!(answer.lines[0].speaker.as_deref(), Some("Ana"));
    assert_eq!(answer.lines[0].start_ms, Some(1000));
    assert_eq!(server.hits(), 1);

    let again = open_transcript(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(again, answer);
    assert_eq!(server.hits(), 1);
    let (kind, text) = kept_transcript(&pool, episode).await;
    assert_eq!(kind.as_deref(), Some("vtt"));
    assert!(text.unwrap().starts_with("WEBVTT"));
}

#[tokio::test]
async fn a_false_transcript_gives_way_to_the_next_and_only_an_honest_one_is_kept() {
    let page = "<!doctype html><title>404</title><script>x()</script>";
    let server = serve(move |line| {
        if line.contains(".vtt") {
            with_status("200 OK", "Content-Type: text/vtt\r\n", page.as_bytes())
        } else if line.contains(".json") {
            with_status("200 OK", "", b"{\"segments\":\"no\"}")
        } else {
            with_status("200 OK", "", SRT.as_bytes())
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&[
                ("transcript_json", &server.public("/t.json")),
                ("vtt", &server.public("/t.vtt")),
                ("srt", &server.public("/t.srt")),
            ]),
            ..Row::default()
        },
    )
    .await;
    let answer = open_transcript(&pool, &fetcher, episode).await.unwrap();
    assert_eq!(answer.lines[0].text, "From SRT");
    assert_eq!(
        server.hits(),
        2,
        "the best, then the next: {:?}",
        server.seen()
    );
    assert_eq!(
        kept_transcript(&pool, episode).await.0.as_deref(),
        Some("srt")
    );

    // Only liars: one word, and nothing kept.
    let liars = row(
        &pool,
        Row {
            transcripts: refs(&[
                ("vtt", &server.public("/a.vtt")),
                ("transcript_json", &server.public("/a.json")),
            ]),
            ..Row::default()
        },
    )
    .await;
    let refused = open_transcript(&pool, &fetcher, liars).await.unwrap_err();
    assert_eq!(word(refused), "invalid_document");
    assert_eq!(kept_transcript(&pool, liars).await, (None, None));
}

#[tokio::test]
async fn a_transcript_is_bounded_and_cannot_lead_to_the_local_network() {
    let private = serve(|_| with_status("200 OK", "", VTT.as_bytes())).await;
    let local = format!("http://127.0.0.1:{}/t.vtt", private.port);
    let redirecting = local.clone();
    let huge = vec![b'x'; MAX_TRANSCRIPT_BYTES + 1];
    let server = serve(move |line| {
        if line.contains("/hop") {
            redirect(&redirecting)
        } else if line.contains("/huge") {
            with_status("200 OK", "", &huge)
        } else {
            Reply::Flood
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    for (address, expected) in [
        (server.public("/hop"), "address_refused"),
        (local, "address_refused"),
        ("ftp://example.org/t.vtt".to_owned(), "address_refused"),
        (server.public("/huge"), "too_large"),
        (server.public("/flood"), "too_large"),
    ] {
        let episode = row(
            &pool,
            Row {
                transcripts: refs(&[("vtt", &address)]),
                ..Row::default()
            },
        )
        .await;
        let refused = open_transcript(&pool, &fetcher, episode).await.unwrap_err();
        assert_eq!(word(refused), expected);
    }
    assert_eq!(private.hits(), 0, "the private address was contacted");
}

#[tokio::test]
async fn a_feed_naming_many_transcripts_gets_a_few_requests() {
    let server = serve(|_| with_status("404 Not Found", "", b"")).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let addresses: Vec<String> = (0..16)
        .map(|index| server.public(&format!("/{index}.vtt")))
        .collect();
    let entries: Vec<(&str, &str)> = addresses.iter().map(|a| ("vtt", a.as_str())).collect();
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&entries),
            ..Row::default()
        },
    )
    .await;
    let refused = open_transcript(&pool, &fetcher, episode).await.unwrap_err();
    assert_eq!(word(refused), "http_404");
    assert_eq!(server.hits(), 4);
}

#[tokio::test]
async fn off_or_offline_asks_nobody_for_a_transcript_and_what_was_kept_stays() {
    let server = serve(|_| with_status("200 OK", "", VTT.as_bytes())).await;
    let online_fetcher = public_fetcher(&server);
    let offline = offline_fetcher(&server);
    let pool = pool().await;
    switch(&pool, false).await;
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&[("vtt", &server.public("/t.vtt"))]),
            ..Row::default()
        },
    )
    .await;

    let refused = open_transcript(&pool, &online_fetcher, episode)
        .await
        .unwrap_err();
    assert_eq!(word(refused), "disabled");
    switch(&pool, true).await;
    let refused = open_transcript(&pool, &offline, episode).await.unwrap_err();
    assert_eq!(word(refused), "offline");
    assert_eq!(server.hits(), 0);

    open_transcript(&pool, &online_fetcher, episode)
        .await
        .unwrap();
    assert_eq!(server.hits(), 1);
    assert_eq!(
        open_transcript(&pool, &offline, episode)
            .await
            .unwrap()
            .lines
            .len(),
        2
    );
    switch(&pool, false).await;
    assert_eq!(
        open_transcript(&pool, &online_fetcher, episode)
            .await
            .unwrap()
            .lines
            .len(),
        2
    );
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn a_kept_transcript_that_does_not_read_is_fetched_again() {
    let server = serve(|_| with_status("200 OK", "", VTT.as_bytes())).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&[("vtt", &server.public("/t.vtt"))]),
            ..Row::default()
        },
    )
    .await;
    for (kind, stored) in [
        (Some("vtt"), Some("<html>not a transcript")),
        (Some("vtt"), Some("WEBVTT\n\n")),
        (Some("something"), Some(VTT)),
        (None, Some(VTT)),
        (Some("vtt"), None),
        // What an import leaves.
        (None, None),
    ] {
        sqlx::query(
            "UPDATE podcast_episode SET transcript_kind = ?, transcript_cache = ? WHERE id = ?",
        )
        .bind(kind)
        .bind(stored)
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
        let hits = server.hits();
        let answer = open_transcript(&pool, &fetcher, episode).await.unwrap();
        assert_eq!(answer.lines.len(), 2, "{kind:?} {stored:?}");
        assert_eq!(server.hits(), hits + 1, "{kind:?} {stored:?}");
    }
}

#[tokio::test]
async fn an_episode_without_a_transcript_says_so_and_asks_nobody() {
    let server = serve(|_| Reply::Silence).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    for transcripts in [None, Some("[]".to_owned()), Some("not json".to_owned())] {
        let episode = row(
            &pool,
            Row {
                transcripts,
                ..Row::default()
            },
        )
        .await;
        let answer = open_transcript(&pool, &fetcher, episode).await.unwrap();
        assert!(!answer.available);
    }
    assert_eq!(server.hits(), 0);
}

#[tokio::test]
async fn an_untimed_transcript_says_it_cannot_follow_and_holds_text_only() {
    let html = "<html><body><p>One <b>line</b></p><script>steal()</script><p>Two</p></body></html>";
    let server = serve(move |_| with_status("200 OK", "", html.as_bytes())).await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let episode = row(
        &pool,
        Row {
            transcripts: refs(&[("html", &server.public("/t.html"))]),
            ..Row::default()
        },
    )
    .await;
    let answer = open_transcript(&pool, &fetcher, episode).await.unwrap();
    assert!(answer.available && !answer.timed);
    let text: Vec<&str> = answer.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(text, ["One line", "Two"]);
    assert!(answer.lines.iter().all(|line| line.start_ms.is_none()));
}

/// Everything a failure can say, with a token in every address: no word of it.
#[tokio::test]
async fn an_address_is_in_no_answer_and_no_word() {
    const SECRET: &str = "s3cr3t-t0ken";
    let server = serve(|line| {
        if line.contains("/hop") {
            redirect("http://127.0.0.1:9/private")
        } else if line.contains("/ok") {
            json_reply(CHAPTER_FILE)
        } else {
            with_status("500 Oops", "", b"")
        }
    })
    .await;
    let fetcher = public_fetcher(&server);
    let pool = enabled_pool().await;
    let mut outputs = Vec::new();
    for path in ["/hop", "/ok", "/broken"] {
        let address = server.public(&format!("{path}/{SECRET}?auth={SECRET}"));
        let episode = row(
            &pool,
            Row {
                chapters_url: Some(address.clone()),
                transcripts: refs(&[("vtt", &address)]),
                ..Row::default()
            },
        )
        .await;
        outputs.push(format!(
            "{:?}",
            open_chapters(&pool, &fetcher, episode).await
        ));
        outputs.push(format!(
            "{:?}",
            open_transcript(&pool, &fetcher, episode).await
        ));
        let row = store::episode(&pool, episode).await.unwrap().unwrap();
        outputs.push(serde_json::to_string(&row).unwrap());
    }
    for output in outputs {
        assert!(!output.contains(SECRET), "{output}");
    }
}
