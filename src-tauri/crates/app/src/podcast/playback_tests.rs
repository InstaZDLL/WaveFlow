//! Playing an episode: a stream under the fetch rules, and what a session writes,
//! on the really migrated schema with foreign keys on.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use symphonia::core::io::MediaSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use url::Url;
use waveflow_core::playback::read_ahead::READ_RELEASED;
use waveflow_core::podcast::FetchFailure;

use super::fetch::{client_builder, Fetcher, Origin};
use super::playback::{
    channel_speed, counts_as_played, downloaded_file, ext_hint_for_tests, resolve_source,
    start_position, EpisodeSource, SaveClock, SaveReason, SAVE_EVERY,
};
use super::refresh::PodcastError;
use super::store;
use super::stream::{EnclosureSource, Gate, ReadGuard, StreamRefusal};
use super::tests::{
    enabled_pool, fetcher, offline, online, public_fetcher, redirect, serve, tls_redirecting_to,
    url, with_status, PUBLIC_HOST,
};
use crate::audio::state::{PlayerState, SharedPlayback};

// ── a server with a file on it ──────────────────────────────────────

/// How a [`FileServer`] misbehaves.
#[derive(Clone, Copy, Default)]
struct Quirks {
    /// Answers every request with the whole file and a 200, saying nothing of
    /// ranges.
    no_ranges: bool,
    /// A 206 for the first request, then a 200 from the first byte.
    forgets_ranges: bool,
    /// Closes the first connection after this many bytes of body.
    cut_first_after: Option<usize>,
}

struct FileServer {
    port: u16,
    /// The start of each request's range, in order; `None` for no range.
    requests: Arc<Mutex<Vec<Option<u64>>>>,
    sent: Arc<AtomicU64>,
}

impl FileServer {
    fn address(&self) -> String {
        format!("http://127.0.0.1:{}/episode.mp3", self.port)
    }

    fn seen(&self) -> Vec<Option<u64>> {
        self.requests.lock().unwrap().clone()
    }
}

/// `length` bytes that say where they are: byte `n` is `n % 251`.
fn pattern(length: usize) -> Arc<Vec<u8>> {
    Arc::new((0..length).map(|at| (at % 251) as u8).collect())
}

async fn serve_file(body: Arc<Vec<u8>>, quirks: Quirks) -> FileServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sent = Arc::new(AtomicU64::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    let (seen, counted) = (Arc::clone(&requests), Arc::clone(&sent));
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (body, seen, counted, served) = (
                Arc::clone(&body),
                Arc::clone(&seen),
                Arc::clone(&counted),
                Arc::clone(&served),
            );
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let asked = head
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|value| value.trim().trim_end_matches('-').parse::<u64>().ok());
                seen.lock().unwrap().push(asked);
                let turn = served.fetch_add(1, Ordering::SeqCst);
                let total = body.len();
                let ranged = !quirks.no_ranges && (!quirks.forgets_ranges || turn == 0);
                let start = if ranged {
                    asked.unwrap_or(0) as usize
                } else {
                    0
                };
                let status = if ranged && asked.is_some() {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{total}\r\n",
                        total - 1
                    )
                } else {
                    "HTTP/1.1 200 OK\r\n".to_owned()
                };
                let answer = format!(
                    "{status}Content-Type: audio/mpeg\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    total - start
                );
                if stream.write_all(answer.as_bytes()).await.is_err() {
                    return;
                }
                let mut remaining = &body[start..];
                let mut allowance = if turn == 0 {
                    quirks.cut_first_after
                } else {
                    None
                };
                while !remaining.is_empty() {
                    let mut take = remaining.len().min(16 * 1024);
                    if let Some(left) = allowance.as_mut() {
                        take = take.min(*left);
                        *left -= take;
                    }
                    if take == 0 {
                        return;
                    }
                    if stream.write_all(&remaining[..take]).await.is_err() {
                        return;
                    }
                    counted.fetch_add(take as u64, Ordering::SeqCst);
                    remaining = &remaining[take..];
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    FileServer {
        port,
        requests,
        sent,
    }
}

// ── the stream ──────────────────────────────────────────────────────

fn open_gate() -> Gate {
    Arc::new(|| Box::pin(async { Ok(()) }))
}

fn local_origin() -> Origin {
    Origin::of(&url("http://nas.local/feed"))
}

/// Opens on a blocking thread, as the decoder's is: `open` waits on the runtime.
async fn open(
    fetcher: Fetcher,
    gate: Gate,
    address: Url,
    origin: Origin,
) -> Result<EnclosureSource, StreamRefusal> {
    let runtime = Handle::current();
    tokio::task::spawn_blocking(move || {
        EnclosureSource::open(runtime, fetcher, gate, address, origin)
    })
    .await
    .unwrap()
}

async fn open_local(server: &FileServer) -> EnclosureSource {
    let source = open(
        fetcher(),
        open_gate(),
        url(&server.address()),
        local_origin(),
    )
    .await;
    source.unwrap_or_else(|refusal| panic!("refused: {refusal:?}"))
}

/// Runs what the decoder would do with the source, on a thread of its own.
async fn on_decoder<T: Send + 'static>(
    mut source: EnclosureSource,
    work: impl FnOnce(&mut EnclosureSource) -> T + Send + 'static,
) -> (EnclosureSource, T) {
    tokio::task::spawn_blocking(move || {
        let out = work(&mut source);
        (source, out)
    })
    .await
    .unwrap()
}

fn read_exactly(source: &mut EnclosureSource, length: usize) -> std::io::Result<Vec<u8>> {
    let mut out = vec![0u8; length];
    source.read_exact(&mut out)?;
    Ok(out)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_seek_is_a_range_request_and_lands_on_the_right_bytes() {
    let body = pattern(64 * 1024 * 1024);
    let server = serve_file(Arc::clone(&body), Quirks::default()).await;
    let source = open_local(&server).await;
    assert!(source.is_seekable());
    assert_eq!(source.byte_len(), Some(body.len() as u64));

    let far = 48 * 1024 * 1024;
    let expected = body[far..far + 4096].to_vec();
    let head = body[..4096].to_vec();
    let (source, _) = on_decoder(source, move |source| {
        assert_eq!(read_exactly(source, 4096).unwrap(), head);
        assert_eq!(
            source.seek(SeekFrom::Start(far as u64)).unwrap(),
            far as u64
        );
        assert_eq!(read_exactly(source, 4096).unwrap(), expected);
    })
    .await;

    assert_eq!(server.seen(), vec![Some(0), Some(far as u64)]);
    assert_eq!(source.requests(), 2);
    drop(source);
    // The whole point of a range: 64 MiB is not sent to hear 8 KiB of it, and the
    // 48 MiB skipped are not sent at all. Loose, since the sockets of two connections
    // hold what the queue does not, several MiB each on Linux.
    assert!(
        server.sent.load(Ordering::SeqCst) < 32 * 1024 * 1024,
        "{} bytes sent",
        server.sent.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_short_jump_forward_reads_through_rather_than_asking_again() {
    let body = pattern(4 * 1024 * 1024);
    let server = serve_file(Arc::clone(&body), Quirks::default()).await;
    let source = open_local(&server).await;
    let jump = 100 * 1024;
    let expected = body[jump..jump + 1024].to_vec();
    let (_source, _) = on_decoder(source, move |source| {
        source.seek(SeekFrom::Start(jump as u64)).unwrap();
        assert_eq!(read_exactly(source, 1024).unwrap(), expected);
    })
    .await;
    assert_eq!(server.seen(), vec![Some(0)]);
}

/// Where an MP4 keeps its index: a legal place to land, and no request can ask for
/// it, since `bytes=<len>-` has no byte to answer with.
#[tokio::test(flavor = "multi_thread")]
async fn the_end_of_the_file_is_reached_without_a_request() {
    let body = pattern(1024 * 1024);
    let total = body.len() as u64;
    let server = serve_file(Arc::clone(&body), Quirks::default()).await;
    let source = open_local(&server).await;
    let expected = body[1000..2000].to_vec();
    let (_source, _) = on_decoder(source, move |source| {
        assert_eq!(source.seek(SeekFrom::End(0)).unwrap(), total);
        assert_eq!(source.read(&mut [0u8; 64]).unwrap(), 0);
        assert_eq!(source.seek(SeekFrom::Start(1000)).unwrap(), 1000);
        assert_eq!(read_exactly(source, 1000).unwrap(), expected);
    })
    .await;
    assert_eq!(server.seen(), vec![Some(0), Some(1000)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_that_answers_no_range_is_read_once_through() {
    let body = pattern(256 * 1024);
    let server = serve_file(
        Arc::clone(&body),
        Quirks {
            no_ranges: true,
            ..Quirks::default()
        },
    )
    .await;
    let source = open_local(&server).await;
    assert!(!source.is_seekable());
    assert_eq!(source.byte_len(), None);
    let whole = Arc::clone(&body);
    let (_source, _) = on_decoder(source, move |source| {
        let mut out = Vec::new();
        source.read_to_end(&mut out).unwrap();
        assert_eq!(&out, whole.as_ref());
        assert!(source.seek(SeekFrom::Start(0)).is_err());
    })
    .await;
    assert_eq!(server.seen().len(), 1);
}

/// A 200 to a seek is the file from its first byte, which would play the wrong
/// place. The seek is refused, and what was playing goes on.
#[tokio::test(flavor = "multi_thread")]
async fn a_seek_answered_from_the_first_byte_is_refused_and_playback_goes_on() {
    let body = pattern(4 * 1024 * 1024);
    let server = serve_file(
        Arc::clone(&body),
        Quirks {
            forgets_ranges: true,
            ..Quirks::default()
        },
    )
    .await;
    let source = open_local(&server).await;
    let expected = body[1024..2048].to_vec();
    let (_source, _) = on_decoder(source, move |source| {
        read_exactly(source, 1024).unwrap();
        let err = source
            .seek(SeekFrom::Start(3 * 1024 * 1024))
            .expect_err("a 200 is not a seek");
        assert_eq!(err.to_string(), "podcast: http_200");
        assert_eq!(read_exactly(source, 1024).unwrap(), expected);
    })
    .await;
}

/// A long pause outlives the connection. The read that finds out takes it up again
/// where it stopped.
#[tokio::test(flavor = "multi_thread")]
async fn a_body_that_breaks_off_is_taken_up_where_it_stopped() {
    let body = pattern(1024 * 1024);
    let cut = 100 * 1024;
    let server = serve_file(
        Arc::clone(&body),
        Quirks {
            cut_first_after: Some(cut),
            ..Quirks::default()
        },
    )
    .await;
    let source = open_local(&server).await;
    let whole = Arc::clone(&body);
    let (_source, _) = on_decoder(source, move |source| {
        let mut out = Vec::new();
        source.read_to_end(&mut out).unwrap();
        assert!(out == *whole, "the bytes around the break differ");
    })
    .await;
    let seen = server.seen();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[1], Some(cut as u64));
}

/// A redirect out of a public feed into a private address is refused before the
/// private server hears anything, by the stream as by the feed.
#[tokio::test(flavor = "multi_thread")]
async fn a_public_enclosure_cannot_redirect_to_a_private_address() {
    let private = serve(|_| with_status("200 OK", "", b"secret")).await;
    let hop = private.local("/admin");
    let public = serve(move |_| redirect(&hop)).await;
    let address = url(&public.public("/episode.mp3"));
    let refusal = open(
        public_fetcher(&public),
        open_gate(),
        address.clone(),
        Origin::of(&address),
    )
    .await
    .err();
    assert_eq!(
        refusal,
        Some(StreamRefusal::Failed(FetchFailure::AddressRefused))
    );
    assert_eq!(public.hits(), 1);
    assert_eq!(private.hits(), 0, "the private server was contacted");
}

/// A public feed naming a private enclosure outright: the rule is the feed's, not
/// the address's own.
#[tokio::test(flavor = "multi_thread")]
async fn a_public_feed_cannot_name_a_private_enclosure() {
    let private = serve(|_| with_status("200 OK", "", b"secret")).await;
    let refusal = open(
        fetcher(),
        open_gate(),
        url(&private.local("/episode.mp3")),
        Origin::of(&url(&format!("https://{PUBLIC_HOST}/feed"))),
    )
    .await
    .err();
    assert_eq!(
        refusal,
        Some(StreamRefusal::Failed(FetchFailure::AddressRefused))
    );
    assert_eq!(private.hits(), 0, "the private server was contacted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_secure_enclosure_cannot_redirect_into_the_clear() {
    let clear = serve(|_| with_status("200 OK", "", b"audio")).await;
    let port = tls_redirecting_to(format!("http://{PUBLIC_HOST}:{}/clear", clear.port)).await;
    let fetcher = Fetcher::with(
        client_builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], port).into()),
        online,
        Duration::from_secs(5),
    )
    .unwrap();
    let address = url(&format!("https://{PUBLIC_HOST}:{port}/episode.mp3"));
    let refusal = open(fetcher, open_gate(), address.clone(), Origin::of(&address))
        .await
        .err();
    assert_eq!(
        refusal,
        Some(StreamRefusal::Failed(FetchFailure::AddressRefused))
    );
    assert_eq!(clear.hits(), 0, "the clear server was contacted");
}

#[tokio::test(flavor = "multi_thread")]
async fn more_than_five_redirects_are_refused() {
    let server = serve(|_| redirect("/again")).await;
    let refusal = open(
        fetcher(),
        open_gate(),
        url(&server.local("/episode.mp3")),
        local_origin(),
    )
    .await
    .err();
    assert_eq!(
        refusal,
        Some(StreamRefusal::Failed(FetchFailure::TooManyRedirects))
    );
    assert_eq!(server.hits(), 6, "the first request, and five hops");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_web_page_in_place_of_audio_is_refused() {
    let server = serve(|_| {
        with_status(
            "200 OK",
            "Content-Type: text/html; charset=utf-8\r\n",
            b"<html>sign in</html>",
        )
    })
    .await;
    let refusal = open(
        fetcher(),
        open_gate(),
        url(&server.local("/episode.mp3")),
        local_origin(),
    )
    .await
    .err();
    assert_eq!(refusal, Some(StreamRefusal::Failed(FetchFailure::NotAudio)));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_switch_off_and_offline_mode_refuse_before_anything_is_contacted() {
    let server = serve_file(pattern(1024), Quirks::default()).await;

    let off: Gate = Arc::new(|| Box::pin(async { Err(StreamRefusal::Disabled) }));
    let refusal = open(fetcher(), off, url(&server.address()), local_origin())
        .await
        .err();
    assert_eq!(refusal, Some(StreamRefusal::Disabled));

    let offline_fetcher =
        Fetcher::with(client_builder().no_proxy(), offline, Duration::from_secs(1)).unwrap();
    let refusal = open(
        offline_fetcher,
        open_gate(),
        url(&server.address()),
        local_origin(),
    )
    .await
    .err();
    assert_eq!(refusal, Some(StreamRefusal::Offline));

    assert_eq!(
        server.seen(),
        Vec::<Option<u64>>::new(),
        "somebody was contacted"
    );
}

/// The gate is asked again for every request, not only the first: a switch turned
/// off while an episode plays holds for its next seek.
#[tokio::test(flavor = "multi_thread")]
async fn a_seek_after_the_switch_went_off_is_refused_and_contacts_nobody() {
    let switch = Arc::new(AtomicBool::new(true));
    let read = Arc::clone(&switch);
    let gate: Gate = Arc::new(move || {
        let on = read.load(Ordering::SeqCst);
        Box::pin(async move {
            if on {
                Ok(())
            } else {
                Err(StreamRefusal::Disabled)
            }
        })
    });
    let server = serve_file(pattern(4 * 1024 * 1024), Quirks::default()).await;
    let source = open(fetcher(), gate, url(&server.address()), local_origin())
        .await
        .unwrap();
    switch.store(false, Ordering::SeqCst);
    let (_source, _) = on_decoder(source, |source| {
        let err = source.seek(SeekFrom::Start(3 * 1024 * 1024)).unwrap_err();
        assert_eq!(err.to_string(), "podcast: disabled");
    })
    .await;
    assert_eq!(server.seen().len(), 1);
}

/// Offline mode turned on stops the transfer where it is, and the read that finds
/// out cannot open another.
#[tokio::test(flavor = "multi_thread")]
async fn offline_mode_turned_on_mid_stream_stops_it() {
    static OFFLINE: AtomicBool = AtomicBool::new(false);
    fn is_offline() -> bool {
        OFFLINE.load(Ordering::SeqCst)
    }
    let body = pattern(32 * 1024 * 1024);
    let server = serve_file(body, Quirks::default()).await;
    let fetcher = Fetcher::with(
        client_builder().no_proxy(),
        is_offline,
        Duration::from_secs(5),
    )
    .unwrap();
    let source = open(fetcher, open_gate(), url(&server.address()), local_origin())
        .await
        .unwrap();
    let (_source, _) = on_decoder(source, |source| {
        read_exactly(source, 1024).unwrap();
        OFFLINE.store(true, Ordering::SeqCst);
        let mut sink = vec![0u8; 1024 * 1024];
        let err = loop {
            match source.read(&mut sink) {
                Ok(0) => panic!("a stream stopped short is not the end of the file"),
                Ok(_) => {}
                Err(err) => break err,
            }
        };
        assert_eq!(err.to_string(), "podcast: offline");
    })
    .await;
    assert!(server.sent.load(Ordering::SeqCst) < 32 * 1024 * 1024);
}

/// A host that goes quiet holds the decoder in a read. A command queued for it lets
/// go once the wait is old enough to call stalled, far short of the stall itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_read_lets_go_when_a_command_asks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer).await;
        let _ = stream
            .write_all(
                b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-99999/100000\r\n\
                  Content-Type: audio/mpeg\r\nContent-Length: 100000\r\n\r\nabc",
            )
            .await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    });
    let source = open(
        fetcher(),
        open_gate(),
        url(&format!("http://127.0.0.1:{port}/episode.mp3")),
        local_origin(),
    )
    .await
    .unwrap();
    let guard = source.release_handle();
    let asking = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        guard.request();
    });
    let started = Instant::now();
    let (_source, err) = on_decoder(source, |source| {
        let mut sink = [0u8; 16];
        // What had already arrived, then the wait.
        assert_eq!(source.read(&mut sink).unwrap(), 3);
        source.read(&mut sink).unwrap_err()
    })
    .await;
    asking.await.unwrap();
    assert_eq!(err.to_string(), READ_RELEASED);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited {:?}",
        started.elapsed()
    );
}

/// A command may yet be dropped as stale, and a healthy stream must survive that: a
/// read that gets its bytes in time is not let go.
#[tokio::test(flavor = "multi_thread")]
async fn a_young_read_is_left_alone_when_a_command_asks() {
    let body = pattern(1024);
    let server = serve_file(Arc::clone(&body), Quirks::default()).await;
    let source = open_local(&server).await;
    let guard: Arc<ReadGuard> = source.release_handle();
    guard.request();
    let expected = body.to_vec();
    let (_source, _) = on_decoder(source, move |source| {
        assert_eq!(read_exactly(source, 1024).unwrap(), expected);
    })
    .await;
}

#[test]
fn a_type_gives_the_probe_a_hint_and_anything_else_gives_none() {
    assert_eq!(ext_hint_for_tests(Some("audio/mpeg")), Some("mp3"));
    assert_eq!(
        ext_hint_for_tests(Some("Audio/MP4; codecs=mp4a")),
        Some("m4a")
    );
    assert_eq!(ext_hint_for_tests(Some("application/octet-stream")), None);
    assert_eq!(ext_hint_for_tests(None), None);
}

// ── where an episode's bytes come from ──────────────────────────────

async fn channel(pool: &SqlitePool, key: &str, feed_url: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO podcast_channel (feed_key, feed_url, title, created_at, updated_at)
         VALUES (?, ?, 'The Show', 0, 0) RETURNING id",
    )
    .bind(key)
    .bind(feed_url)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A row, with what a test cares about.
#[derive(Clone, Copy, Default)]
struct Row<'a> {
    key: &'a str,
    published_at: Option<i64>,
    duration_ms: Option<i64>,
    enclosure: Option<&'a str>,
    download: Option<&'a str>,
    position_ms: i64,
    played_at: Option<i64>,
    last_opened_at: Option<i64>,
    deleted_at: Option<i64>,
}

async fn episode(pool: &SqlitePool, channel_id: i64, row: Row<'_>) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO podcast_episode
            (channel_id, item_key, title, published_at, duration_ms, enclosure_url, download_file,
             position_ms, played_at, last_opened_at, deleted_at, first_seen_at, updated_at)
         VALUES (?, ?, 'An episode', ?, ?, ?, ?, ?, ?, ?, ?, 0, 0) RETURNING id",
    )
    .bind(channel_id)
    .bind(row.key)
    .bind(row.published_at)
    .bind(row.duration_ms)
    .bind(row.enclosure)
    .bind(row.download)
    .bind(row.position_ms)
    .bind(row.played_at)
    .bind(row.last_opened_at)
    .bind(row.deleted_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn playable(pool: &SqlitePool, id: i64) -> store::PlayableEpisode {
    store::playable(pool, id).await.unwrap().unwrap()
}

const ENCLOSURE: &str = "https://cdn.example.org/s3cret/ep.mp3?auth=s3cret";

fn word(result: Result<EpisodeSource, PodcastError>) -> String {
    match result {
        Ok(_) => "played".to_owned(),
        Err(err) => crate::error::AppError::from(err).to_string(),
    }
}

/// A file on disk needs nobody, so neither switch reaches it. Without one, each of
/// them refuses, and the enclosure is not even looked at.
#[tokio::test]
async fn a_downloaded_file_plays_whatever_the_switches_say_and_nothing_else_does() {
    let pool = enabled_pool().await;
    let dir = tempfile::tempdir().unwrap();
    let ch = channel(&pool, "a", Some("https://feeds.example.org/feed")).await;
    let on_disk = episode(
        &pool,
        ch,
        Row {
            key: "disk",
            enclosure: Some(ENCLOSURE),
            ..Row::default()
        },
    )
    .await;
    let name = format!("{on_disk}.mp3");
    std::fs::write(dir.path().join(&name), b"audio").unwrap();
    store_download(&pool, on_disk, &name).await;
    let on_disk = playable(&pool, on_disk).await;
    for (enabled, is_offline) in [(true, false), (false, false), (true, true), (false, true)] {
        let source = resolve_source(&on_disk, dir.path(), enabled, is_offline).unwrap();
        assert!(
            matches!(&source, EpisodeSource::File(path) if path == &dir.path().join(&name)),
            "switch {enabled}, offline {is_offline}"
        );
    }

    // Named, and gone from the disk: back to being a request.
    let gone = episode(
        &pool,
        ch,
        Row {
            key: "gone",
            enclosure: Some(ENCLOSURE),
            ..Row::default()
        },
    )
    .await;
    store_download(&pool, gone, &format!("{gone}.mp3")).await;
    let gone = playable(&pool, gone).await;
    assert!(word(resolve_source(&gone, dir.path(), false, false)).ends_with("podcast: disabled"));
    assert!(word(resolve_source(&gone, dir.path(), true, true)).ends_with("podcast: offline"));
    assert!(matches!(
        resolve_source(&gone, dir.path(), true, false),
        Ok(EpisodeSource::Stream { .. })
    ));

    // A restored profile: no file and no address.
    let bare = playable(
        &pool,
        episode(
            &pool,
            ch,
            Row {
                key: "bare",
                ..Row::default()
            },
        )
        .await,
    )
    .await;
    assert!(word(resolve_source(&bare, dir.path(), true, false)).ends_with("podcast: no_source"));
}

async fn store_download(pool: &SqlitePool, id: i64, name: &str) {
    sqlx::query("UPDATE podcast_episode SET download_file = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// `download_file` may have come out of an archive somebody made. Whatever it says,
/// the row that names anything but this episode's file in the managed folder opens
/// nothing there and falls back to streaming, every real file being in reach of the
/// names below.
#[tokio::test]
async fn a_download_name_out_of_an_archive_is_hostile() {
    let pool = enabled_pool().await;
    let dir = tempfile::tempdir().unwrap();
    let inside = dir.path().join("inside");
    std::fs::create_dir(&inside).unwrap();
    let ch = channel(&pool, "a", Some("https://feeds.example.org/feed")).await;
    let id = episode(
        &pool,
        ch,
        Row {
            key: "a",
            enclosure: Some(ENCLOSURE),
            ..Row::default()
        },
    )
    .await;
    let own = format!("{id}.mp3");
    let other = format!("{}.mp3", id + 1);
    std::fs::write(inside.join(&own), b"audio").unwrap();
    std::fs::write(inside.join(&other), b"somebody else's").unwrap();
    std::fs::write(dir.path().join(&own), b"outside").unwrap();
    assert_eq!(
        downloaded_file(&inside, id, &own),
        Some(inside.join(&own)),
        "this test proves nothing if its own file does not open"
    );

    let absolute = dir.path().join(&own);
    let absolute = absolute.to_str().unwrap();
    let names = [
        format!("../{own}"),
        format!("..\\{own}"),
        format!("sub/{own}"),
        format!("sub\\{own}"),
        format!("{id}.mp3/../{own}"),
        format!("{id}./{own}"),
        // Another episode's, in the same folder.
        other,
        // An alternate data stream, and what Windows quietly trims.
        format!("{own}:hidden"),
        format!("{own}."),
        format!("{own} "),
        // A drive prefix, a share, a locator.
        format!("C:{own}"),
        format!("C:\\{own}"),
        format!("\\\\server\\share\\{own}"),
        format!("wfnet://1/{own}"),
        absolute.to_owned(),
        // No name to speak of.
        String::new(),
        ".".to_owned(),
        "..".to_owned(),
        id.to_string(),
        format!("{id}."),
        ".mp3".to_owned(),
    ];
    for name in names {
        assert_eq!(downloaded_file(&inside, id, &name), None, "{name:?}");
        store_download(&pool, id, &name).await;
        let row = playable(&pool, id).await;
        // Off, a hostile name refuses instead of opening anything.
        assert!(
            word(resolve_source(&row, &inside, false, false)).ends_with("podcast: disabled"),
            "{name:?}"
        );
        // On, it streams, as if there were no file.
        assert!(
            matches!(
                resolve_source(&row, &inside, true, false),
                Ok(EpisodeSource::Stream { .. })
            ),
            "{name:?}"
        );
    }
}

/// A stream's rule comes from the feed's address, and an enclosure with none behind
/// it gets the stricter one.
#[tokio::test]
async fn a_streams_origin_comes_from_the_channel() {
    let pool = enabled_pool().await;
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        ("public", Some("https://feeds.example.org/feed"), true),
        ("local", Some("http://nas.local/feed"), false),
        ("blank", None, true),
    ];
    for (key, feed, public) in cases {
        let ch = channel(&pool, key, feed).await;
        let id = episode(
            &pool,
            ch,
            Row {
                key,
                enclosure: Some(ENCLOSURE),
                ..Row::default()
            },
        )
        .await;
        match resolve_source(&playable(&pool, id).await, dir.path(), true, false) {
            Ok(EpisodeSource::Stream { origin, .. }) => {
                assert_eq!(origin.is_public(), public, "{key}");
            }
            _ => panic!("{key} did not resolve to a stream"),
        }
    }
}

// ── resume ──────────────────────────────────────────────────────────

#[test]
fn an_episode_counts_as_played_past_95_percent_or_within_its_last_30_seconds() {
    let hour = 3_600_000;
    assert!(!counts_as_played(hour * 94 / 100, hour));
    assert!(counts_as_played(hour * 95 / 100, hour));
    assert!(counts_as_played(hour, hour));
    // Five minutes: 95 % is 15 s from the end, so the 30 s come first.
    let short = 300_000;
    assert!(!counts_as_played(269_999, short));
    assert!(counts_as_played(270_000, short));
    // Shorter than the 30 s: any position is in them.
    assert!(counts_as_played(0, 20_000));
    // An unknown length says nothing.
    assert!(!counts_as_played(hour, 0));
}

#[tokio::test]
async fn a_played_episode_starts_over_and_any_other_from_its_position() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let mut cases = Vec::new();
    for (key, position, played, duration, expected) in [
        ("fresh", 0, None, Some(600_000), 0),
        ("part", 120_000, None, Some(600_000), 120_000),
        ("played", 120_000, Some(1), Some(600_000), 0),
        ("no-length", 120_000, None, None, 120_000),
        // A position in the last two seconds is a track that was finished.
        ("tail", 599_000, None, Some(600_000), 0),
    ] {
        let id = episode(
            &pool,
            ch,
            Row {
                key,
                position_ms: position,
                played_at: played,
                duration_ms: duration,
                ..Row::default()
            },
        )
        .await;
        cases.push((key, id, expected));
    }
    for (key, id, expected) in cases {
        assert_eq!(
            start_position(&playable(&pool, id).await),
            expected,
            "{key}"
        );
    }
}

#[tokio::test]
async fn saving_a_position_turns_played_once_and_forgets_it() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let id = episode(
        &pool,
        ch,
        Row {
            key: "a",
            duration_ms: Some(600_000),
            ..Row::default()
        },
    )
    .await;
    assert!(!store::save_position(&pool, id, 60_000, false)
        .await
        .unwrap());
    let row = playable(&pool, id).await;
    assert_eq!((row.position_ms, row.played_at), (60_000, None));

    assert!(store::save_position(&pool, id, 590_000, true)
        .await
        .unwrap());
    let row = playable(&pool, id).await;
    assert_eq!(row.position_ms, 0);
    let first = row.played_at.expect("it was played");

    // Heard again from the top, it stays played, and says nothing new.
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(!store::save_position(&pool, id, 590_000, true)
        .await
        .unwrap());
    assert_eq!(playable(&pool, id).await.played_at, Some(first));
    assert!(!store::save_position(&pool, id, 30_000, false)
        .await
        .unwrap());
    assert_eq!(playable(&pool, id).await.played_at, Some(first));

    // A row that went away while it played.
    assert!(!store::save_position(&pool, id + 999, 1, true)
        .await
        .unwrap());
}

#[tokio::test]
async fn marking_by_hand_sets_the_state_and_forgets_the_position() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let other = channel(&pool, "b", None).await;
    let one = episode(
        &pool,
        ch,
        Row {
            key: "1",
            position_ms: 5_000,
            ..Row::default()
        },
    )
    .await;
    let two = episode(
        &pool,
        ch,
        Row {
            key: "2",
            ..Row::default()
        },
    )
    .await;
    let removed = episode(
        &pool,
        ch,
        Row {
            key: "3",
            deleted_at: Some(1),
            ..Row::default()
        },
    )
    .await;
    let elsewhere = episode(
        &pool,
        other,
        Row {
            key: "4",
            position_ms: 7_000,
            ..Row::default()
        },
    )
    .await;

    assert_eq!(
        store::mark_played(&pool, one, true).await.unwrap(),
        Some(ch)
    );
    let row = playable(&pool, one).await;
    assert!(row.played_at.is_some());
    assert_eq!(row.position_ms, 0);
    assert_eq!(
        store::mark_played(&pool, one, false).await.unwrap(),
        Some(ch)
    );
    assert_eq!(playable(&pool, one).await.played_at, None);
    assert_eq!(
        store::mark_played(&pool, one + 999, true).await.unwrap(),
        None
    );
    assert_eq!(
        store::mark_played(&pool, removed, true).await.unwrap(),
        None
    );

    assert_eq!(
        store::mark_channel_played(&pool, ch, true).await.unwrap(),
        2
    );
    assert!(playable(&pool, one).await.played_at.is_some());
    assert!(playable(&pool, two).await.played_at.is_some());
    let untouched = playable(&pool, elsewhere).await;
    assert_eq!((untouched.position_ms, untouched.played_at), (7_000, None));
    assert_eq!(
        store::mark_channel_played(&pool, ch, false).await.unwrap(),
        2
    );
    assert_eq!(playable(&pool, two).await.played_at, None);
}

/// One per channel, the one in progress or else the newest unplayed, the channels
/// with one in progress first. Nothing here makes a request.
#[tokio::test]
async fn to_listen_keeps_one_episode_per_channel() {
    let pool = enabled_pool().await;
    let (a, b, c, d) = (
        channel(&pool, "a", None).await,
        channel(&pool, "b", None).await,
        channel(&pool, "c", None).await,
        channel(&pool, "d", None).await,
    );
    let row = |key, published| Row {
        key,
        published_at: Some(published),
        enclosure: Some(ENCLOSURE),
        ..Row::default()
    };
    // Channel a: the newest is unplayed, an older one in progress wins.
    episode(&pool, a, row("a-new", 300)).await;
    let a_progress = episode(
        &pool,
        a,
        Row {
            position_ms: 1_000,
            last_opened_at: Some(10),
            ..row("a-old", 100)
        },
    )
    .await;
    // Channel b: the newest unplayed, past a played one and a removed one.
    episode(
        &pool,
        b,
        Row {
            played_at: Some(1),
            ..row("b-played", 500)
        },
    )
    .await;
    episode(
        &pool,
        b,
        Row {
            deleted_at: Some(1),
            ..row("b-removed", 400)
        },
    )
    .await;
    let b_new = episode(&pool, b, row("b-new", 200)).await;
    episode(&pool, b, row("b-older", 150)).await;
    // Channel c: everything heard, so nothing to listen to.
    episode(
        &pool,
        c,
        Row {
            played_at: Some(1),
            ..row("c", 100)
        },
    )
    .await;
    // Channel d: a restored profile's row has no address and cannot play, but a
    // downloaded one can.
    episode(
        &pool,
        d,
        Row {
            enclosure: None,
            ..row("d-bare", 900)
        },
    )
    .await;
    let d_disk = episode(
        &pool,
        d,
        Row {
            enclosure: None,
            download: Some("1.mp3"),
            ..row("d-disk", 50)
        },
    )
    .await;

    let listed: Vec<i64> = store::to_listen(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|episode| episode.id)
        .collect();
    assert_eq!(listed, vec![a_progress, b_new, d_disk]);
}

/// A queue lives in memory: the end of an episode passes over what was heard since,
/// and what went away since, and stops only on something to hear.
#[tokio::test]
async fn the_end_of_an_episode_passes_over_what_there_is_nothing_to_hear_in() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let row = |key| Row {
        key,
        enclosure: Some(ENCLOSURE),
        ..Row::default()
    };
    let new = episode(&pool, ch, row("new")).await;
    let part = episode(
        &pool,
        ch,
        Row {
            position_ms: 5_000,
            ..row("part")
        },
    )
    .await;
    let heard = episode(
        &pool,
        ch,
        Row {
            played_at: Some(1),
            ..row("heard")
        },
    )
    .await;
    let removed = episode(
        &pool,
        ch,
        Row {
            deleted_at: Some(1),
            ..row("removed")
        },
    )
    .await;
    assert!(!store::nothing_to_hear(&pool, new).await.unwrap());
    assert!(!store::nothing_to_hear(&pool, part).await.unwrap());
    assert!(store::nothing_to_hear(&pool, heard).await.unwrap());
    assert!(store::nothing_to_hear(&pool, removed).await.unwrap());
    assert!(store::nothing_to_hear(&pool, removed + 999).await.unwrap());
}

/// Episode rows go with their channel, and a session has nothing left to write to.
#[tokio::test]
async fn forgetting_a_channel_takes_its_episodes_and_its_speed() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let id = episode(
        &pool,
        ch,
        Row {
            key: "a",
            ..Row::default()
        },
    )
    .await;
    assert!(store::set_channel_speed(&pool, ch, Some(1.5))
        .await
        .unwrap());
    assert_eq!(playable(&pool, id).await.playback_speed, Some(1.5));
    assert!(store::set_channel_speed(&pool, ch, None).await.unwrap());
    assert_eq!(playable(&pool, id).await.playback_speed, None);
    assert!(store::delete_channel(&pool, ch).await.unwrap());
    assert!(store::playable(&pool, id).await.unwrap().is_none());
    assert!(!store::set_channel_speed(&pool, ch, Some(1.5))
        .await
        .unwrap());
}

/// What a session writes touches two tables of its own. The second half is the
/// other line of defence: were an episode's negative id ever to reach the analytics
/// writer, no `track` row answers it, so no play and no scrobble come of it either.
#[tokio::test]
async fn an_episode_leaves_the_play_events_and_the_scrobble_queue_empty() {
    let pool = enabled_pool().await;
    let ch = channel(&pool, "a", None).await;
    let id = episode(
        &pool,
        ch,
        Row {
            key: "a",
            duration_ms: Some(600_000),
            enclosure: Some(ENCLOSURE),
            ..Row::default()
        },
    )
    .await;
    store::note_opened(&pool, id).await.unwrap();
    store::save_position(&pool, id, 60_000, false)
        .await
        .unwrap();
    store::save_position(&pool, id, 600_000, true)
        .await
        .unwrap();
    store::mark_played(&pool, id, false).await.unwrap();
    crate::audio::analytics::insert_play_event(
        &pool,
        crate::commands::player::next_radio_track_id(),
        600_000,
        true,
        super::playback::SOURCE_TYPE,
        None,
    )
    .await;
    for table in ["play_event", "scrobble_queue"] {
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
}

// ── when a position is written ──────────────────────────────────────

#[test]
fn a_position_is_due_at_a_pause_at_a_seek_and_every_15_seconds() {
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let mut clock = SaveClock::default();

    // An episode just opened is where its row says.
    assert_eq!(clock.due(1, PlayerState::Playing, 7, at(0)), None);
    assert_eq!(clock.due(1, PlayerState::Playing, 7, at(14)), None);
    assert_eq!(
        clock.due(1, PlayerState::Playing, 7, at(SAVE_EVERY.as_secs())),
        Some(SaveReason::Tick)
    );
    // The clock starts over from a write, whatever caused it.
    assert_eq!(clock.due(1, PlayerState::Playing, 7, at(20)), None);

    assert_eq!(
        clock.due(1, PlayerState::Playing, 8, at(21)),
        Some(SaveReason::Seeked)
    );
    assert_eq!(clock.due(1, PlayerState::Playing, 8, at(22)), None);

    assert_eq!(
        clock.due(1, PlayerState::Paused, 8, at(23)),
        Some(SaveReason::Paused)
    );
    // Once per pause, and a paused player does not move.
    assert_eq!(clock.due(1, PlayerState::Paused, 8, at(24)), None);
    assert_eq!(clock.due(1, PlayerState::Paused, 8, at(500)), None);
    // A seek while paused is still a seek.
    assert_eq!(
        clock.due(1, PlayerState::Paused, 9, at(501)),
        Some(SaveReason::Seeked)
    );

    // Not this episode's to read: the decoder reports where it was left.
    for state in [PlayerState::Loading, PlayerState::Idle, PlayerState::Ended] {
        assert_eq!(clock.due(1, state, 10, at(900)), None, "{state:?}");
    }

    // Another episode starts its own clock.
    assert_eq!(clock.due(2, PlayerState::Playing, 11, at(901)), None);
    assert_eq!(clock.due(2, PlayerState::Playing, 11, at(910)), None);
}

// ── speed ───────────────────────────────────────────────────────────

#[test]
fn null_is_the_normal_speed_and_nothing_stored_gets_out_of_range() {
    assert_eq!(channel_speed(None), 1.0);
    assert_eq!(channel_speed(Some(1.5)), 1.5);
    assert_eq!(channel_speed(Some(9.0)), 2.0);
    assert_eq!(channel_speed(Some(0.1)), 0.5);
    assert_eq!(channel_speed(Some(f64::NAN)), 1.0);
}

/// Every way out of an episode ends in [`SharedPlayback::release_episode_speed`],
/// and what plays next finds the user's speed in place.
#[test]
fn an_episode_holds_its_speed_and_gives_the_users_back() {
    let shared = SharedPlayback::new();
    shared.restore_user_speed(1.25);
    assert_eq!(shared.playback_speed(), 1.25);

    shared.hold_episode_speed(1.5);
    assert_eq!(shared.playback_speed(), 1.5);

    shared.release_episode_speed();
    assert_eq!(shared.playback_speed(), 1.25);
}

/// The user's slider has no business with an episode's speed, and the other way
/// round: each waits for its own.
#[test]
fn the_two_speeds_do_not_reach_each_other() {
    let shared = SharedPlayback::new();
    shared.restore_user_speed(1.25);
    shared.hold_episode_speed(1.5);

    // Changed during the episode: heard after it.
    shared.set_user_speed(0.75);
    assert_eq!(shared.playback_speed(), 1.5);
    shared.set_episode_speed(2.0);
    assert_eq!(shared.playback_speed(), 2.0);

    shared.release_episode_speed();
    assert_eq!(shared.playback_speed(), 0.75);

    // Crossed the end of the episode: dropped, not applied to what plays next.
    shared.set_episode_speed(2.0);
    assert_eq!(shared.playback_speed(), 0.75);

    // And the user's own, with no episode, is heard at once.
    shared.set_user_speed(1.1);
    assert_eq!(shared.playback_speed(), 1.1);
}

/// `player_get_state` restores every setting, and a window mounting during an
/// episode calls it: that must not take the episode's speed away.
#[test]
fn a_settings_restore_during_an_episode_keeps_its_speed() {
    let shared = SharedPlayback::new();
    shared.hold_episode_speed(1.5);
    shared.restore_user_speed(1.25);
    assert_eq!(shared.playback_speed(), 1.5);
    shared.release_episode_speed();
    assert_eq!(shared.playback_speed(), 1.25);
}
