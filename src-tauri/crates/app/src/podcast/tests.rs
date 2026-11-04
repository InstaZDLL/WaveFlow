//! The fetcher against servers that misbehave on purpose, and the database on
//! the really migrated schema with foreign keys on.

use std::io::Write as _;
use std::ops::Deref;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use url::Url;
use waveflow_core::podcast::{FetchFailure, OpmlEntry};

use super::fetch::{client_builder, FeedBody, FetchError, Fetcher, Origin, Validators};
use super::refresh::{
    import_pass, refresh_pass, subscribe, Leases, PassObserver, PassOutcome, PodcastError,
    Subscribed,
};
use super::schedule::{run_schedule, since_last_check, Schedule, UNREADABLE_RETRY};
use super::store;

pub(super) const PUBLIC_HOST: &str = "feeds.example.org";

// ── a server that does what the test says ───────────────────────────

/// What one request is answered with.
#[derive(Clone)]
pub(super) enum Reply {
    /// These bytes, then the connection closes.
    Raw(Vec<u8>),
    /// A chunked body that never ends, as fast as it can.
    Flood,
    /// Headers, then a byte now and then, for ever.
    Trickle,
    /// Accepts and never answers.
    Silence,
    /// These bytes, after long enough for a test to do something meanwhile.
    Late(Vec<u8>),
}

pub(super) fn with_status(status: &str, headers: &str, body: &[u8]) -> Reply {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    Reply::Raw(out)
}

fn ok(body: &str) -> Reply {
    with_status(
        "200 OK",
        "Content-Type: application/rss+xml\r\n",
        body.as_bytes(),
    )
}

pub(super) fn redirect(location: &str) -> Reply {
    with_status("302 Found", &format!("Location: {location}\r\n"), b"")
}

pub(super) struct Server {
    pub(super) port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Every request's first line and headers, in order.
    fn seen(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub(super) fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// An address on this server, by IP: a local address, as far as `may_open`
    /// can tell.
    pub(super) fn local(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// An address on this server under a public name, which only a fetcher from
    /// [`public_fetcher`] resolves to it.
    pub(super) fn public(&self, path: &str) -> String {
        format!("http://{PUBLIC_HOST}:{}{path}", self.port)
    }
}

pub(super) async fn serve(answer: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let answer = Arc::clone(&answer);
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                seen.lock().unwrap().push(head);
                match answer(&path) {
                    Reply::Raw(bytes) => {
                        let _ = stream.write_all(&bytes).await;
                        let _ = stream.shutdown().await;
                    }
                    Reply::Flood => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                            .await;
                        let chunk = format!("10000\r\n{}\r\n", "x".repeat(0x10000));
                        while stream.write_all(chunk.as_bytes()).await.is_ok() {}
                    }
                    Reply::Trickle => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                            .await;
                        while stream.write_all(b"1\r\nx\r\n").await.is_ok() {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                    Reply::Late(bytes) => {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        let _ = stream.write_all(&bytes).await;
                        let _ = stream.shutdown().await;
                    }
                    Reply::Silence => {
                        tokio::time::sleep(Duration::from_secs(600)).await;
                    }
                }
            });
        }
    });
    Server { port, requests }
}

pub(super) fn online() -> bool {
    false
}

pub(super) fn offline() -> bool {
    true
}

/// A deadline the slowest machine running the whole suite stays inside.
const ROOMY: Duration = Duration::from_secs(30);

pub(super) fn fetcher() -> Fetcher {
    Fetcher::with(client_builder().no_proxy(), online, ROOMY).unwrap()
}

/// A fetcher whose deadline a test can wait out.
fn impatient_fetcher() -> Fetcher {
    Fetcher::with(
        client_builder().no_proxy(),
        online,
        Duration::from_millis(800),
    )
    .unwrap()
}

/// A fetcher that believes [`PUBLIC_HOST`] is the local test server, so an
/// address that `may_open` calls public really reaches something here.
pub(super) fn public_fetcher(server: &Server) -> Fetcher {
    Fetcher::with(
        client_builder()
            .no_proxy()
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], server.port).into()),
        online,
        ROOMY,
    )
    .unwrap()
}

pub(super) fn url(raw: &str) -> Url {
    Url::parse(raw).unwrap()
}

async fn fetch(fetcher: &Fetcher, raw: &str) -> Result<FeedBody, FetchError> {
    fetcher
        .feed(&url(raw), Origin::of(&url(raw)), &Validators::default())
        .await
}

fn failed(failure: FetchFailure) -> Option<FetchError> {
    Some(FetchError::Failed(failure))
}

// ── the rules of a hop ──────────────────────────────────────────────

/// A public feed may not lead anywhere private: the hop is refused before it is
/// contacted, and the private server hears nothing.
#[tokio::test]
async fn a_public_address_cannot_redirect_to_a_private_one() {
    let private = serve(|_| ok("secret")).await;
    for target in [
        private.local("/admin"),
        format!("http://localhost:{}/admin", private.port),
        "http://192.168.1.1/".to_owned(),
        "http://10.0.0.1/".to_owned(),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://[::1]/".to_owned(),
        "http://[::ffff:192.168.1.1]/".to_owned(),
        "http://router/".to_owned(),
        "http://nas.local/".to_owned(),
    ] {
        let hop = target.clone();
        let public = serve(move |_| redirect(&hop)).await;
        let result = fetch(&public_fetcher(&public), &public.public("/feed")).await;
        assert_eq!(
            result.err(),
            failed(FetchFailure::AddressRefused),
            "{target}"
        );
        assert_eq!(public.hits(), 1, "{target}");
    }
    assert_eq!(private.hits(), 0, "the private server was contacted");
}

/// A feed typed with a local address may name local addresses, as a station may.
#[tokio::test]
async fn a_local_address_may_lead_to_a_local_one() {
    let target = serve(|_| ok("<rss/>")).await;
    let hop = target.local("/there");
    let local = serve(move |_| redirect(&hop)).await;
    let result = fetch(&fetcher(), &local.local("/feed")).await;
    assert!(matches!(result, Ok(FeedBody::Fresh { .. })));
    assert_eq!(target.hits(), 1);
}

#[tokio::test]
async fn an_origin_is_decided_once_from_the_typed_address() {
    let private = serve(|_| ok("secret")).await;
    let public = Origin::of(&url("https://feeds.example.org/feed"));
    let result = fetcher()
        .small_file(&url(&private.local("/cover.png")), public, 1024)
        .await;
    assert_eq!(result.err(), failed(FetchFailure::AddressRefused));
    assert_eq!(private.hits(), 0);
    let local = Origin::of(&url("http://nas.local/feed"));
    assert!(fetcher()
        .small_file(&url(&private.local("/cover.png")), local, 1024)
        .await
        .is_ok());
}

/// The certificate of [`tls_redirecting_to`], for `feeds.example.org` and any name under
/// it, trusted by a test's client as its own root so the check itself still runs.
fn test_certificate() -> reqwest::Certificate {
    reqwest::Certificate::from_der(include_bytes!("testdata/self-signed-cert.der")).unwrap()
}

/// A real TLS server answers a redirect into the clear.
#[tokio::test]
async fn a_secure_address_cannot_redirect_into_the_clear() {
    let clear = serve(|_| ok("<rss/>")).await;
    let location = format!("http://{PUBLIC_HOST}:{}/clear", clear.port);
    let port = tls_redirecting_to(location).await;
    let fetcher = Fetcher::with(
        client_builder()
            .no_proxy()
            .add_root_certificate(test_certificate())
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], port).into()),
        online,
        Duration::from_secs(5),
    )
    .unwrap();
    let result = fetch(&fetcher, &format!("https://{PUBLIC_HOST}:{port}/feed")).await;
    assert_eq!(result.err(), failed(FetchFailure::AddressRefused));
    assert_eq!(clear.hits(), 0, "the clear server was contacted");
}

/// A feed reached over TLS through a temporary redirect announces an address in the
/// clear. What it is judged by is where it was served from, not the address that led
/// there, and that one being in the clear gives the announcement nothing.
#[tokio::test]
async fn a_secure_feed_cannot_announce_the_clear_whatever_led_to_it() {
    let announced = serve(|_| ok(&rss(&[("a", "Clear")]))).await;
    let body = feed_announcing(&announced.public("/new"));
    let port = tls_answering(format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    let target = format!("https://{PUBLIC_HOST}:{port}/feed");
    let start = serve(move |_| redirect(&target)).await;
    let pool = enabled_pool().await;
    let fetcher = Fetcher::with(
        client_builder()
            .no_proxy()
            .add_root_certificate(test_certificate())
            .resolve(PUBLIC_HOST, ([127, 0, 0, 1], port).into()),
        online,
        ROOMY,
    )
    .unwrap();
    let id = subscribe(
        &TestLeases(pool.clone()),
        &fetcher,
        &start.public("/feed"),
        cache_dir().path(),
    )
    .await
    .unwrap()
    .channel_id();
    assert_eq!(announced.hits(), 0, "the clear address was contacted");
    assert_eq!(stored_address(&pool, id).await, start.public("/feed"));
}

pub(super) async fn tls_redirecting_to(location: String) -> u16 {
    tls_answering(format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ))
    .await
}

/// A blocking one-connection TLS server on a thread of its own, since `rustls`
/// is what the app already has and it needs no async glue for this.
async fn tls_answering(response: String) -> u16 {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let cert = CertificateDer::from(include_bytes!("testdata/self-signed-cert.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("testdata/self-signed-key.der").to_vec(),
    ));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, tcp);
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match std::io::Read::read(&mut stream, &mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => return,
            }
        }
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    port
}

#[tokio::test]
async fn five_redirects_are_followed_and_a_sixth_is_not() {
    fn chain(path: &str) -> Reply {
        let step: usize = path.trim_start_matches('/').parse().unwrap();
        redirect(&format!("/{}", step + 1))
    }
    let endless = serve(chain).await;
    assert_eq!(
        fetch(&fetcher(), &endless.local("/0")).await.err(),
        failed(FetchFailure::TooManyRedirects)
    );
    assert_eq!(endless.hits(), 6, "the first request and five hops");

    let five = serve(|path| {
        if path == "/5" {
            ok("<rss/>")
        } else {
            chain(path)
        }
    })
    .await;
    assert!(fetch(&fetcher(), &five.local("/0")).await.is_ok());

    let nowhere = serve(|_| with_status("302 Found", "", b"")).await;
    assert_eq!(
        fetch(&fetcher(), &nowhere.local("/")).await.err(),
        failed(FetchFailure::TooManyRedirects)
    );
}

#[tokio::test]
async fn a_hop_holding_credentials_or_another_scheme_is_refused_before_contact() {
    for location in [
        "https://user:pw@127.0.0.1/",
        "file:///etc/passwd",
        "ftp://127.0.0.1/",
    ] {
        let hop = location.to_owned();
        let server = serve(move |_| redirect(&hop)).await;
        assert_eq!(
            fetch(&fetcher(), &server.local("/")).await.err(),
            failed(FetchFailure::AddressRefused),
            "{location}"
        );
    }
}

// ── bodies and deadlines ────────────────────────────────────────────

#[tokio::test]
async fn a_declared_length_past_the_ceiling_is_refused_unread() {
    let server = serve(|_| {
        Reply::Raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999\r\nConnection: close\r\n\r\n".to_vec(),
        )
    })
    .await;
    assert_eq!(
        fetch(&fetcher(), &server.local("/")).await.err(),
        failed(FetchFailure::TooLarge)
    );
}

/// A lying `Content-Length`: declaring less than it sends gets it cut off at
/// what it declared, and declaring more than it sends is a failure, not a
/// truncated feed quietly stored.
#[tokio::test]
async fn a_lying_content_length_gains_nothing() {
    let short = serve(|_| {
        Reply::Raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcdefgh".to_vec(),
        )
    })
    .await;
    match fetch(&fetcher(), &short.local("/")).await {
        Ok(FeedBody::Fresh { bytes, .. }) => assert_eq!(bytes, b"abcd"),
        other => panic!("{other:?}"),
    }
    let long = serve(|_| {
        Reply::Raw(
            b"HTTP/1.1 200 OK\r\nContent-Length: 400\r\nConnection: close\r\n\r\nabcd".to_vec(),
        )
    })
    .await;
    assert_eq!(
        fetch(&fetcher(), &long.local("/")).await.err(),
        failed(FetchFailure::Unreached)
    );
}

/// A body that never ends and declares nothing is stopped by its ceiling when it
/// is fast, and by the deadline when it is slow.
#[tokio::test]
async fn a_body_that_never_ends_is_stopped() {
    let flood = serve(|_| Reply::Flood).await;
    assert_eq!(
        fetcher()
            .small_file(
                &url(&flood.local("/")),
                Origin::of(&url(&flood.local("/"))),
                1024 * 1024
            )
            .await
            .err(),
        failed(FetchFailure::TooLarge)
    );

    let started = std::time::Instant::now();
    let trickle = serve(|_| Reply::Trickle).await;
    assert_eq!(
        fetch(&impatient_fetcher(), &trickle.local("/")).await.err(),
        failed(FetchFailure::Unreached)
    );
    assert!(started.elapsed() < Duration::from_secs(5));

    let silence = serve(|_| Reply::Silence).await;
    assert_eq!(
        fetch(&impatient_fetcher(), &silence.local("/")).await.err(),
        failed(FetchFailure::Unreached)
    );
}

#[tokio::test]
async fn a_status_is_kept_as_its_number() {
    let server = serve(|_| with_status("503 Service Unavailable", "", b"busy")).await;
    assert_eq!(
        fetch(&fetcher(), &server.local("/")).await.err(),
        failed(FetchFailure::Status(503))
    );
}

// ── offline mode ────────────────────────────────────────────────────

#[tokio::test]
async fn offline_mode_contacts_nobody() {
    let server = serve(|_| ok("<rss/>")).await;
    let fetcher =
        Fetcher::with(client_builder().no_proxy(), offline, Duration::from_secs(1)).unwrap();
    assert_eq!(
        fetch(&fetcher, &server.local("/")).await.err(),
        Some(FetchError::Offline)
    );
    assert_eq!(server.hits(), 0);
}

/// Offline mode is checked before every request, each hop included, not only
/// where a fetch begins.
#[tokio::test]
async fn offline_mode_turned_on_mid_redirect_stops_the_next_hop() {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn offline_after_the_first_request() -> bool {
        CALLS.fetch_add(1, Ordering::SeqCst) > 0
    }
    let server = serve(|_| redirect("/next")).await;
    let fetcher = Fetcher::with(
        client_builder().no_proxy(),
        offline_after_the_first_request,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        fetch(&fetcher, &server.local("/")).await.err(),
        Some(FetchError::Offline)
    );
    assert_eq!(server.hits(), 1);
}

// ── the database ────────────────────────────────────────────────────

/// The real schema, with foreign keys on, as the app opens it.
async fn pool() -> SqlitePool {
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
    pool
}

async fn switch(pool: &SqlitePool, on: bool) {
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES ('podcasts.enabled', ?, 'bool', 0)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(if on { "true" } else { "false" })
    .execute(pool)
    .await
    .unwrap();
}

pub(super) async fn enabled_pool() -> SqlitePool {
    let pool = pool().await;
    switch(&pool, true).await;
    pool
}

struct Plain(SqlitePool);

impl Deref for Plain {
    type Target = SqlitePool;

    fn deref(&self) -> &SqlitePool {
        &self.0
    }
}

struct TestLeases(SqlitePool);

impl Leases for TestLeases {
    type Lease = Plain;

    async fn lease(&self) -> Result<Plain, PodcastError> {
        Ok(Plain(self.0.clone()))
    }
}

/// Records what a pass reported, and stops it when told to.
#[derive(Default)]
struct Recorder {
    stop: AtomicBool,
    changed: Mutex<Vec<i64>>,
}

impl PassObserver for Recorder {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn progress(&self, _done: u64, _total: u64) {}

    fn channel_changed(&self, channel_id: i64) {
        self.changed.lock().unwrap().push(channel_id);
    }
}

fn rss(items: &[(&str, &str)]) -> String {
    let items: String = items
        .iter()
        .map(|(guid, title)| {
            format!(
                "<item><guid>{guid}</guid><title>{title}</title>\
                 <pubDate>Mon, 01 Jan 2024 10:00:00 +0000</pubDate>\
                 <enclosure url=\"/audio/{guid}.mp3\" type=\"audio/mpeg\"/></item>"
            )
        })
        .collect();
    format!(
        "<rss><channel><title>The Show</title><description>&lt;p&gt;About&lt;/p&gt;</description>\
         {items}</channel></rss>"
    )
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap()
}

fn cache_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn subscribing_stores_the_feed_and_a_second_time_contacts_nobody() {
    let pool = enabled_pool().await;
    let body = rss(&[("a", "First"), ("b", "Second")]);
    let server = serve(move |_| ok(&body)).await;
    let cache = cache_dir();
    let leases = TestLeases(pool.clone());
    let added = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap();
    let Subscribed::Added(id) = added else {
        panic!("{added:?}");
    };
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.title, "The Show");
    assert_eq!(channel.description.as_deref(), Some("About"));
    assert!(channel.has_feed_address);
    assert_eq!(channel.episode_count, 2);
    assert_eq!(channel.unplayed_count, 2);
    assert!(channel.last_refreshed_at.is_some());
    assert_eq!(channel.last_error, None);
    let episodes = store::list_episodes(&pool, id, 10, 0).await.unwrap();
    assert_eq!(episodes.len(), 2);
    assert!(episodes.iter().all(|e| e.has_enclosure && !e.downloaded));
    // A relative enclosure resolved against where the feed was read from.
    let stored: String = sqlx::query_scalar("SELECT enclosure_url FROM podcast_episode LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.starts_with(&server.local("/audio/")), "{stored}");

    let hits = server.hits();
    let again = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap();
    assert_eq!(again, Subscribed::Already(id));
    assert_eq!(server.hits(), hits);
}

/// A typo leaves nothing behind, whichever way it fails.
#[tokio::test]
async fn what_is_not_a_feed_leaves_nothing_behind() {
    let pool = enabled_pool().await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let html = serve(|_| {
        with_status(
            "200 OK",
            "Content-Type: text/html\r\n",
            b"<!DOCTYPE html><html><body>Not a feed</body></html>",
        )
    })
    .await;
    let missing = serve(|_| with_status("404 Not Found", "", b"")).await;
    for (address, word) in [
        (html.local("/"), "invalid_feed"),
        (missing.local("/"), "http_404"),
        ("file:///etc/passwd".to_owned(), "address_refused"),
        ("not an address".to_owned(), "address_refused"),
        (
            "https://user:pw@example.org/feed".to_owned(),
            "address_refused",
        ),
    ] {
        let err = subscribe(&leases, &fetcher(), &address, cache.path())
            .await
            .unwrap_err();
        let PodcastError::Failed(failure) = err else {
            panic!("{err:?}");
        };
        assert_eq!(failure.code(), word);
    }
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_channel").await,
        0
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_episode").await,
        0
    );
}

/// A refresh updates an episode re-published under its guid, keeps what belongs
/// to the user, and does not bring back what the user removed.
#[tokio::test]
async fn a_refresh_matches_on_the_guid_and_keeps_what_is_the_users() {
    let pool = enabled_pool().await;
    let bodies = Arc::new(Mutex::new(rss(&[
        ("a", "First"),
        ("b", "Second"),
        ("c", "Third"),
    ])));
    let current = Arc::clone(&bodies);
    let server = serve(move |_| ok(&current.lock().unwrap())).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();

    sqlx::query(
        "UPDATE podcast_episode SET position_ms = 5000, played_at = 7 WHERE title = 'First'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE podcast_episode SET deleted_at = 9 WHERE title = 'Second'")
        .execute(&pool)
        .await
        .unwrap();

    *bodies.lock().unwrap() = rss(&[
        ("a", "First, corrected"),
        ("b", "Second, back again"),
        ("d", "Fourth"),
    ]);
    let recorder = Recorder::default();
    let summary = refresh_pass(&leases, &fetcher(), None, cache.path(), &recorder)
        .await
        .unwrap();
    assert_eq!(summary.outcome, PassOutcome::Done);
    assert_eq!(summary.refreshed, 1);
    assert_eq!(*recorder.changed.lock().unwrap(), [id]);

    let episodes = store::list_episodes(&pool, id, 10, 0).await.unwrap();
    let mut titles: Vec<&str> = episodes.iter().map(|e| e.title.as_str()).collect();
    titles.sort_unstable();
    assert_eq!(
        titles,
        ["First, corrected", "Fourth", "Third"],
        "updated, new, and gone from the feed but kept"
    );
    let first = episodes
        .iter()
        .find(|e| e.title == "First, corrected")
        .unwrap();
    assert_eq!(first.position_ms, 5000);
    assert_eq!(first.played_at, Some(7));
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_episode").await,
        4,
        "the removed row stays, so it stays removed"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM podcast_episode
              WHERE title = 'Second' AND deleted_at = 9"
        )
        .await,
        1,
        "a removed row is not even updated"
    );
}

/// The guid is often the enclosure's address, token and all: nothing but its
/// hash is stored, and the hash is what the match runs on.
#[tokio::test]
async fn a_guid_is_stored_only_as_its_hash() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("the-guid-marker", "One")]))).await;
    let cache = cache_dir();
    subscribe(
        &TestLeases(pool.clone()),
        &fetcher(),
        &server.local("/feed"),
        cache.path(),
    )
    .await
    .unwrap();
    let key: String = sqlx::query_scalar("SELECT item_key FROM podcast_episode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        key,
        waveflow_core::podcast::address::item_key("the-guid-marker")
    );
}

/// Unsubscribing while the feed is being read has to stick: the refresh must not create
/// the row again, and must not write into the channel followed next, which an id used
/// twice would hand it.
#[tokio::test]
async fn a_refresh_in_flight_does_not_bring_back_a_channel() {
    let pool = enabled_pool().await;
    let slow = Arc::new(AtomicBool::new(false));
    let state = Arc::clone(&slow);
    let server = serve(move |path| {
        if path == "/next" {
            return ok(&rss(&[("n", "Next")]));
        }
        let Reply::Raw(bytes) = ok(&rss(&[("a", "One")])) else {
            unreachable!()
        };
        if state.load(Ordering::SeqCst) {
            Reply::Late(bytes)
        } else {
            Reply::Raw(bytes)
        }
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    slow.store(true, Ordering::SeqCst);
    let hits = server.hits();
    let client = fetcher();
    let recorder = Recorder::default();
    let (summary, next) = tokio::join!(
        refresh_pass(&leases, &client, None, cache.path(), &recorder),
        async {
            while server.hits() == hits {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(store::delete_channel(&pool, id).await.unwrap());
            subscribe(&leases, &client, &server.local("/next"), cache.path())
                .await
                .unwrap()
                .channel_id()
        }
    );
    let summary = summary.unwrap();
    assert_eq!((summary.refreshed, summary.failed), (0, 0));
    assert_ne!(next, id, "an id was used twice");
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_channel").await,
        1
    );
    let episodes = store::list_episodes(&pool, next, 10, 0).await.unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(
        episodes[0].title, "Next",
        "the old feed went into the new channel"
    );
    assert!(recorder.changed.lock().unwrap().is_empty());
}

/// The switch is read before each request, not only where a channel begins: turned off
/// while a feed is on its way, it stops the picture and the announced address after it.
#[tokio::test]
async fn the_switch_turned_off_mid_channel_stops_the_next_request() {
    let pool = enabled_pool().await;
    let slow = Arc::new(AtomicBool::new(false));
    let state = Arc::clone(&slow);
    let feed = "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
                <title>T</title><itunes:image href=\"/moved-cover.png\"/>\
                </channel></rss>";
    let announcing = "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
                      <title>T</title><itunes:new-feed-url>/announced</itunes:new-feed-url>\
                      </channel></rss>";
    let server = serve(move |path| {
        let body = if path == "/announcing" {
            announcing
        } else {
            feed
        };
        if path.ends_with(".png") || path == "/announced" {
            return with_status("200 OK", "", b"");
        }
        let Reply::Raw(bytes) = ok(body) else {
            unreachable!()
        };
        if state.load(Ordering::SeqCst) {
            Reply::Late(bytes)
        } else {
            Reply::Raw(bytes)
        }
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let client = fetcher();

    // A subscription: the feed is on its way when the switch goes off.
    slow.store(true, Ordering::SeqCst);
    let hits = server.hits();
    let address = server.local("/feed");
    let (subscribed, ()) =
        tokio::join!(subscribe(&leases, &client, &address, cache.path()), async {
            while server.hits() == hits {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            switch(&pool, false).await;
        });
    assert!(
        subscribed.is_ok(),
        "what was read is stored: {subscribed:?}"
    );
    assert!(
        !server.seen().iter().any(|r| r.contains(".png")),
        "the picture was asked for with the switch off"
    );

    // An announced address, the same way.
    switch(&pool, true).await;
    let hits = server.hits();
    let address = server.local("/announcing");
    let (subscribed, ()) =
        tokio::join!(subscribe(&leases, &client, &address, cache.path()), async {
            while server.hits() == hits {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            switch(&pool, false).await;
        });
    assert!(
        matches!(subscribed, Err(PodcastError::Disabled)),
        "{subscribed:?}"
    );
    assert!(
        !server.seen().iter().any(|r| r.contains("/announced")),
        "the announced address was asked for with the switch off"
    );
}

/// A permanent redirect too long to be stored would leave the channel unreadable for good.
#[tokio::test]
async fn a_move_too_long_to_keep_leaves_the_address() {
    let pool = enabled_pool().await;
    let (server, moved) = moving_server(|path| match path {
        "/feed" => {
            let long = format!(
                "/{}",
                "a".repeat(waveflow_core::podcast::address::MAX_URL_CHARS)
            );
            with_status(
                "301 Moved Permanently",
                &format!("Location: {long}\r\n"),
                b"",
            )
        }
        _ => ok(&rss(&[("a", "One")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    moved.store(true, Ordering::SeqCst);
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.failed, 1);
    assert_eq!(stored_address(&pool, id).await, server.local("/feed"));
    moved.store(false, Ordering::SeqCst);
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.failed, 0, "the channel can still be asked");
}

/// The export's document and its count, apart from the file it goes to.
#[tokio::test]
async fn an_export_says_what_it_wrote_and_what_it_could_not() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    for path in ["/one", "/two"] {
        subscribe(&leases, &fetcher(), &server.local(path), cache.path())
            .await
            .unwrap();
    }
    sqlx::query("UPDATE podcast_channel SET feed_url = NULL WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let (document, report) = crate::commands::podcast::opml_export(&pool).await.unwrap();
    assert_eq!((report.written, report.without_address), (1, 1));
    assert!(document.contains("/two"));
    assert!(!document.contains("/one"));
}

/// An unchanged feed costs a 304: its validators go back, and nothing is
/// rewritten.
#[tokio::test]
async fn validators_are_sent_back_and_a_304_changes_nothing() {
    let pool = enabled_pool().await;
    let served = Arc::new(AtomicUsize::new(0));
    let count_served = Arc::clone(&served);
    let server = serve(move |_| {
        if count_served.fetch_add(1, Ordering::SeqCst) == 0 {
            with_status(
                "200 OK",
                "ETag: \"v1\"\r\nLast-Modified: Mon, 01 Jan 2024 10:00:00 GMT\r\n",
                rss(&[("a", "One")]).as_bytes(),
            )
        } else {
            with_status("304 Not Modified", "", b"")
        }
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap();
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!((summary.refreshed, summary.unchanged), (0, 1));
    let second = server.seen().pop().unwrap().to_ascii_lowercase();
    assert!(second.contains("if-none-match: \"v1\""), "{second}");
    assert!(
        second.contains("if-modified-since: mon, 01 jan 2024"),
        "{second}"
    );
}

/// A failure is stored as its word, the channel keeps what it had, and a later
/// success clears it.
#[tokio::test]
async fn a_failed_refresh_keeps_the_channel_and_stores_one_word() {
    let pool = enabled_pool().await;
    let healthy = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&healthy);
    let server = serve(move |_| {
        if state.load(Ordering::SeqCst) {
            ok(&rss(&[("a", "One")]))
        } else {
            with_status(
                "500 Internal Server Error",
                "",
                b"<h1>Trace: secret text</h1>",
            )
        }
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();

    healthy.store(false, Ordering::SeqCst);
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        Some(id),
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.failed, 1);
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.last_error.as_deref(), Some("http_500"));
    assert_eq!(channel.episode_count, 1);
    assert!(channel.last_refreshed_at.is_some());

    healthy.store(true, Ordering::SeqCst);
    refresh_pass(
        &leases,
        &fetcher(),
        Some(id),
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.last_error, None);
}

/// A feed that hands out fresh guids at every refresh must not grow the table
/// without end. What holds something of the user's is never what goes.
#[tokio::test]
async fn rows_past_the_ceiling_go_unless_they_are_the_users() {
    let pool = enabled_pool().await;
    let round = Arc::new(AtomicUsize::new(0));
    let rounds = Arc::clone(&round);
    let server = serve(move |_| {
        let round = rounds.load(Ordering::SeqCst);
        let items: Vec<(String, String)> = (0..500)
            .map(|index| (format!("r{round}-{index}"), format!("Round {round}")))
            .collect();
        let items: Vec<(&str, &str)> = items
            .iter()
            .map(|(guid, title)| (guid.as_str(), title.as_str()))
            .collect();
        ok(&rss(&items))
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap();
    // Nothing in this feed is dated apart, so "oldest" falls to the lowest id,
    // which is what is marked here.
    sqlx::query(
        "UPDATE podcast_episode SET position_ms = 1
          WHERE id = (SELECT MIN(id) FROM podcast_episode)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for next in 1..6 {
        round.store(next, Ordering::SeqCst);
        refresh_pass(
            &leases,
            &fetcher(),
            None,
            cache.path(),
            &Recorder::default(),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_episode").await,
        2_001,
        "the ceiling, and the one row that holds a position"
    );
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM podcast_episode WHERE position_ms = 1"
        )
        .await,
        1
    );
}

/// Unsubscribing forgets the episodes through the foreign key, on the migrated
/// schema: a test on a hand-written fixture would not prove the cascade.
#[tokio::test]
async fn unsubscribing_forgets_the_episodes() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let cache = cache_dir();
    let id = subscribe(
        &TestLeases(pool.clone()),
        &fetcher(),
        &server.local("/feed"),
        cache.path(),
    )
    .await
    .unwrap()
    .channel_id();
    assert!(store::delete_channel(&pool, id).await.unwrap());
    assert!(!store::delete_channel(&pool, id).await.unwrap());
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_episode").await,
        0
    );
}

/// What an archive leaves behind, and what subscribing again gives back: the
/// channel's row, its episodes and their positions.
#[tokio::test]
async fn subscribing_again_gives_a_restored_channel_its_addresses_back() {
    let pool = enabled_pool().await;
    let server = serve(|_| {
        with_status(
            "200 OK",
            "ETag: \"v1\"\r\n",
            rss(&[("a", "One")]).as_bytes(),
        )
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    sqlx::query("UPDATE podcast_episode SET position_ms = 4242")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "UPDATE podcast_channel SET feed_url = NULL, site_url = NULL, image_url = NULL,
                etag = NULL, last_modified = NULL;
         UPDATE podcast_episode SET enclosure_url = NULL, chapters_url = NULL, transcripts = NULL;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert!(!channel.has_feed_address);
    let episode = &store::list_episodes(&pool, id, 1, 0).await.unwrap()[0];
    assert!(!episode.has_enclosure);
    // A refresh cannot reach a channel with no address, and does not try.
    let hits = server.hits();
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.refreshed + summary.failed, 0);
    assert_eq!(server.hits(), hits);

    let again = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap();
    assert_eq!(again, Subscribed::Added(id));
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert!(channel.has_feed_address);
    let episode = &store::list_episodes(&pool, id, 1, 0).await.unwrap()[0];
    assert!(episode.has_enclosure);
    assert_eq!(episode.position_ms, 4242);
}

// ── the picture ─────────────────────────────────────────────────────

/// One transparent pixel.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn feed_with_image(image: &str) -> String {
    format!(
        "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
         <title>Pictured</title><itunes:image href=\"{image}\"/></channel></rss>"
    )
}

#[tokio::test]
async fn a_channel_picture_lands_in_the_cache_and_is_fetched_once() {
    let pool = enabled_pool().await;
    let server = serve(|path| match path {
        "/cover.png" => with_status("200 OK", "", PNG),
        _ => ok(&feed_with_image("/cover.png")),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    let hash = channel.artwork_hash.expect("the picture was cached");
    assert!(waveflow_core::artwork::metadata::existing_path(cache.path(), &hash).is_some());

    refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    let fetched = server
        .seen()
        .iter()
        .filter(|request| request.contains("/cover.png"))
        .count();
    assert_eq!(
        fetched, 1,
        "the same address with the file on disk needs nothing"
    );
}

/// A page in place of a picture is not cached, and neither failure costs the
/// channel its subscription.
#[tokio::test]
async fn what_is_not_a_picture_stays_out_of_the_cache() {
    let pool = enabled_pool().await;
    let server = serve(|path| match path {
        "/cover.png" => with_status("200 OK", "", b"<html>not an image</html>"),
        _ => ok(&feed_with_image("/cover.png")),
    })
    .await;
    let cache = cache_dir();
    let id = subscribe(
        &TestLeases(pool.clone()),
        &fetcher(),
        &server.local("/feed"),
        cache.path(),
    )
    .await
    .unwrap()
    .channel_id();
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.artwork_hash, None);
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0);
}

/// A feed on the internet cannot use its picture to reach the user's network.
#[tokio::test]
async fn a_public_feed_cannot_name_a_private_picture() {
    let pool = enabled_pool().await;
    let private = serve(|_| with_status("200 OK", "", PNG)).await;
    let image = private.local("/router.png");
    let public = serve(move |_| ok(&feed_with_image(&image))).await;
    let cache = cache_dir();
    let id = subscribe(
        &TestLeases(pool.clone()),
        &public_fetcher(&public),
        &public.public("/feed"),
        cache.path(),
    )
    .await
    .unwrap()
    .channel_id();
    assert_eq!(private.hits(), 0, "the private server was contacted");
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.artwork_hash, None);
}

// ── the switch ──────────────────────────────────────────────────────

/// With the switch off nothing goes out, and what was stored stays readable.
#[tokio::test]
async fn with_the_switch_off_nothing_goes_out() {
    let pool = pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();

    // Off is the default: no row in `profile_setting` at all.
    assert!(!store::is_enabled(&pool).await.unwrap());
    assert!(matches!(
        subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path()).await,
        Err(PodcastError::Disabled)
    ));
    assert_eq!(server.hits(), 0);

    switch(&pool, true).await;
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    let after_subscribing = server.hits();

    switch(&pool, false).await;
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.outcome, PassOutcome::Disabled);
    let list = vec![OpmlEntry {
        title: None,
        feed_url: server.local("/other"),
    }];
    assert!(matches!(
        import_pass(
            &leases,
            &fetcher(),
            list,
            cache.path(),
            &Recorder::default()
        )
        .await,
        Err(PodcastError::Disabled)
    ));
    assert_eq!(server.hits(), after_subscribing, "a request went out");
    assert!(
        matches!(
            subscribe(&leases, &fetcher(), &server.local("/third"), cache.path()).await,
            Err(PodcastError::Disabled)
        ),
        "whichever command is called"
    );
    assert_eq!(server.hits(), after_subscribing, "a request went out");
    assert!(
        !store::is_enabled(&pool).await.unwrap(),
        "nothing but the setting's own write turns it on"
    );

    assert_eq!(store::list_channels(&pool).await.unwrap().len(), 1);
    assert_eq!(
        store::list_episodes(&pool, id, 10, 0).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn offline_mode_stops_a_pass_without_blaming_the_channel() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    let hits = server.hits();
    let cut_off =
        Fetcher::with(client_builder().no_proxy(), offline, Duration::from_secs(1)).unwrap();
    let summary = refresh_pass(&leases, &cut_off, None, cache.path(), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(summary.outcome, PassOutcome::Offline);
    assert!(matches!(
        subscribe(&leases, &cut_off, &server.local("/other"), cache.path()).await,
        Err(PodcastError::Offline)
    ));
    assert_eq!(server.hits(), hits);
    let channel = store::channel(&pool, id).await.unwrap().unwrap();
    assert_eq!(channel.last_error, None);
}

#[tokio::test]
async fn a_cancelled_pass_stops_before_the_next_channel() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    for path in ["/one", "/two"] {
        subscribe(&leases, &fetcher(), &server.local(path), cache.path())
            .await
            .unwrap();
    }
    let hits = server.hits();
    let recorder = Recorder::default();
    recorder.stop.store(true, Ordering::SeqCst);
    let summary = refresh_pass(&leases, &fetcher(), None, cache.path(), &recorder)
        .await
        .unwrap();
    assert_eq!(summary.outcome, PassOutcome::Cancelled);
    assert_eq!(server.hits(), hits);
}

// ── the timer ───────────────────────────────────────────────────────

const INTERVAL: Duration = super::schedule::REFRESH_INTERVAL;
const GRACE: Duration = super::schedule::OVERDUE_GRACE;

/// The profile as the timer sees it, on the test's paused clock.
struct Timer {
    wake: Arc<Notify>,
    on: Arc<AtomicBool>,
    /// A pass stamps the profile, unless a test says it reached nobody.
    stamps: Arc<AtomicBool>,
    last_check: Arc<Mutex<Option<tokio::time::Instant>>>,
    refreshes: Arc<AtomicUsize>,
    handle: tokio::task::JoinHandle<()>,
}

impl Timer {
    /// Starts the loop. `checked_ago` is the stored last check.
    fn start(on: bool, checked_ago: Option<Duration>) -> Self {
        // Far enough along that a stamp in the past is a real instant.
        let wake = Arc::new(Notify::new());
        let on = Arc::new(AtomicBool::new(on));
        let stamps = Arc::new(AtomicBool::new(true));
        let last_check = Arc::new(Mutex::new(
            checked_ago.map(|ago| tokio::time::Instant::now() - ago),
        ));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let handle = {
            let (wake, on, stamps, last_check, refreshes) = (
                Arc::clone(&wake),
                Arc::clone(&on),
                Arc::clone(&stamps),
                Arc::clone(&last_check),
                Arc::clone(&refreshes),
            );
            tokio::spawn(async move {
                let read = || {
                    let on = on.load(Ordering::SeqCst);
                    let since_last_check = last_check
                        .lock()
                        .unwrap()
                        .map(|at: tokio::time::Instant| at.elapsed());
                    async move {
                        if on {
                            Schedule::On { since_last_check }
                        } else {
                            Schedule::Off
                        }
                    }
                };
                let refresh = || {
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    if stamps.load(Ordering::SeqCst) {
                        *last_check.lock().unwrap() = Some(tokio::time::Instant::now());
                    }
                    async {}
                };
                run_schedule(&wake, read, refresh).await;
            })
        };
        Self {
            wake,
            on,
            stamps,
            last_check,
            refreshes,
            handle,
        }
    }

    fn count(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    async fn advance(&self, by: Duration) {
        settle().await;
        tokio::time::advance(by).await;
        settle().await;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Lets everything that can run, run, on the paused clock.
async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

/// Nothing goes out during startup, even for a profile that is overdue or has
/// never been checked: it waits out the grace, then counts from the check.
#[tokio::test(start_paused = true)]
async fn an_overdue_check_runs_five_minutes_after_launch() {
    for checked_ago in [None, Some(INTERVAL), Some(INTERVAL * 40)] {
        let timer = Timer::start(true, checked_ago);
        timer.advance(GRACE - Duration::from_secs(1)).await;
        assert_eq!(
            timer.count(),
            0,
            "a request during startup: {checked_ago:?}"
        );
        timer.advance(Duration::from_secs(1)).await;
        assert_eq!(timer.count(), 1, "{checked_ago:?}");
        timer.advance(INTERVAL - Duration::from_secs(1)).await;
        assert_eq!(timer.count(), 1, "{checked_ago:?}");
        timer.advance(Duration::from_secs(1)).await;
        assert_eq!(timer.count(), 2, "{checked_ago:?}");
    }
}

/// The six hours count from the last check, not from launch: a session of four
/// hours after one of three still gets its check, on time.
#[tokio::test(start_paused = true)]
async fn the_interval_counts_from_the_stored_check() {
    let timer = Timer::start(true, Some(Duration::from_secs(3 * 60 * 60)));
    timer
        .advance(Duration::from_secs(3 * 60 * 60) - Duration::from_secs(1))
        .await;
    assert_eq!(timer.count(), 0);
    timer.advance(Duration::from_secs(1)).await;
    assert_eq!(timer.count(), 1);
}

/// A check nearly due at launch still waits out the grace: nothing goes out
/// during startup, however close it was.
#[tokio::test(start_paused = true)]
async fn a_check_nearly_due_still_waits_out_the_grace() {
    let timer = Timer::start(true, Some(INTERVAL - Duration::from_secs(1)));
    timer.advance(GRACE - Duration::from_secs(1)).await;
    assert_eq!(timer.count(), 0);
    timer.advance(Duration::from_secs(1)).await;
    assert_eq!(timer.count(), 1);
}

/// With the switch off no timer runs at all, however long goes by. Turning it on
/// gives a check after the grace, never at once.
#[tokio::test(start_paused = true)]
async fn with_the_switch_off_the_timer_does_not_run() {
    let timer = Timer::start(false, None);
    timer.advance(INTERVAL * 10).await;
    assert_eq!(timer.count(), 0);

    timer.on.store(true, Ordering::SeqCst);
    timer.wake.notify_one();
    settle().await;
    assert_eq!(timer.count(), 0, "fired on being turned on");
    timer.advance(GRACE).await;
    assert_eq!(timer.count(), 1);

    timer.on.store(false, Ordering::SeqCst);
    timer.wake.notify_one();
    timer.advance(INTERVAL * 3).await;
    assert_eq!(timer.count(), 1);
}

/// The switch turned off by a path that did not wake the loop is still read when
/// the wait ends.
#[tokio::test(start_paused = true)]
async fn the_switch_is_read_again_when_the_wait_ends() {
    let timer = Timer::start(true, None);
    settle().await;
    timer.on.store(false, Ordering::SeqCst);
    timer.advance(GRACE).await;
    assert_eq!(timer.count(), 0);
}

/// A pass run by hand during the wait stamps the profile without waking the loop.
/// When the old wait ends the check is no longer due, and no host is asked twice.
#[tokio::test(start_paused = true)]
async fn a_check_run_by_hand_moves_the_next_one() {
    let timer = Timer::start(true, None);
    timer.advance(Duration::from_secs(60)).await;
    *timer.last_check.lock().unwrap() = Some(tokio::time::Instant::now());
    timer.advance(GRACE).await;
    assert_eq!(timer.count(), 0, "asked again right after a check by hand");
    timer.advance(INTERVAL - GRACE).await;
    assert_eq!(timer.count(), 1);
}

/// A pass that reached nobody, in offline mode, stamps nothing. It must come back
/// after the grace and not spin, and it runs when offline mode ends.
#[tokio::test(start_paused = true)]
async fn a_pass_that_stamps_nothing_does_not_spin() {
    let timer = Timer::start(true, None);
    timer.stamps.store(false, Ordering::SeqCst);
    timer.advance(GRACE).await;
    assert_eq!(timer.count(), 1);
    settle().await;
    assert_eq!(timer.count(), 1, "ran again at once");
    timer.advance(GRACE).await;
    assert_eq!(timer.count(), 2);
    assert!(timer.last_check.lock().unwrap().is_none());
}

/// A stamp from the future is a clock that moved back. Read as "just now" it would read
/// so again at every wake, and no check would ever come.
#[test]
fn a_last_check_in_the_future_counts_as_never() {
    assert_eq!(since_last_check(None, 1_000), None);
    assert_eq!(since_last_check(Some(2_000), 1_000), None);
    assert_eq!(
        since_last_check(Some(1_000), 1_000),
        Some(Duration::from_millis(0))
    );
    assert_eq!(
        since_last_check(Some(400), 1_000),
        Some(Duration::from_millis(600))
    );
}

/// Settings that cannot be read are not an answer: nothing goes out on them, the loop
/// does not spin, and it does not stay parked for good either.
#[tokio::test(start_paused = true)]
async fn unreadable_settings_are_looked_at_again() {
    let reads = Arc::new(AtomicUsize::new(0));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let readable = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Notify::new());
    let handle = {
        let (reads, refreshes, readable, wake) = (
            Arc::clone(&reads),
            Arc::clone(&refreshes),
            Arc::clone(&readable),
            Arc::clone(&wake),
        );
        tokio::spawn(async move {
            let read = || {
                reads.fetch_add(1, Ordering::SeqCst);
                let answer = if readable.load(Ordering::SeqCst) {
                    Schedule::On {
                        since_last_check: None,
                    }
                } else {
                    Schedule::Unreadable
                };
                async move { answer }
            };
            let refresh = || {
                refreshes.fetch_add(1, Ordering::SeqCst);
                async {}
            };
            run_schedule(&wake, read, refresh).await;
        })
    };
    settle().await;
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    tokio::time::advance(UNREADABLE_RETRY * 2).await;
    settle().await;
    assert!(reads.load(Ordering::SeqCst) >= 2, "stayed parked");
    assert!(reads.load(Ordering::SeqCst) <= 3, "spun");
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);

    readable.store(true, Ordering::SeqCst);
    tokio::time::advance(UNREADABLE_RETRY).await;
    settle().await;
    tokio::time::advance(GRACE).await;
    settle().await;
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    handle.abort();
}

/// What the loop reads: a full pass stamps the profile, a pass over one channel
/// or one that reached nobody does not.
#[tokio::test]
async fn a_pass_over_every_channel_is_what_stamps_the_profile() {
    let pool = enabled_pool().await;
    let server = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    assert_eq!(store::last_check_at(&pool).await.unwrap(), None);

    refresh_pass(
        &leases,
        &fetcher(),
        Some(id),
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(store::last_check_at(&pool).await.unwrap(), None);

    let cut_off =
        Fetcher::with(client_builder().no_proxy(), offline, Duration::from_secs(1)).unwrap();
    refresh_pass(&leases, &cut_off, None, cache.path(), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(store::last_check_at(&pool).await.unwrap(), None);

    refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert!(store::last_check_at(&pool).await.unwrap().is_some());
}

// ── a feed that has moved ───────────────────────────────────────────

async fn stored_address(pool: &SqlitePool, id: i64) -> String {
    sqlx::query_scalar("SELECT feed_url FROM podcast_channel WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A server whose `/feed` answers with `answer` once asked to, and a feed before.
async fn moving_server(
    answer: impl Fn(&str) -> Reply + Send + Sync + 'static,
) -> (Server, Arc<AtomicBool>) {
    let moved = Arc::new(AtomicBool::new(false));
    let state = Arc::clone(&moved);
    let server = serve(move |path| {
        if path == "/feed" && !state.load(Ordering::SeqCst) {
            ok(&rss(&[("a", "One")]))
        } else {
            answer(path)
        }
    })
    .await;
    (server, moved)
}

/// 301 and 308 update the stored address, and the row, its episodes and their
/// positions stay. The next refresh asks the new address only.
#[tokio::test]
async fn a_permanent_redirect_updates_the_address() {
    for status in ["301 Moved Permanently", "308 Permanent Redirect"] {
        let pool = enabled_pool().await;
        let (server, moved) = moving_server(move |path| match path {
            "/feed" => with_status(status, "Location: /new\r\n", b""),
            _ => ok(&rss(&[("a", "One, moved")])),
        })
        .await;
        let leases = TestLeases(pool.clone());
        let cache = cache_dir();
        let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
            .await
            .unwrap()
            .channel_id();
        sqlx::query("UPDATE podcast_episode SET position_ms = 77")
            .execute(&pool)
            .await
            .unwrap();
        moved.store(true, Ordering::SeqCst);
        refresh_pass(
            &leases,
            &fetcher(),
            None,
            cache.path(),
            &Recorder::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            stored_address(&pool, id).await,
            server.local("/new"),
            "{status}"
        );
        let episode = &store::list_episodes(&pool, id, 1, 0).await.unwrap()[0];
        assert_eq!(episode.title, "One, moved");
        assert_eq!(episode.position_ms, 77);

        let before = server.hits();
        refresh_pass(
            &leases,
            &fetcher(),
            None,
            cache.path(),
            &Recorder::default(),
        )
        .await
        .unwrap();
        let asked = server.seen();
        assert_eq!(asked.len(), before + 1);
        assert!(asked[before].starts_with("GET /new "), "{}", asked[before]);
        assert_eq!(
            subscribe(&leases, &fetcher(), &server.local("/new"), cache.path())
                .await
                .unwrap(),
            Subscribed::Already(id),
            "the channel is known by its new address"
        );
    }
}

/// 302 and 307 say nothing about the address, and neither does what comes after
/// one: a permanent redirect after a temporary one is not the feed moving.
#[tokio::test]
async fn a_temporary_redirect_leaves_the_address() {
    for status in ["302 Found", "307 Temporary Redirect"] {
        let pool = enabled_pool().await;
        let (server, moved) = moving_server(move |path| match path {
            "/feed" => with_status(status, "Location: /hop\r\n", b""),
            "/hop" => with_status("301 Moved Permanently", "Location: /new\r\n", b""),
            _ => ok(&rss(&[("a", "One")])),
        })
        .await;
        let leases = TestLeases(pool.clone());
        let cache = cache_dir();
        let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
            .await
            .unwrap()
            .channel_id();
        moved.store(true, Ordering::SeqCst);
        let summary = refresh_pass(
            &leases,
            &fetcher(),
            None,
            cache.path(),
            &Recorder::default(),
        )
        .await
        .unwrap();
        assert_eq!(summary.refreshed, 1, "{status}");
        assert_eq!(
            stored_address(&pool, id).await,
            server.local("/feed"),
            "{status}"
        );
    }
}

/// The run of permanent hops stops at the first temporary one: that hop is where
/// the feed moved to.
#[tokio::test]
async fn the_address_is_the_last_of_the_permanent_hops() {
    let pool = enabled_pool().await;
    let (server, moved) = moving_server(|path| match path {
        "/feed" => with_status("301 Moved Permanently", "Location: /stable\r\n", b""),
        "/stable" => with_status("302 Found", "Location: /cdn\r\n", b""),
        _ => ok(&rss(&[("a", "One")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    moved.store(true, Ordering::SeqCst);
    refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(stored_address(&pool, id).await, server.local("/stable"));
}

/// A 301 ending on a 304 is still the feed moving, with nothing new to read.
#[tokio::test]
async fn a_move_is_kept_when_the_feed_has_not_changed() {
    let pool = enabled_pool().await;
    let (server, moved) = moving_server(|path| match path {
        "/feed" => with_status("301 Moved Permanently", "Location: /new\r\n", b""),
        _ => with_status("304 Not Modified", "", b""),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    moved.store(true, Ordering::SeqCst);
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.unchanged, 1);
    assert_eq!(stored_address(&pool, id).await, server.local("/new"));
}

/// A move onto an address another channel follows is refused: two rows cannot be
/// one address, and the channel stays where it was, with what it had.
#[tokio::test]
async fn a_move_onto_a_followed_address_leaves_the_channel() {
    let pool = enabled_pool().await;
    let (server, moved) = moving_server(|path| match path {
        "/feed" => with_status("301 Moved Permanently", "Location: /other\r\n", b""),
        _ => ok(&rss(&[("b", "Other")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let first = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    subscribe(&leases, &fetcher(), &server.local("/other"), cache.path())
        .await
        .unwrap();
    moved.store(true, Ordering::SeqCst);
    let summary = refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(summary.failed, 0);
    assert_eq!(stored_address(&pool, first).await, server.local("/feed"));
    let episodes = store::list_episodes(&pool, first, 10, 0).await.unwrap();
    assert!(
        !episodes.is_empty() && episodes.iter().all(|episode| episode.title != "Other"),
        "the other channel's episodes were stored under this one"
    );
}

/// Subscribing through an address that has moved stores where it leads, and one
/// that leads to a followed feed is that feed.
#[tokio::test]
async fn subscribing_through_a_move_stores_where_it_leads() {
    let pool = enabled_pool().await;
    let server = serve(|path| match path {
        "/old" => with_status("301 Moved Permanently", "Location: /feed\r\n", b""),
        _ => ok(&rss(&[("a", "One")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let added = subscribe(&leases, &fetcher(), &server.local("/old"), cache.path())
        .await
        .unwrap();
    assert_eq!(
        stored_address(&pool, added.channel_id()).await,
        server.local("/feed")
    );
    assert_eq!(
        subscribe(&leases, &fetcher(), &server.local("/old"), cache.path())
            .await
            .unwrap(),
        Subscribed::Already(added.channel_id())
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_channel").await,
        1
    );
}

fn feed_announcing(address: &str) -> String {
    format!(
        "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
         <title>Old</title><itunes:new-feed-url>{address}</itunes:new-feed-url>\
         <item><guid>a</guid><title>Old title</title>\
         <enclosure url=\"/a.mp3\"/></item></channel></rss>"
    )
}

/// `itunes:new-feed-url` is read before it is believed: the new feed's content is
/// stored and the address follows.
#[tokio::test]
async fn a_feed_announcing_a_new_address_is_followed() {
    let pool = enabled_pool().await;
    let (server, moved) = moving_server(|path| match path {
        "/feed" => ok(&feed_announcing("/new")),
        _ => ok(&rss(&[("a", "New title")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();
    moved.store(true, Ordering::SeqCst);
    refresh_pass(
        &leases,
        &fetcher(),
        None,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(stored_address(&pool, id).await, server.local("/new"));
    let episode = &store::list_episodes(&pool, id, 1, 0).await.unwrap()[0];
    assert_eq!(episode.title, "New title");
}

/// An announced address that does not answer, or is not a feed, costs nothing: a
/// typo in someone's feed must not lose the show, and the old content is stored.
#[tokio::test]
async fn an_announced_address_that_is_no_feed_leaves_the_channel() {
    for target in ["/dead", "/page"] {
        let pool = enabled_pool().await;
        let announced = target.to_owned();
        let (server, moved) = moving_server(move |path| match path {
            "/feed" => ok(&feed_announcing(&announced)),
            "/dead" => with_status("404 Not Found", "", b""),
            _ => with_status("200 OK", "", b"<html>a page</html>"),
        })
        .await;
        let leases = TestLeases(pool.clone());
        let cache = cache_dir();
        let id = subscribe(&leases, &fetcher(), &server.local("/feed"), cache.path())
            .await
            .unwrap()
            .channel_id();
        moved.store(true, Ordering::SeqCst);
        let summary = refresh_pass(
            &leases,
            &fetcher(),
            None,
            cache.path(),
            &Recorder::default(),
        )
        .await
        .unwrap();
        assert_eq!((summary.refreshed, summary.failed), (1, 0), "{target}");
        assert_eq!(
            stored_address(&pool, id).await,
            server.local("/feed"),
            "{target}"
        );
        let episode = &store::list_episodes(&pool, id, 1, 0).await.unwrap()[0];
        assert_eq!(episode.title, "Old title", "{target}");
    }
}

/// A public feed announcing a private address is judged by where it was, not by
/// where it points: the private server hears nothing.
#[tokio::test]
async fn a_public_feed_cannot_announce_a_private_address() {
    let private = serve(|_| ok(&rss(&[("a", "Stolen")]))).await;
    let announced = private.local("/feed");
    let public = serve(move |_| ok(&feed_announcing(&announced))).await;
    let pool = enabled_pool().await;
    let cache = cache_dir();
    let id = subscribe(
        &TestLeases(pool.clone()),
        &public_fetcher(&public),
        &public.public("/feed"),
        cache.path(),
    )
    .await
    .unwrap()
    .channel_id();
    assert_eq!(private.hits(), 0, "the private server was contacted");
    assert_eq!(stored_address(&pool, id).await, public.public("/feed"));
}

// ── the import ──────────────────────────────────────────────────────

/// One dead feed does not lose the rest of a list, and the report names it by
/// its title and its word, not by its address.
#[tokio::test]
async fn an_import_carries_on_past_a_dead_feed() {
    let pool = enabled_pool().await;
    let server = serve(|path| match path {
        "/dead" => with_status("410 Gone", "", b""),
        _ => ok(&rss(&[("a", "One")])),
    })
    .await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let fetcher = public_fetcher(&server);
    subscribe(&leases, &fetcher, &server.public("/known"), cache.path())
        .await
        .unwrap();
    let list = vec![
        OpmlEntry {
            title: Some("Known".into()),
            feed_url: server.public("/known"),
        },
        OpmlEntry {
            title: Some("Dead".into()),
            feed_url: server.public("/dead"),
        },
        OpmlEntry {
            title: None,
            feed_url: server.public("/new"),
        },
    ];
    let report = import_pass(&leases, &fetcher, list, cache.path(), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(report.outcome, PassOutcome::Done);
    assert_eq!((report.added, report.already, report.total), (1, 1, 3));
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].title.as_deref(), Some("Dead"));
    assert_eq!(report.failed[0].code, "http_410");
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains(PUBLIC_HOST), "{json}");
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM podcast_channel").await,
        2
    );
}

/// A list is a file someone may have been handed: it may not make the app probe
/// the local network, and the word a line comes back with cannot say what
/// answered, since nothing was asked.
#[tokio::test]
async fn an_import_does_not_reach_the_local_network() {
    let pool = enabled_pool().await;
    let local = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let public = serve(|_| ok(&rss(&[("a", "One")]))).await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let list = vec![
        OpmlEntry {
            title: Some("Router".into()),
            feed_url: local.local("/admin"),
        },
        OpmlEntry {
            title: Some("Named".into()),
            feed_url: "http://nas.local/feed".into(),
        },
        OpmlEntry {
            title: Some("Fine".into()),
            feed_url: public.public("/feed"),
        },
    ];
    let report = import_pass(
        &leases,
        &public_fetcher(&public),
        list,
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    assert_eq!(local.hits(), 0, "a local address was contacted");
    assert_eq!(report.added, 1);
    let words: Vec<&str> = report.failed.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(words, ["address_refused", "address_refused"]);

    // Typed by the person, the same address is theirs to name.
    assert!(
        subscribe(&leases, &fetcher(), &local.local("/admin"), cache.path())
            .await
            .is_ok()
    );
}

// ── no address in any log ───────────────────────────────────────────

/// The token this test looks for, in the path and the query of every address.
const TOKEN: &str = "s3cr3t-t0k3n-5f1e77";

/// Everything every subscriber event writes, while [`CAPTURING`] is on.
static CAPTURED: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static CAPTURING: AtomicBool = AtomicBool::new(false);

struct Capture;

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if CAPTURING.load(Ordering::SeqCst) {
            CAPTURED.lock().unwrap().extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs a whole subscription's life against addresses holding a token in the
/// path and the query, with every level of every crate captured, `reqwest` and
/// its `log` records included, and looks for the token in what came out: the log
/// lines, what the commands return, and what the frontend is handed.
#[tokio::test]
async fn a_private_feeds_token_reaches_no_log_line() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::fmt()
        // The worst RUST_LOG someone can set: everything, and by name the places that
        // write a host.
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(
                [
                    "trace",
                    "reqwest::connect=trace",
                    "hyper_util::client::legacy::connect=trace",
                    "hyper_util::client::legacy::pool=trace",
                    "rustls::client::hs=trace",
                ]
                .join(","),
            )
            .expect("valid directives"),
        )
        .with_ansi(false)
        .with_writer(|| Capture)
        .finish()
        .with(crate::logging::without_connection_lines())
        .try_init()
        .expect("no other test installs a global subscriber");
    CAPTURING.store(true, Ordering::SeqCst);

    let healthy = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&healthy);
    let server = serve(move |path| {
        if path.contains("/cover") {
            return with_status("200 OK", "", b"<html>not a picture</html>");
        }
        if path.contains("/moved") {
            return redirect(&format!("/next/{TOKEN}/feed.xml?auth={TOKEN}"));
        }
        if path.contains("/looping") {
            return redirect(&format!("/looping/{TOKEN}?auth={TOKEN}"));
        }
        if path.contains("/page") {
            return with_status("200 OK", "", format!("<html>{TOKEN}</html>").as_bytes());
        }
        if path.contains("/flood") {
            return Reply::Flood;
        }
        if !state.load(Ordering::SeqCst) {
            return with_status("404 Not Found", "", TOKEN.as_bytes());
        }
        ok(&format!(
            "<rss xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel>\
             <title>Private</title><itunes:image href=\"/cover/{TOKEN}.png?auth={TOKEN}\"/>\
             <item><guid>/guid/{TOKEN}</guid><title>One</title>\
             <enclosure url=\"/audio/{TOKEN}.mp3?auth={TOKEN}\" type=\"audio/mpeg\"/></item>\
             </channel></rss>"
        ))
    })
    .await;
    let address = |path: &str| server.local(&format!("{path}/{TOKEN}/feed.xml?auth={TOKEN}"));
    let pool = enabled_pool().await;
    let leases = TestLeases(pool.clone());
    let cache = cache_dir();
    let fetcher = fetcher();

    let mut outputs = Vec::new();
    let id = subscribe(&leases, &fetcher, &address("/feed"), cache.path())
        .await
        .unwrap()
        .channel_id();

    // A refresh that fails, one that succeeds, and every way a subscription can.
    healthy.store(false, Ordering::SeqCst);
    let summary = refresh_pass(&leases, &fetcher, None, cache.path(), &Recorder::default())
        .await
        .unwrap();
    assert_eq!(summary.failed, 1, "this test meant to fail a refresh");
    outputs.push(serde_json::to_string(&summary).unwrap());
    healthy.store(true, Ordering::SeqCst);
    refresh_pass(&leases, &fetcher, None, cache.path(), &Recorder::default())
        .await
        .unwrap();
    for path in ["/moved", "/looping", "/page", "/flood"] {
        if let Err(err) = subscribe(&leases, &fetcher, &address(path), cache.path()).await {
            outputs.push(format!("{err:?}"));
            outputs.push(crate::error::AppError::from(err).to_string());
        }
    }
    let dead = {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        format!("http://127.0.0.1:{port}/{TOKEN}/feed.xml?auth={TOKEN}")
    };
    for refused in [
        dead,
        format!("file:///{TOKEN}"),
        format!("https://{TOKEN}:{TOKEN}@example.org/"),
    ] {
        let err = subscribe(&leases, &fetcher, &refused, cache.path())
            .await
            .unwrap_err();
        outputs.push(format!("{err:?}"));
        outputs.push(crate::error::AppError::from(err).to_string());
    }
    // A token in the host name: that goes into the connection, and past the code that
    // logs one. Once a name the client resolves itself, once one it asks the system for.
    let token_host = format!("{TOKEN}.feeds.example.org");
    let in_host = Fetcher::with(
        client_builder()
            .no_proxy()
            .resolve(&token_host, ([127, 0, 0, 1], server.port).into()),
        online,
        ROOMY,
    )
    .unwrap();
    subscribe(
        &leases,
        &in_host,
        &format!("http://{token_host}:{}/hosted", server.port),
        cache.path(),
    )
    .await
    .unwrap();
    // The same name over TLS, where the handshake carries it too.
    let tls_port = tls_redirecting_to(server.local("/clear")).await;
    let over_tls = Fetcher::with(
        client_builder()
            .no_proxy()
            .add_root_certificate(test_certificate())
            .resolve(&token_host, ([127, 0, 0, 1], tls_port).into()),
        online,
        ROOMY,
    )
    .unwrap();
    assert_eq!(
        fetch(&over_tls, &format!("https://{token_host}:{tls_port}/feed"))
            .await
            .err(),
        failed(FetchFailure::AddressRefused),
        "the handshake went through and the redirect was judged"
    );
    // What `rustls` says of a handshake once a dependency turns its logging on.
    tracing::debug!(target: "rustls::client::hs", "No cached session for {token_host}");
    let unresolved = format!("http://{TOKEN}.invalid/feed.xml");
    let err = subscribe(&leases, &fetcher, &unresolved, cache.path())
        .await
        .unwrap_err();
    outputs.push(format!("{err:?}"));
    let report = import_pass(
        &leases,
        &public_fetcher(&server),
        vec![OpmlEntry {
            title: Some("Listed".into()),
            feed_url: server.public(&format!("/page/{TOKEN}/feed.xml?auth={TOKEN}")),
        }],
        cache.path(),
        &Recorder::default(),
    )
    .await
    .unwrap();
    outputs.push(serde_json::to_string(&report).unwrap());
    outputs.push(serde_json::to_string(&store::list_channels(&pool).await.unwrap()).unwrap());
    outputs.push(
        serde_json::to_string(&store::list_episodes(&pool, id, 10, 0).await.unwrap()).unwrap(),
    );
    episode_life_with_a_token(&server, &mut outputs).await;

    CAPTURING.store(false, Ordering::SeqCst);
    let captured = String::from_utf8_lossy(&CAPTURED.lock().unwrap()).into_owned();
    assert!(
        captured.contains("podcast refresh failed"),
        "the capture proves nothing if it saw nothing: {captured}"
    );
    assert!(
        captured.contains("http_404"),
        "a failure is logged as its word"
    );
    assert!(
        captured.contains("LoadEpisodeAndPlay"),
        "the capture proves nothing about a load it never saw: {captured}"
    );
    assert!(
        !captured.contains(TOKEN),
        "a token reached the log:\n{captured}"
    );
    for output in outputs {
        assert!(
            !output.contains(TOKEN),
            "a token reached an answer: {output}"
        );
    }
    // The row holds the address, and that is the only place.
    let stored: String = sqlx::query_scalar("SELECT feed_url FROM podcast_channel")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.contains(TOKEN));
}

/// An episode played, sought and refused, with a token in the address of each hop,
/// and the command that carries it printed the way every logger prints one.
async fn episode_life_with_a_token(server: &Server, outputs: &mut Vec<String>) {
    use std::io::{Read, Seek, SeekFrom};

    use super::playback::{EpisodeLoad, EpisodeSource};
    use super::stream::{EnclosureSource, Gate};
    use crate::audio::engine::{AudioCmd, LastLoad, LoadIntent};

    let open = |raw: String| {
        let runtime = tokio::runtime::Handle::current();
        let gate: Gate = Arc::new(|| Box::pin(async { Ok(()) }));
        tokio::task::spawn_blocking(move || {
            let address = url(&raw);
            let origin = Origin::of(&address);
            EnclosureSource::open(runtime, fetcher(), gate, address, origin).map(|mut source| {
                // It reads what the host sent, and a seek that goes wrong says why.
                let mut sink = [0u8; 256];
                let read = source.read(&mut sink).map(|_| ());
                let sought = source.seek(SeekFrom::Start(1 << 20)).map(|_| ());
                (format!("{read:?}"), format!("{sought:?}"))
            })
        })
    };
    for path in [
        format!("/audio/{TOKEN}/ok.mp3?auth={TOKEN}"),
        format!("/moved/{TOKEN}/ok.mp3?auth={TOKEN}"),
        format!("/looping/{TOKEN}/ok.mp3?auth={TOKEN}"),
        format!("/page/{TOKEN}/ok.mp3?auth={TOKEN}"),
        format!("/missing/{TOKEN}/ok.mp3?auth={TOKEN}"),
    ] {
        match open(server.local(&path)).await.unwrap() {
            Ok((read, sought)) => {
                tracing::warn!(read, sought, "episode stream");
                outputs.push(read);
                outputs.push(sought);
            }
            Err(refusal) => {
                tracing::warn!(error = %refusal.message(), "episode open failed");
                outputs.push(format!("{refusal:?}"));
                outputs.push(refusal.message());
            }
        }
    }

    let enclosure = url(&server.local(&format!("/audio/{TOKEN}/ok.mp3?auth={TOKEN}")));
    let command = AudioCmd::LoadEpisodeAndPlay {
        intent: LoadIntent::from_raw(1),
        episode: Box::new(EpisodeLoad {
            episode_id: 1,
            channel_id: 1,
            profile_id: 1,
            track_id: -1,
            start_ms: 0,
            duration_ms: 0,
            title: "One".into(),
            channel: "Private".into(),
            artwork_path: None,
            speed: 1.0,
            source: EpisodeSource::Stream {
                origin: Origin::of(&enclosure),
                enclosure,
                declared_type: None,
            },
        }),
    };
    let captured = LastLoad::capture(&command).expect("a load");
    tracing::debug!(?command, ?captured, "dropping a load");
    outputs.push(format!("{command:?}"));
    outputs.push(format!("{captured:?}"));
}
