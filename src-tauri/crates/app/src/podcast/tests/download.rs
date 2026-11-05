//! Downloads against servers that misbehave on purpose, on the really migrated
//! schema with foreign keys on, into a real folder.

use std::path::Path;
use std::sync::atomic::AtomicI64;

use tokio::sync::watch;

use super::super::download::{
    delete_download, download_episode, record_in_place, stored_file, sweep, DownloadObserver,
    DownloadState, DownloadUpdate, Ended, Job, Pace, Pushed, Queue, Swept, EPISODE_GONE,
};
use super::*;

const SECURE_FEED: &str = "https://feeds.example.org/feed";

/// The start of an MP3, then `size` bytes in all.
fn audio(size: usize) -> Vec<u8> {
    let mut bytes = b"ID3\x04\0\0\0\0\0\0\0\0\0\0".to_vec();
    bytes.resize(size.max(bytes.len()), 0);
    bytes
}

fn audio_reply(size: usize) -> Reply {
    with_status("200 OK", "Content-Type: audio/mpeg\r\n", &audio(size))
}

/// Run short, so a test waits for what it means to and no longer.
fn quick() -> Pace {
    Pace {
        gate_every: Duration::from_millis(20),
        progress_every: Duration::from_millis(10),
        ..Pace::standard()
    }
}

/// A channel at `feed` and one episode at `enclosure`: `(channel, episode)`.
async fn episode_at(pool: &SqlitePool, feed: Option<&str>, enclosure: &str) -> (i64, i64) {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    let key = NEXT.fetch_add(1, Ordering::SeqCst).to_string();
    let channel: i64 = sqlx::query_scalar(
        "INSERT INTO podcast_channel (feed_key, feed_url, title, created_at, updated_at)
         VALUES (?, ?, 'A channel', 0, 0) RETURNING id",
    )
    .bind(&key)
    .bind(feed)
    .fetch_one(pool)
    .await
    .unwrap();
    let episode: i64 = sqlx::query_scalar(
        "INSERT INTO podcast_episode
            (channel_id, item_key, enclosure_url, title, first_seen_at, updated_at)
         VALUES (?, ?, ?, 'An episode', 0, 0) RETURNING id",
    )
    .bind(channel)
    .bind(&key)
    .bind(enclosure)
    .fetch_one(pool)
    .await
    .unwrap();
    (channel, episode)
}

/// What a row says of its file.
async fn recorded(pool: &SqlitePool, episode: i64) -> (Option<String>, Option<i64>, Option<i64>) {
    sqlx::query_as(
        "SELECT download_file, download_size, downloaded_at FROM podcast_episode WHERE id = ?",
    )
    .bind(episode)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The names in the folder, sorted. A folder never created holds nothing.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[derive(Default)]
struct Seen {
    updates: Mutex<Vec<DownloadUpdate>>,
}

impl DownloadObserver for Seen {
    fn update(&self, update: DownloadUpdate) {
        self.updates.lock().unwrap().push(update);
    }
}

async fn run(
    leases: &impl Leases,
    fetcher: &Fetcher,
    dir: &Path,
    episode: i64,
    pace: Pace,
) -> Ended {
    let (_keep, cancel) = watch::channel(false);
    download_episode(
        leases,
        fetcher,
        dir,
        episode,
        pace,
        cancel,
        &Seen::default(),
    )
    .await
}

fn word(failure: FetchFailure) -> Ended {
    Ended::Failed(failure.code())
}

fn folder() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Waits for a partial file to appear, which says the body is being read.
async fn wait_for_partial(dir: &Path, episode: i64) {
    let partial = dir.join(format!("{episode}.part"));
    for _ in 0..500 {
        if partial.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no partial file appeared");
}

// ── what lands on the disk ──────────────────────────────────────────

#[tokio::test]
async fn an_episode_lands_under_its_id_and_a_suffix_from_its_bytes() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    // A feed that names a path gets no say: the address holds a traversal and an
    // extension that is not what the bytes are.
    let address = server.local("/..%2F..%2Fevil.exe?name=../../evil.exe");
    let (channel, episode) = episode_at(&pool, Some(&server.local("/feed")), &address).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, Ended::Stored { size: Some(4096) });
    let name = format!("{episode}.mp3");
    assert_eq!(names_in(dir.path()), std::slice::from_ref(&name));
    assert_eq!(std::fs::read(dir.path().join(&name)).unwrap(), audio(4096));
    let (file, size, at) = recorded(&pool, episode).await;
    assert_eq!(file.as_deref(), Some(name.as_str()));
    assert_eq!(size, Some(4096));
    assert!(at.is_some());
    let row = store::list_episodes(&pool, channel, 10, 0).await.unwrap();
    assert!(row[0].downloaded, "the page reads it as downloaded");
    assert_eq!(row[0].download_size, Some(4096));
}

#[tokio::test]
async fn the_suffix_follows_the_bytes_and_a_body_split_in_small_chunks_still_counts() {
    let server = serve(|_| {
        // One byte per chunk: the sniff must wait for what it needs.
        let mut out = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for byte in b"\0\0\0\x20ftypM4A \0\0\0\0" {
            out.extend_from_slice(b"1\r\n");
            out.push(*byte);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        Reply::Raw(out)
    })
    .await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, Ended::Stored { size: Some(16) });
    assert_eq!(names_in(dir.path()), [format!("{episode}.m4a")]);
}

#[tokio::test]
async fn html_in_place_of_audio_leaves_nothing_at_all() {
    let server = serve(|_| ok("<!doctype html><html><body>Sign in to listen</body></html>")).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::NotAudio));
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

#[tokio::test]
async fn a_body_too_short_to_judge_is_not_an_episode() {
    let server = serve(|_| with_status("200 OK", "", b"ID3")).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::NotAudio));
    assert!(names_in(dir.path()).is_empty());
}

#[tokio::test]
async fn a_status_is_kept_as_its_number() {
    let server = serve(|_| with_status("403 Forbidden", "", b"no")).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, Ended::Failed("http_403".to_owned()));
    assert!(names_in(dir.path()).is_empty());
}

// ── what a host says and what it does ───────────────────────────────

/// Says less than it sends: only what was declared is read, and what is stored
/// is what arrived, whatever the header said.
#[tokio::test]
async fn a_length_that_understates_gains_nothing() {
    let server = serve(|_| {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_vec();
        out.extend_from_slice(&audio(50_000));
        Reply::Raw(out)
    })
    .await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, Ended::Stored { size: Some(1000) });
    assert_eq!(
        std::fs::metadata(dir.path().join(format!("{episode}.mp3")))
            .unwrap()
            .len(),
        1000
    );
    assert_eq!(recorded(&pool, episode).await.1, Some(1000));
}

/// Says more than it sends and closes: a body cut short is not an episode.
#[tokio::test]
async fn a_length_that_overstates_leaves_nothing() {
    let server = serve(|_| {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Length: 900000\r\nConnection: close\r\n\r\n".to_vec();
        out.extend_from_slice(&audio(5_000));
        Reply::Raw(out)
    })
    .await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::Unreached));
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A header past the ceiling is believed, since believing it costs the host only
/// its own episode. It does not need the bytes to come.
#[tokio::test]
async fn a_declared_length_past_the_ceiling_is_refused_unread() {
    let server = serve(|_| {
        Reply::Raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1073741825\r\nConnection: close\r\n\r\n".to_vec(),
        )
    })
    .await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::TooLarge));
    assert!(names_in(dir.path()).is_empty());
}

#[tokio::test]
async fn a_body_that_never_ends_stops_at_the_ceiling() {
    let server = serve(|_| Reply::AudioFlood).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let pace = Pace {
        max_bytes: 2 * 1024 * 1024,
        ..quick()
    };

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        pace,
    )
    .await;

    assert_eq!(ended, word(FetchFailure::TooLarge));
    assert!(
        names_in(dir.path()).is_empty(),
        "the partial file went with it"
    );
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A byte now and then holds nobody up: the queue behind it has to move.
#[tokio::test]
async fn a_body_that_barely_moves_is_given_up_on() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let pace = Pace {
        window: Duration::from_millis(200),
        min_bytes_per_window: 1024,
        ..quick()
    };

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        pace,
    )
    .await;

    assert_eq!(ended, word(FetchFailure::Unreached));
    assert!(names_in(dir.path()).is_empty());
}

#[tokio::test]
async fn a_host_that_never_answers_runs_out_of_time() {
    let server = serve(|_| Reply::Silence).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let pace = Pace {
        open_deadline: Duration::from_millis(200),
        ..quick()
    };

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        pace,
    )
    .await;

    assert_eq!(ended, word(FetchFailure::Unreached));
}

// ── decision 3, on an enclosure ─────────────────────────────────────

#[tokio::test]
async fn a_public_enclosure_cannot_redirect_to_a_private_address() {
    let private = serve(|_| audio_reply(4096)).await;
    let hop = private.local("/router");
    let public = serve(move |_| redirect(&hop)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), &public.public("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &public_fetcher(&public),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::AddressRefused));
    assert_eq!(private.hits(), 0, "the private server was contacted");
    assert!(names_in(dir.path()).is_empty());
}

/// The feed decides, not the enclosure: a public feed may not name its own
/// enclosure on the local network either.
#[tokio::test]
async fn a_public_feed_cannot_name_a_private_enclosure() {
    let private = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), &private.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::AddressRefused));
    assert_eq!(private.hits(), 0);
}

/// A channel with no address is not one somebody typed on this machine.
#[tokio::test]
async fn a_channel_without_an_address_reaches_nothing_local() {
    let private = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, None, &private.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::AddressRefused));
    assert_eq!(private.hits(), 0);
}

#[tokio::test]
async fn a_secure_enclosure_cannot_redirect_into_the_clear() {
    let clear = serve(|_| audio_reply(4096)).await;
    let port = tls_redirecting_to(format!("http://{PUBLIC_HOST}:{}/clear", clear.port)).await;
    let tls = Fetcher::with(
        client_builder()
            .no_proxy()
            .add_root_certificate(test_certificate())
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], port).into()),
        online,
        Duration::from_secs(5),
    )
    .unwrap();
    let pool = enabled_pool().await;
    let dir = folder();
    let enclosure = format!("https://{PUBLIC_HOST}:{port}/a.mp3");
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), &enclosure).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &tls,
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::AddressRefused));
    assert_eq!(clear.hits(), 0, "the clear server was contacted");
}

// ── the quota ───────────────────────────────────────────────────────

async fn fill_quota(pool: &SqlitePool, room: u64) {
    let quota = store::download_quota_bytes(pool).await.unwrap();
    let (_, other) = episode_at(pool, Some(SECURE_FEED), "https://example.org/other.mp3").await;
    store::record_download(pool, other, &format!("{other}.mp3"), quota - room)
        .await
        .unwrap();
}

/// No header to go on: the quota is met while the body is read, and what had been
/// written goes.
#[tokio::test]
async fn the_quota_is_met_on_the_way_through_the_body() {
    let server = serve(|_| Reply::AudioFlood).await;
    let pool = enabled_pool().await;
    fill_quota(&pool, 1024 * 1024).await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::QuotaExceeded));
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A header past the room left is believed, as one past the ceiling is.
#[tokio::test]
async fn a_declared_length_past_the_room_left_is_refused_unread() {
    let server = serve(|_| audio_reply(1024 * 1024)).await;
    let pool = enabled_pool().await;
    fill_quota(&pool, 1024).await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::QuotaExceeded));
    assert!(names_in(dir.path()).is_empty());
}

#[tokio::test]
async fn at_the_quota_nobody_is_contacted() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    fill_quota(&pool, 0).await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, word(FetchFailure::QuotaExceeded));
    assert_eq!(server.hits(), 0);
}

#[tokio::test]
async fn the_quota_is_the_profiles_setting_and_ten_gib_by_default() {
    let pool = pool().await;
    const GIB: u64 = 1024 * 1024 * 1024;
    assert_eq!(
        store::download_quota_bytes(&pool).await.unwrap(),
        10 * GIB,
        "the default"
    );
    for (value, expected) in [
        ("3", 3 * GIB),
        ("0", 10 * GIB),
        ("much", 10 * GIB),
        ("-1", 10 * GIB),
    ] {
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, ?, 'int', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(store::DOWNLOAD_QUOTA_KEY)
        .bind(value)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            store::download_quota_bytes(&pool).await.unwrap(),
            expected,
            "{value}"
        );
    }
}

/// The row is what is counted, and an episode being fetched again does not count
/// against itself: its old file is gone.
#[tokio::test]
async fn a_row_without_its_file_is_fetched_again_and_does_not_count_against_itself() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let quota = store::download_quota_bytes(&pool).await.unwrap();
    store::record_download(&pool, episode, &format!("{episode}.mp3"), quota)
        .await
        .unwrap();

    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;

    assert_eq!(ended, Ended::Stored { size: Some(4096) });
    assert_eq!(recorded(&pool, episode).await.1, Some(4096));
}

// ── stopping ────────────────────────────────────────────────────────

#[tokio::test]
async fn cancelling_in_the_middle_leaves_nothing() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let (stop, cancel) = watch::channel(false);
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        wait_for_partial(dir.path(), episode).await;
        stop.send_replace(true);
    })
    .0;

    assert_eq!(ended, Ended::Cancelled);
    assert!(names_in(dir.path()).is_empty(), "the partial file stayed");
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A cancel does not wait for a host that has not answered.
#[tokio::test]
async fn cancelling_does_not_wait_for_the_host() {
    let server = serve(|_| Reply::Silence).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let (stop, cancel) = watch::channel(false);
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let started = std::time::Instant::now();
    let ended = tokio::join!(download, async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.send_replace(true);
    })
    .0;

    assert_eq!(ended, Ended::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn with_the_switch_off_nothing_goes_out_and_a_download_under_way_stops() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();

    switch(&pool, false).await;
    let ended = run(&leases, &fetcher, dir.path(), episode, quick()).await;
    assert_eq!(ended, Ended::Failed("disabled".to_owned()));
    assert_eq!(
        server.hits(),
        0,
        "the switch is off and a host was contacted"
    );

    switch(&pool, true).await;
    let (_keep, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        wait_for_partial(dir.path(), episode).await;
        switch(&pool, false).await;
    })
    .0;
    assert_eq!(ended, Ended::Failed("disabled".to_owned()));
    assert!(names_in(dir.path()).is_empty());
}

#[tokio::test]
async fn offline_mode_contacts_nobody_and_stops_a_download_under_way() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());

    let always_offline = Fetcher::with(client_builder().no_proxy(), offline, ROOMY).unwrap();
    let ended = run(&leases, &always_offline, dir.path(), episode, quick()).await;
    assert_eq!(ended, Ended::Failed("offline".to_owned()));
    assert_eq!(server.hits(), 0);

    static GONE_OFFLINE: AtomicBool = AtomicBool::new(false);
    let turning = Fetcher::with(
        client_builder().no_proxy(),
        || GONE_OFFLINE.load(Ordering::SeqCst),
        ROOMY,
    )
    .unwrap();
    let (_keep, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &turning,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        wait_for_partial(dir.path(), episode).await;
        GONE_OFFLINE.store(true, Ordering::SeqCst);
    })
    .0;
    assert_eq!(ended, Ended::Failed("offline".to_owned()));
    assert!(names_in(dir.path()).is_empty());
}

// ── the profile ─────────────────────────────────────────────────────

/// Hands out `left` leases, then refuses, as a lease pinned to a profile no longer
/// active does.
struct LeasesUntilSwitch {
    pool: SqlitePool,
    left: AtomicUsize,
}

impl Leases for LeasesUntilSwitch {
    type Lease = Plain;

    async fn lease(&self) -> Result<Plain, PodcastError> {
        let left = self.left.load(Ordering::SeqCst);
        if left == 0 {
            return Err(PodcastError::ProfileChanged);
        }
        self.left.store(left - 1, Ordering::SeqCst);
        Ok(Plain(self.pool.clone()))
    }
}

/// The switch lands while the body is being read: the next look at the profile
/// stops it, and nothing stays.
#[tokio::test]
async fn a_profile_switch_stops_the_body_and_writes_nothing() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = LeasesUntilSwitch {
        pool: pool.clone(),
        left: AtomicUsize::new(usize::MAX),
    };
    let fetcher = fetcher();
    let (_keep, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        wait_for_partial(dir.path(), episode).await;
        leases.left.store(0, Ordering::SeqCst);
    })
    .0;

    assert_eq!(ended, Ended::Cancelled);
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// The switch lands once the bytes are all here: the file does not get its name,
/// and the old profile's row stays as it was.
#[tokio::test]
async fn a_profile_switch_before_the_rename_leaves_nothing() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    // The gate and the read before the body; none for `finish`.
    let leases = LeasesUntilSwitch {
        pool: pool.clone(),
        left: AtomicUsize::new(2),
    };
    let pace = Pace {
        gate_every: Duration::from_secs(3600),
        ..quick()
    };

    let ended = run(&leases, &fetcher(), dir.path(), episode, pace).await;

    assert_eq!(ended, Ended::Cancelled);
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

// ── a row that goes, and deleting ───────────────────────────────────

/// Unsubscribing while the body is read: the row is gone when the file would be
/// recorded, and the file goes too.
#[tokio::test]
async fn unsubscribing_during_a_download_leaves_no_file() {
    let bytes = {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n".to_vec();
        out.extend_from_slice(&audio(4096));
        out
    };
    let server = serve(move |_| Reply::Late(bytes.clone())).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (channel, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();
    let (_keep, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        while server.hits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        store::delete_channel(&pool, channel).await.unwrap();
    })
    .0;

    assert_eq!(ended, Ended::Failed(EPISODE_GONE.to_owned()));
    assert!(names_in(dir.path()).is_empty());
}

/// An episode removed while its body was being read stays removed, and its hidden row
/// holds no file against the quota.
#[tokio::test]
async fn removing_an_episode_during_its_download_leaves_no_file() {
    let bytes = {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n".to_vec();
        out.extend_from_slice(&audio(4096));
        out
    };
    let server = serve(move |_| Reply::Late(bytes.clone())).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();
    let (_keep, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        quick(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        while server.hits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        sqlx::query("UPDATE podcast_episode SET deleted_at = 1 WHERE id = ?")
            .bind(episode)
            .execute(&pool)
            .await
            .unwrap();
    })
    .0;

    assert_eq!(ended, Ended::Failed(EPISODE_GONE.to_owned()));
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A delete sets the flag, then reads the row. One that read it just before the row was
/// written found nothing to remove, so what writes the row has to look at the flag
/// again, or the episode the user removed stays on disk.
#[tokio::test]
async fn a_delete_landing_as_the_row_is_written_leaves_nothing() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    let name = format!("{episode}.mp3");
    let target = dir.path().join(&name);
    std::fs::write(&target, audio(64)).unwrap();
    let (stop, cancel) = watch::channel(true);

    let result = record_in_place(&pool, &target, episode, &name, 64, &cancel).await;

    assert!(matches!(
        result,
        Err(crate::podcast::download::Stop::Cancelled)
    ));
    assert!(!target.exists());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));

    // Nobody asked: the row stays, and a delete arriving now finds it.
    std::fs::write(&target, audio(64)).unwrap();
    stop.send_replace(false);
    let result = record_in_place(&pool, &target, episode, &name, 64, &cancel).await;
    assert!(matches!(result, Ok(64)));
    assert_eq!(
        recorded(&pool, episode).await.0.as_deref(),
        Some(name.as_str())
    );
    assert!(delete_download(&pool, dir.path(), episode).await.unwrap());
    assert!(!target.exists());
}

#[tokio::test]
async fn deleting_a_download_keeps_the_row_with_the_switch_off_too() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (channel, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    sqlx::query("UPDATE podcast_episode SET position_ms = 4242 WHERE id = ?")
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
    run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;
    assert_eq!(names_in(dir.path()).len(), 1);

    switch(&pool, false).await;
    // A downloaded episode stays readable with the switch off.
    let name = recorded(&pool, episode).await.0.unwrap();
    assert!(stored_file(dir.path(), episode, &name).is_some());

    assert!(delete_download(&pool, dir.path(), episode).await.unwrap());
    assert!(names_in(dir.path()).is_empty());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM podcast_episode WHERE position_ms = 4242"
        )
        .await,
        1,
        "the row and its position stay"
    );
    assert!(!store::list_episodes(&pool, channel, 10, 0).await.unwrap()[0].downloaded);
    assert!(
        !delete_download(&pool, dir.path(), episode).await.unwrap(),
        "asking twice is not a failure"
    );
}

#[tokio::test]
async fn deleting_a_download_whose_file_is_already_gone_clears_the_row() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    store::record_download(&pool, episode, &format!("{episode}.mp3"), 10)
        .await
        .unwrap();
    assert!(delete_download(&pool, dir.path(), episode).await.unwrap());
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// `download_file` may have come out of an archive somebody made, and what is
/// joined to a path here gets removed. Each of these aims at a file outside the
/// folder, which stays, and the column is emptied.
#[tokio::test]
async fn a_stored_name_that_is_not_ours_is_never_opened_or_removed() {
    let pool = enabled_pool().await;
    let outer = folder();
    let dir = outer.path().join("downloads");
    std::fs::create_dir_all(&dir).unwrap();
    let precious = outer.path().join("precious.mp3");
    std::fs::write(&precious, b"keep me").unwrap();
    // Also what a shape check on the name alone might let through once joined.
    std::fs::write(dir.join("precious.mp3"), b"not ours").unwrap();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    let absolute = precious.to_string_lossy().into_owned();
    let hostile = [
        "../precious.mp3",
        "..\\precious.mp3",
        absolute.as_str(),
        "1.mp3/../../precious.mp3",
        "1.mp3\\..\\..\\precious.mp3",
        "C:\\Windows\\win.ini",
        "C:precious.mp3",
        "\\\\server\\share\\1.mp3",
        "wfnet://share/1.mp3",
        "precious.mp3",
        "1.mp3:stream",
        "1.",
        ".mp3",
        "..",
        ".",
        "",
    ];

    for name in hostile {
        assert_eq!(stored_file(&dir, episode, name), None, "{name}");

        // By the delete command.
        store::record_download(&pool, episode, name, 7)
            .await
            .unwrap();
        delete_download(&pool, &dir, episode).await.unwrap();
        assert!(precious.is_file(), "{name} reached outside by a delete");
        assert_eq!(recorded(&pool, episode).await.0, None, "{name} stayed");

        // By the sweep at launch.
        store::record_download(&pool, episode, name, 7)
            .await
            .unwrap();
        sweep(&pool, &dir, outer.path(), |_| false, true)
            .await
            .unwrap();
        assert!(precious.is_file(), "{name} reached outside by a sweep");
        assert_eq!(recorded(&pool, episode).await.0, None, "{name} stayed");

        // By unsubscribing, which removes what the rows name.
        crate::podcast::download::remove_files(dir.clone(), vec![(episode, name.to_owned())]).await;
        assert!(
            precious.is_file(),
            "{name} reached outside by unsubscribing"
        );

        // A download is not fooled into thinking it is already there.
        store::record_download(&pool, episode, name, 7)
            .await
            .unwrap();
        assert_eq!(
            store::episode_download(&pool, episode)
                .await
                .unwrap()
                .unwrap()
                .download_file
                .as_deref()
                .and_then(|stored| stored_file(&dir, episode, stored)),
            None,
            "{name}"
        );
        store::clear_download(&pool, episode).await.unwrap();
    }
    assert!(
        dir.join("precious.mp3").is_file(),
        "a name that is not ours in the folder is left alone"
    );
}

/// A row naming another episode's file reaches nothing: deleting this one must not
/// take that one with it, and playing it must not play that one.
#[tokio::test]
async fn a_stored_name_of_another_episode_is_not_this_ones() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, mine) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/m.mp3").await;
    let (_, other) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/o.mp3").await;
    let name = format!("{other}.mp3");
    std::fs::write(dir.path().join(&name), audio(64)).unwrap();
    store::record_download(&pool, other, &name, 64)
        .await
        .unwrap();
    store::record_download(&pool, mine, &name, 64)
        .await
        .unwrap();
    assert_eq!(stored_file(dir.path(), mine, &name), None);
    assert!(stored_file(dir.path(), other, &name).is_some());

    delete_download(&pool, dir.path(), mine).await.unwrap();

    assert_eq!(names_in(dir.path()), std::slice::from_ref(&name));
    assert_eq!(recorded(&pool, mine).await.0, None);
    assert_eq!(
        recorded(&pool, other).await.0.as_deref(),
        Some(name.as_str())
    );
}

/// The two lots meeting: what a download put in place is what playing the episode
/// opens, from its position, whatever the switch and offline mode say, and deleting
/// it makes the episode a request again.
#[tokio::test]
async fn a_downloaded_episode_plays_from_disk_at_its_position() {
    use super::super::playback::{resolve_source, start_position, EpisodeSource};

    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let ended = run(
        &TestLeases(pool.clone()),
        &fetcher(),
        dir.path(),
        episode,
        quick(),
    )
    .await;
    assert_eq!(ended, Ended::Stored { size: Some(4096) });
    assert!(store::save_position(&pool, episode, 4242, false)
        .await
        .is_ok());
    let hits = server.hits();

    let row = store::playable(&pool, episode).await.unwrap().unwrap();
    assert_eq!(start_position(&row), 4242);
    for (enabled, is_offline) in [(true, false), (false, false), (true, true), (false, true)] {
        let source = resolve_source(&row, dir.path(), enabled, is_offline).unwrap();
        assert!(
            matches!(&source, EpisodeSource::File(path)
                if *path == dir.path().join(format!("{episode}.mp3"))),
            "switch {enabled}, offline {is_offline}"
        );
    }
    assert_eq!(server.hits(), hits, "playing it asked nobody");

    delete_download(&pool, dir.path(), episode).await.unwrap();
    let row = store::playable(&pool, episode).await.unwrap().unwrap();
    assert_eq!(start_position(&row), 4242, "the position stays");
    assert!(matches!(
        resolve_source(&row, dir.path(), true, false),
        Ok(EpisodeSource::Stream { .. })
    ));
    assert!(matches!(
        resolve_source(&row, dir.path(), false, false),
        Err(PodcastError::Disabled)
    ));
}

/// A size out of a made-up archive buys no room under the quota.
#[tokio::test]
async fn a_negative_size_does_not_make_room() {
    let pool = enabled_pool().await;
    let (_, big) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/b.mp3").await;
    let (_, liar) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/l.mp3").await;
    store::record_download(&pool, big, &format!("{big}.mp3"), 4096)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE podcast_episode SET download_file = '9.mp3', download_size = -999999 WHERE id = ?",
    )
    .bind(liar)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(store::download_used_bytes(&pool, None).await.unwrap(), 4096);
}

// ── what a brutal stop leaves ───────────────────────────────────────

/// Everything a process killed at the wrong moment can leave, and what must stay.
#[tokio::test]
async fn a_sweep_removes_what_a_crash_left_and_nothing_else() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, kept) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/kept.mp3").await;
    let (_, cut) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/cut.mp3").await;
    let (_, unrecorded) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/r.mp3").await;
    let (_, lost) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/lost.mp3").await;

    let kept_name = format!("{kept}.mp3");
    std::fs::write(dir.path().join(&kept_name), audio(4096)).unwrap();
    store::record_download(&pool, kept, &kept_name, 4096)
        .await
        .unwrap();
    // Killed while the body was being written.
    std::fs::write(dir.path().join(format!("{cut}.part")), audio(1024)).unwrap();
    // Killed between the rename and the row.
    std::fs::write(dir.path().join(format!("{unrecorded}.mp3")), audio(1024)).unwrap();
    // The file was removed behind the app's back.
    store::record_download(&pool, lost, &format!("{lost}.mp3"), 2048)
        .await
        .unwrap();
    // Not ours: nothing here knows what it is.
    std::fs::write(dir.path().join("desktop.ini"), b"[.]").unwrap();

    let swept = sweep(&pool, dir.path(), dir.path(), |_| false, true)
        .await
        .unwrap();

    assert_eq!(
        swept,
        Swept {
            partial_files: 1,
            orphan_files: 1,
            orphan_rows: 1
        }
    );
    assert_eq!(
        names_in(dir.path()),
        [kept_name.clone(), "desktop.ini".to_owned()]
    );
    assert_eq!(
        recorded(&pool, kept).await.0.as_deref(),
        Some(kept_name.as_str())
    );
    assert_eq!(recorded(&pool, lost).await, (None, None, None));
    assert_eq!(
        store::download_used_bytes(&pool, None).await.unwrap(),
        4096,
        "the quota counts what is left"
    );
    assert_eq!(
        sweep(&pool, dir.path(), dir.path(), |_| false, true)
            .await
            .unwrap(),
        Swept::default(),
        "a second look finds nothing"
    );
}

/// A download under way is not a crash's leftovers.
#[tokio::test]
async fn a_sweep_leaves_the_download_under_way_alone() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, writing) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/w.mp3").await;
    let partial = dir.path().join(format!("{writing}.part"));
    let renamed = dir.path().join(format!("{writing}.mp3"));
    std::fs::write(&partial, b"x").unwrap();
    std::fs::write(&renamed, b"x").unwrap();

    let swept = sweep(&pool, dir.path(), dir.path(), move |id| id == writing, true)
        .await
        .unwrap();

    assert_eq!(swept, Swept::default());
    assert!(partial.is_file() && renamed.is_file());
}

/// The caches' drive is unplugged and the session fell back: the rows are right and
/// the folder is the wrong one, and clearing them would lose every download.
#[tokio::test]
async fn a_sweep_of_a_folder_it_cannot_trust_keeps_the_rows() {
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    let name = format!("{episode}.mp3");
    store::record_download(&pool, episode, &name, 10)
        .await
        .unwrap();

    let swept = sweep(&pool, dir.path(), dir.path(), |_| false, false)
        .await
        .unwrap();

    assert_eq!(swept.orphan_rows, 0);
    assert_eq!(
        recorded(&pool, episode).await.0.as_deref(),
        Some(name.as_str())
    );
}

/// The same drive, gone after launch: a sweep on a profile switch cannot tell a file
/// that is gone from a folder that does not answer, and must not clear what it cannot
/// see. The next one, with the drive back, would remove the files as orphans.
#[tokio::test]
async fn a_folder_that_does_not_answer_clears_no_row() {
    let pool = enabled_pool().await;
    let outer = folder();
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    let name = format!("{episode}.mp3");
    store::record_download(&pool, episode, &name, 10)
        .await
        .unwrap();

    // The caches' root is not there: a drive unplugged.
    let unplugged = outer.path().join("unplugged");
    let dir = unplugged.join("profiles/1/podcast-downloads");
    let swept = sweep(&pool, &dir, &unplugged, |_| false, true)
        .await
        .unwrap();
    assert_eq!(swept.orphan_rows, 0);
    assert_eq!(
        recorded(&pool, episode).await.0.as_deref(),
        Some(name.as_str())
    );

    // The folder cannot be listed, whatever the reason.
    let not_a_folder = outer.path().join("not-a-folder");
    std::fs::write(&not_a_folder, b"x").unwrap();
    let swept = sweep(&pool, &not_a_folder, outer.path(), |_| false, true)
        .await
        .unwrap();
    assert_eq!(swept.orphan_rows, 0);
    assert_eq!(
        recorded(&pool, episode).await.0.as_deref(),
        Some(name.as_str())
    );

    // The root answers and the folder is gone: so is the file, and the row goes.
    let removed = outer.path().join("removed");
    let swept = sweep(&pool, &removed, outer.path(), |_| false, true)
        .await
        .unwrap();
    assert_eq!(swept.orphan_rows, 1);
    assert_eq!(recorded(&pool, episode).await, (None, None, None));
}

/// A task dropped halfway, the way a runtime going down drops one, takes its partial
/// file with it.
#[tokio::test]
async fn a_download_dropped_halfway_takes_its_partial_file_with_it() {
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let path = dir.path().to_path_buf();
    let task = tokio::spawn({
        let pool = pool.clone();
        let fetcher = fetcher();
        async move { run(&TestLeases(pool), &fetcher, &path, episode, quick()).await }
    });
    wait_for_partial(dir.path(), episode).await;
    task.abort();
    let _ = task.await;
    assert!(names_in(dir.path()).is_empty());
}

// ── what is said about it ───────────────────────────────────────────

#[tokio::test]
async fn an_episode_on_disk_asks_nobody() {
    let server = serve(|_| audio_reply(4096)).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());
    run(&leases, &fetcher(), dir.path(), episode, quick()).await;
    assert_eq!(server.hits(), 1);

    let ended = run(&leases, &fetcher(), dir.path(), episode, quick()).await;

    assert_eq!(ended, Ended::Stored { size: None });
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn an_episode_gone_or_removed_is_said_to_be() {
    let pool = enabled_pool().await;
    let dir = folder();
    let gone = Ended::Failed(EPISODE_GONE.to_owned());
    let leases = TestLeases(pool.clone());
    assert_eq!(
        run(&leases, &fetcher(), dir.path(), 999_999, quick()).await,
        gone
    );
    let (_, episode) = episode_at(&pool, Some(SECURE_FEED), "https://example.org/a.mp3").await;
    sqlx::query("UPDATE podcast_episode SET deleted_at = 1 WHERE id = ?")
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        run(&leases, &fetcher(), dir.path(), episode, quick()).await,
        gone
    );
}

/// About four a second, however fast the bytes come, and in the shape the frontend
/// reads. Run at the real pace, since the pace is what is being tested.
#[tokio::test]
async fn progress_goes_out_at_most_about_four_times_a_second() {
    // A byte every 50 ms: faster than the events may go, and nothing to fill a disk.
    let server = serve(|_| Reply::AudioTrickle).await;
    let pool = enabled_pool().await;
    let dir = folder();
    let (_, episode) =
        episode_at(&pool, Some(&server.local("/feed")), &server.local("/a.mp3")).await;
    let leases = TestLeases(pool.clone());
    let fetcher = fetcher();
    let (stop, cancel) = watch::channel(false);
    let seen = Seen::default();
    let download = download_episode(
        &leases,
        &fetcher,
        dir.path(),
        episode,
        Pace::standard(),
        cancel,
        &seen,
    );
    let ended = tokio::join!(download, async {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        stop.send_replace(true);
    })
    .0;
    assert_eq!(ended, Ended::Cancelled);

    let updates = seen.updates.lock().unwrap();
    assert!(updates.len() >= 2, "{} updates", updates.len());
    // 1.5 s at 4 a second, and the first one that says it started.
    assert!(updates.len() <= 8, "{} updates in 1.5 s", updates.len());
    assert!(updates.iter().all(|u| u.state == DownloadState::Running));
    assert!(updates
        .windows(2)
        .all(|w| w[0].received_bytes <= w[1].received_bytes));
    let last = updates.last().unwrap();
    assert!(last.received_bytes > 0);
    assert_eq!(
        serde_json::to_value(last).unwrap(),
        serde_json::json!({
            "episodeId": episode,
            "state": "running",
            "receivedBytes": last.received_bytes,
            "totalBytes": null,
            "errorWord": null,
        })
    );
}

// ── the queue ───────────────────────────────────────────────────────

fn job(profile_id: i64, channel_id: i64, episode_id: i64) -> Job {
    Job {
        profile_id,
        channel_id,
        episode_id,
    }
}

#[tokio::test]
async fn the_queue_runs_one_at_a_time_in_order_and_asking_twice_changes_nothing() {
    let queue = Queue::new();
    assert_eq!(queue.push(job(1, 1, 10)), Pushed::QueuedStartWorker);
    assert_eq!(queue.push(job(1, 1, 11)), Pushed::Queued);
    assert_eq!(queue.push(job(1, 1, 12)), Pushed::Queued);
    assert_eq!(queue.push(job(1, 1, 11)), Pushed::Already);
    // The same id in another profile is another episode.
    assert_eq!(queue.push(job(2, 1, 11)), Pushed::Queued);

    let order = Mutex::new(Vec::new());
    let running = AtomicUsize::new(0);
    queue
        .drain(|job, _| {
            let (order, running) = (&order, &running);
            async move {
                assert_eq!(running.fetch_add(1, Ordering::SeqCst), 0, "two at a time");
                tokio::time::sleep(Duration::from_millis(5)).await;
                order.lock().unwrap().push((job.profile_id, job.episode_id));
                running.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .await;

    assert_eq!(*order.lock().unwrap(), [(1, 10), (1, 11), (1, 12), (2, 11)]);
    assert_eq!(
        queue.push(job(1, 1, 10)),
        Pushed::QueuedStartWorker,
        "the worker retired, so the next one has to be started"
    );
}

#[tokio::test]
async fn the_queue_cancels_what_is_in_line_and_tells_what_runs_to_stop() {
    let queue = Queue::new();
    queue.push(job(1, 1, 10));
    queue.push(job(1, 1, 11));
    queue.push(job(1, 2, 12));
    queue.push(job(2, 1, 13));
    let stopped = Mutex::new(Vec::new());
    queue
        .drain(|job, cancel| {
            let (queue, stopped) = (&queue, &stopped);
            async move {
                if job.episode_id == 10 {
                    assert_eq!(queue.in_flight(1), (Some(10), vec![11, 12]));
                    assert_eq!(queue.in_flight(2), (None, vec![13]));
                    // In line: taken out here, and the caller says so.
                    assert!(queue.cancel(1, 11));
                    // Another profile's episode of the same number is another one.
                    assert!(!queue.cancel(2, 10));
                    assert!(!*cancel.borrow());
                    // Running: told to stop, and it says so itself.
                    assert!(!queue.cancel(1, 10));
                    assert!(*cancel.borrow());
                    // Nothing of the kind.
                    assert!(!queue.cancel(1, 99));
                }
                stopped.lock().unwrap().push(job.episode_id);
            }
        })
        .await;
    assert_eq!(*stopped.lock().unwrap(), [10, 12, 13]);
}

#[tokio::test]
async fn unsubscribing_empties_the_line_of_that_channel() {
    let queue = Queue::new();
    queue.push(job(1, 7, 10));
    queue.push(job(1, 7, 11));
    queue.push(job(1, 8, 12));
    queue.push(job(2, 7, 13));
    let ran = Mutex::new(Vec::new());
    queue
        .drain(|job, cancel| {
            let (queue, ran) = (&queue, &ran);
            async move {
                if job.episode_id == 10 {
                    let taken = queue.cancel_channel(1, 7);
                    assert_eq!(taken, [self::job(1, 7, 11)]);
                    assert!(*cancel.borrow(), "the one running is told to stop");
                }
                ran.lock().unwrap().push(job.episode_id);
            }
        })
        .await;
    assert_eq!(*ran.lock().unwrap(), [10, 12, 13]);
}

/// A panic in one download must not leave a worker nobody would start again.
#[tokio::test]
async fn a_panic_in_a_download_retires_the_worker() {
    static QUEUE: Queue = Queue::new();
    QUEUE.push(job(1, 1, 10));
    let worker = tokio::spawn(async {
        QUEUE
            .drain(|_, _| async {
                panic!("in a download");
            })
            .await;
    });
    assert!(worker.await.is_err());
    assert_eq!(QUEUE.push(job(1, 1, 11)), Pushed::QueuedStartWorker);
}

/// Another profile's job says nothing to the view on screen, whose episode of the
/// same number it would land on. The only test that touches this flag.
#[test]
fn only_the_active_profile_hears_about_its_downloads() {
    crate::podcast::download::profile_changed(5);
    assert!(crate::podcast::download::is_active(5));
    assert!(!crate::podcast::download::is_active(4));
}

// ── where it goes ───────────────────────────────────────────────────

/// Under the caches and per profile, and moved with them. Nowhere near a library
/// folder, since a library folder is a folder the user picked.
#[test]
fn the_folder_is_per_profile_under_the_cache_root() {
    let paths = crate::paths::AppPaths::from_root("/data/waveflow".into(), None)
        .with_cache_root("/fast/cache".into());
    let one = paths.profile_podcast_download_dir(1);
    assert!(one.starts_with("/fast/cache"));
    assert!(one.starts_with(paths.profile_cache_dir(1)));
    assert_ne!(one, paths.profile_podcast_download_dir(2));
}
