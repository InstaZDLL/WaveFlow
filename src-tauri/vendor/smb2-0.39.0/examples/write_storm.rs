// Repro harness for the "NAS goes silent under concurrent streamed writes" wedge.
//
// Mimics cmdr's bulk-copy shape with no cmdr in the picture: a sliding window of
// N concurrent per-file tasks against ONE SMB session, each file going through
// either the compound CREATE+WRITE+FLUSH (+ CLOSE) fast path (small) or the
// pipelined `FileWriter` (large). Optionally a second connection long-polling
// CHANGE_NOTIFY, matching cmdr's watcher.
//
// Usage:
//   SMB2_HOST=127.0.0.1:10445 SMB2_SHARE=public cargo run --release --example write_storm
//   SMB2_HOST=192.168.1.156:445 SMB2_USER=pi SMB2_PASS=... WS_CONC=8 cargo run --release --example write_storm
//
// Knobs (all env vars, all optional):
//   SMB2_HOST   server "host:port"          (default 127.0.0.1:10445)
//   SMB2_USER   username, empty = guest     (default empty)
//   SMB2_PASS   password
//   SMB2_SHARE  share name                  (default public)
//   WS_DIR      remote dir to write into    (default write-storm)
//   WS_FILES    how many files              (default 64)
//   WS_CONC     concurrent per-file tasks   (default 8)
//   WS_LARGE    bytes for a "large" file    (default 12800000)
//   WS_SMALL    bytes for a "small" file    (default 520345)
//   WS_LARGE_EVERY  1-in-N files are large  (default 2)
//   WS_PUSH     source chunk size pushed into FileWriter (default 524288)
//   WS_WATCH    1 = also run a CHANGE_NOTIFY watcher on a 2nd connection
//   WS_RESP_TIMEOUT_SECS  smb2 response deadline, 0 = disabled (default 180)
//   WS_STALL_SECS  declare a wedge after this long with no file completing (default 60)
//   WS_KEEP     1 = leave the written files behind (default: delete them)
//
// Set RUST_LOG=smb2=debug (or trace) for protocol logging.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smb2::client::{ClientConfig, Connection, SmbClient, Tree};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(key: &str) -> bool {
    matches!(env_or(key, "0").as_str(), "1" | "true" | "yes")
}

struct Config {
    addr: String,
    user: String,
    pass: String,
    share: String,
    dir: String,
    files: u64,
    conc: usize,
    large: usize,
    small: usize,
    large_every: u64,
    push: usize,
    watch: bool,
    resp_timeout: Option<Duration>,
    stall: Duration,
    keep: bool,
    /// Abort 1-in-N copy tasks mid-flight, after WS_CANCEL_AFTER_MS.
    cancel_every: u64,
    cancel_after_ms: u64,
}

fn config() -> Config {
    let resp = env_num("WS_RESP_TIMEOUT_SECS", 180);
    Config {
        addr: env_or("SMB2_HOST", "127.0.0.1:10445"),
        user: env_or("SMB2_USER", ""),
        pass: env_or("SMB2_PASS", ""),
        share: env_or("SMB2_SHARE", "public"),
        dir: env_or("WS_DIR", "write-storm"),
        files: env_num("WS_FILES", 64),
        conc: env_num("WS_CONC", 8) as usize,
        large: env_num("WS_LARGE", 12_800_000) as usize,
        small: env_num("WS_SMALL", 520_345) as usize,
        large_every: env_num("WS_LARGE_EVERY", 2).max(1),
        push: env_num("WS_PUSH", 524_288) as usize,
        watch: env_flag("WS_WATCH"),
        resp_timeout: (resp > 0).then(|| Duration::from_secs(resp)),
        stall: Duration::from_secs(env_num("WS_STALL_SECS", 60)),
        keep: env_flag("WS_KEEP"),
        cancel_every: env_num("WS_CANCEL_EVERY", 0),
        cancel_after_ms: env_num("WS_CANCEL_AFTER_MS", 40),
    }
}

/// Shared progress, so the monitor task can tell "slow" from "wedged".
struct Progress {
    files_done: AtomicU64,
    bytes_done: AtomicU64,
    last_change_ms: AtomicU64,
    wedged: AtomicBool,
    started: Instant,
}

impl Progress {
    fn new() -> Self {
        Self {
            files_done: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            last_change_ms: AtomicU64::new(0),
            wedged: AtomicBool::new(false),
            started: Instant::now(),
        }
    }

    fn record(&self, bytes: u64) {
        self.files_done.fetch_add(1, Ordering::Relaxed);
        self.bytes_done.fetch_add(bytes, Ordering::Relaxed);
        self.last_change_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        let last = self.last_change_ms.load(Ordering::Relaxed);
        self.started
            .elapsed()
            .saturating_sub(Duration::from_millis(last))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cfg = config();

    println!(
        "write_storm: {}/{} dir={} files={} conc={} large={}B small={}B (1 in {}) push={}B watch={} resp_timeout={:?}",
        cfg.addr,
        cfg.share,
        cfg.dir,
        cfg.files,
        cfg.conc,
        cfg.large,
        cfg.small,
        cfg.large_every,
        cfg.push,
        cfg.watch,
        cfg.resp_timeout,
    );
    if cfg.cancel_every > 0 {
        println!(
            "  cancelling 1-in-{} tasks after {}ms",
            cfg.cancel_every, cfg.cancel_after_ms
        );
    }

    let mut client = SmbClient::connect(ClientConfig {
        addr: cfg.addr.clone(),
        timeout: Duration::from_secs(10),
        username: cfg.user.clone(),
        password: cfg.pass.clone(),
        domain: String::new(),
        auto_reconnect: false,
        compression: false,
        dfs_enabled: false,
        dfs_target_overrides: HashMap::new(),
        connect_options: None,
        ..Default::default()
    })
    .await?;

    if let Some(p) = client.params() {
        println!(
            "negotiated: dialect={:?} max_write={} max_read={} max_trans={}",
            p.dialect, p.max_write_size, p.max_read_size, p.max_transact_size
        );
    }
    client
        .connection_mut()
        .set_response_timeout(cfg.resp_timeout);
    client
        .connection_mut()
        .set_stale_request_warning(Some(Duration::from_secs(10)));

    let tree = client.connect_share(&cfg.share).await?;
    // Fresh destination dir. Ignore "already exists".
    let _ = tree
        .create_directory(client.connection_mut(), &cfg.dir)
        .await;

    let max_write = client.params().map(|p| p.max_write_size).unwrap_or(65536) as usize;
    let conn = client.connection_mut().clone();
    let tree = Arc::new(tree);

    let watcher = if cfg.watch {
        Some(spawn_watcher(&cfg).await?)
    } else {
        None
    };

    let progress = Arc::new(Progress::new());
    let monitor = spawn_monitor(conn.clone(), Arc::clone(&progress), cfg.stall);

    let large_body = Arc::new(vec![0x5au8; cfg.large]);
    let small_body = Arc::new(vec![0x2bu8; cfg.small]);

    let mut in_flight = futures_util::stream::FuturesUnordered::new();
    let mut next = 0u64;
    let mut failures: Vec<String> = Vec::new();

    loop {
        while in_flight.len() < cfg.conc && next < cfg.files {
            let idx = next;
            next += 1;
            let is_large = idx.is_multiple_of(cfg.large_every);
            let body = if is_large {
                Arc::clone(&large_body)
            } else {
                Arc::clone(&small_body)
            };
            let path = format!("{}/file-{:05}.bin", cfg.dir, idx);
            let tree = Arc::clone(&tree);
            let conn = conn.clone();
            let progress = Arc::clone(&progress);
            let push = cfg.push;
            let cancel = cfg.cancel_every > 0 && idx % cfg.cancel_every == cfg.cancel_every - 1;
            let cancel_after = Duration::from_millis(cfg.cancel_after_ms);
            in_flight.push(tokio::spawn(async move {
                if cancel {
                    // Abort the copy mid-flight, exactly like a user cancel or a
                    // dropped task: the `write_chunk` future is dropped wherever
                    // it happens to be, including inside `TcpTransport::send`.
                    let handle = tokio::spawn({
                        let (tree, conn, path, body) = (
                            Arc::clone(&tree),
                            conn.clone(),
                            path.clone(),
                            Arc::clone(&body),
                        );
                        async move { copy_one(tree, conn, &path, &body, max_write, push).await }
                    });
                    tokio::time::sleep(cancel_after).await;
                    handle.abort();
                    log::debug!("cancelled path={path:?}");
                    return Ok(0u64);
                }
                let started = Instant::now();
                let result = copy_one(tree, conn, &path, &body, max_write, push).await;
                match &result {
                    Ok(n) => {
                        progress.record(*n);
                        log::debug!(
                            "done ({} bytes) in {:?} path={path:?}",
                            n,
                            started.elapsed()
                        );
                    }
                    Err(e) => {
                        log::warn!("FAILED after {:?} path={path:?}: {}", started.elapsed(), e)
                    }
                }
                result.map_err(|e| format!("{path}: {e}"))
            }));
        }

        if in_flight.is_empty() {
            break;
        }

        use futures_util::StreamExt;
        match in_flight.next().await {
            Some(Ok(Ok(_))) => {}
            Some(Ok(Err(e))) => failures.push(e),
            Some(Err(e)) => failures.push(format!("task panicked/aborted: {e}")),
            None => break,
        }

        if progress.wedged.load(Ordering::Relaxed) {
            break;
        }
    }

    let elapsed = progress.started.elapsed();
    let done = progress.files_done.load(Ordering::Relaxed);
    let bytes = progress.bytes_done.load(Ordering::Relaxed);
    println!(
        "\nfinished: {}/{} files, {:.1} MiB in {:.1?} ({:.1} MiB/s), {} failure(s)",
        done,
        cfg.files,
        bytes as f64 / (1024.0 * 1024.0),
        elapsed,
        bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(0.001),
        failures.len(),
    );
    for f in failures.iter().take(20) {
        println!("  failure: {f}");
    }
    println!("{:#?}", conn.diagnostics());

    monitor.abort();
    if let Some(w) = watcher {
        w.abort();
    }

    if !cfg.keep && failures.is_empty() && done == cfg.files {
        let paths: Vec<String> = (0..cfg.files)
            .map(|i| format!("{}/file-{:05}.bin", cfg.dir, i))
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let mut c = conn.clone();
        let _ = tree.delete_files(&mut c, &refs).await;
        let _ = tree.delete_directory(&mut c, &cfg.dir).await;
    }

    if progress.wedged.load(Ordering::Relaxed) {
        println!("VERDICT: WEDGED");
        std::process::exit(2);
    }
    if !failures.is_empty() {
        println!("VERDICT: FAILED");
        std::process::exit(1);
    }
    println!("VERDICT: OK");
    Ok(())
}

/// One file, taking exactly the branch cmdr's `write_from_stream` would take.
async fn copy_one(
    tree: Arc<Tree>,
    conn: Connection,
    path: &str,
    body: &[u8],
    max_write: usize,
    push: usize,
) -> Result<u64, smb2::Error> {
    if !body.is_empty() && body.len() <= max_write {
        let mut conn = conn;
        return tree.write_file_compound(&mut conn, path, body).await;
    }

    let mut writer = tree.create_file_writer(conn, path).await?;
    log::debug!("stream: open_file_writer path={path:?}");
    let mut offset = 0usize;
    while offset < body.len() {
        let end = (offset + push).min(body.len());
        if let Err(e) = writer.write_chunk(&body[offset..end]).await {
            let _ = writer.abort().await;
            return Err(e);
        }
        offset = end;
    }
    writer.finish().await
}

/// Background reporter: prints the in-flight table and flags a wedge.
fn spawn_monitor(
    conn: Connection,
    progress: Arc<Progress>,
    stall: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let outstanding = conn.outstanding_requests();
            let idle = progress.idle_for();
            let m = conn.diagnostics();
            let oldest = outstanding.first().map(|r| r.age).unwrap_or_default();
            let mut by_cmd: HashMap<String, usize> = HashMap::new();
            for r in &outstanding {
                *by_cmd.entry(format!("{:?}", r.command)).or_default() += 1;
            }
            let mut summary: Vec<String> = by_cmd.iter().map(|(k, v)| format!("{k}x{v}")).collect();
            summary.sort();
            println!(
                "[{:>6.1?}] files={} MiB={:.1} idle={:.1?} outstanding={} [{}] oldest={:.1?} credits={} next_msg_id={} sent={} waits={} starv={} timeouts={} tx_bytes={} rx_bytes={}",
                progress.started.elapsed(),
                progress.files_done.load(Ordering::Relaxed),
                progress.bytes_done.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0),
                idle,
                outstanding.len(),
                summary.join(" "),
                oldest,
                m.credits.available,
                m.credits.next_message_id,
                m.metrics.requests_sent,
                m.metrics.credit_waits,
                m.metrics.credit_starvations,
                m.metrics.response_timeouts,
                m.metrics.wire_bytes_sent,
                m.metrics.wire_bytes_received,
            );
            if idle >= stall && !outstanding.is_empty() {
                progress.wedged.store(true, Ordering::Relaxed);
                println!("\n=== WEDGE DETECTED (no file completed for {idle:.1?}) ===");
                for r in outstanding.iter().take(40) {
                    println!(
                        "  outstanding: {:?} msg_id={} age={:.1?}",
                        r.command, r.message_id, r.age
                    );
                }
                println!("{m:#?}");
            }
        }
    })
}

/// A CHANGE_NOTIFY long-poll on its own connection, like cmdr's SMB watcher.
async fn spawn_watcher(cfg: &Config) -> Result<tokio::task::JoinHandle<()>, smb2::Error> {
    let mut client = SmbClient::connect(ClientConfig {
        addr: cfg.addr.clone(),
        timeout: Duration::from_secs(10),
        username: cfg.user.clone(),
        password: cfg.pass.clone(),
        domain: String::new(),
        auto_reconnect: false,
        compression: false,
        dfs_enabled: false,
        dfs_target_overrides: HashMap::new(),
        connect_options: None,
        ..Default::default()
    })
    .await?;
    let tree = client.connect_share(&cfg.share).await?;
    let dir = cfg.dir.clone();
    let mut watcher = client.watch(&tree, &dir, true).await?;
    Ok(tokio::spawn(async move {
        // Hold the client so the connection outlives this task's first await.
        let _client = client;
        loop {
            match watcher.next_events().await {
                Ok(events) => log::debug!("watcher: {} event(s)", events.len()),
                Err(e) => {
                    log::warn!("watcher died: {e}");
                    break;
                }
            }
        }
    }))
}
