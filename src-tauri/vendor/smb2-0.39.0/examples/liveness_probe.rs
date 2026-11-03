// Measures what the watcher's teardown rule actually depends on: how long a
// real server can put NOTHING on the wire while a CHANGE_NOTIFY is parked on
// an otherwise-quiet session.
//
// `Connection::await_long_poll` ends a watcher only when `quiet_for` reaches
// the response deadline (30 s at the defaults) with the keepalive armed. A
// missed ECHO probe on its own does nothing. So the quantity that decides the
// risk is the SILENCE GAP -- the longest interval with zero received frames --
// not the probe drop rate, and this harness measures the gap directly.
//
// Two connections run side by side against the same server:
//
//   * "measured": keepalive off, response deadline off, one CHANGE_NOTIFY
//     parked, and this harness sends its own ECHO probes on a fixed cadence.
//     Every probe is awaited to completion (never abandoned at 5 s), so a LATE
//     reply is still timestamped -- which matters, because the shipping
//     liveness clock is refreshed by any frame at all, including a probe reply
//     that arrives after the keepalive gave up on it.
//   * "shipping": every default left alone (5 s keepalive, 30 s deadline) with
//     a real `Watcher` parked. Whether IT survives is the end-to-end verdict.
//
// Usage:
//   SMB2_HOST=192.168.1.156:445 SMB2_SHARE=PiHDD LP_SECS=180 \
//     cargo run --release --example liveness_probe
//
// Knobs (env vars, all optional):
//   SMB2_HOST   server "host:port"                 (default 127.0.0.1:10445)
//   SMB2_USER   username, empty = guest            (default empty)
//   SMB2_PASS   password
//   SMB2_SHARE  share name                         (default public)
//   LP_DIR      scratch dir to create and watch    (default liveness-probe)
//   LP_SECS     how long to observe                (default 120)
//   LP_PROBE_MS ECHO cadence in ms                 (default 5000)
//   LP_PROBE_CAP_SECS  hard bound on one probe     (default 120)
//   LP_LABEL    tag written into every output line (default "run")
//
// Every wait here is bounded and a watchdog aborts the process if the run
// overruns, so this can never hang a caller or a CI job.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smb2::client::{Connection, Session, Tree};
use smb2::msg::echo::EchoRequest;
use smb2::types::Command;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One ECHO round trip as observed by this harness.
#[derive(Debug, Clone, Copy)]
struct Probe {
    seq: u64,
    sent_ms: u128,
    /// `None` means the probe never got an answer inside `LP_PROBE_CAP_SECS`.
    replied_ms: Option<u128>,
}

struct Config {
    addr: String,
    user: String,
    pass: String,
    share: String,
    dir: String,
    secs: u64,
    probe_ms: u64,
    probe_cap: Duration,
    label: String,
}

fn config() -> Config {
    Config {
        addr: env_or("SMB2_HOST", "127.0.0.1:10445"),
        user: env_or("SMB2_USER", ""),
        pass: env_or("SMB2_PASS", ""),
        share: env_or("SMB2_SHARE", "public"),
        dir: env_or("LP_DIR", "liveness-probe"),
        secs: env_num("LP_SECS", 120),
        probe_ms: env_num("LP_PROBE_MS", 5000),
        probe_cap: Duration::from_secs(env_num("LP_PROBE_CAP_SECS", 120)),
        label: env_or("LP_LABEL", "run"),
    }
}

/// Connect, authenticate, and attach to the share.
async fn session_to(cfg: &Config) -> smb2::Result<(Connection, Tree)> {
    let mut conn = Connection::connect(&cfg.addr, Duration::from_secs(10)).await?;
    conn.negotiate().await?;
    Session::setup(&mut conn, &cfg.user, &cfg.pass, "").await?;
    let tree = Tree::connect(&mut conn, &cfg.share).await?;
    Ok((conn, tree))
}

#[tokio::main]
async fn main() {
    let _ = env_logger::try_init();
    let cfg = config();
    let t0 = Instant::now();

    // Nothing below may outlive this. The run has a known length; anything
    // past it plus a generous margin is a bug in the harness, and a harness
    // that can hang is worse than no harness.
    let cap = Duration::from_secs(cfg.secs) + cfg.probe_cap + Duration::from_secs(60);
    tokio::spawn(async move {
        tokio::time::sleep(cap).await;
        eprintln!("liveness_probe: hard cap of {cap:?} reached; aborting");
        std::process::exit(2);
    });

    println!(
        "# liveness_probe label={} host={} share={} dir={} secs={} probe_ms={}",
        cfg.label, cfg.addr, cfg.share, cfg.dir, cfg.secs, cfg.probe_ms
    );

    // ---- the measured connection -------------------------------------------
    let (mut m_conn, m_tree) = session_to(&cfg).await.expect("measured session setup");
    // We do the probing by hand so every probe's outcome is timestamped, and
    // we turn the response deadline off so a slow reply is recorded rather
    // than abandoned. Each probe is bounded below instead.
    m_conn.set_keepalive(None);
    m_conn.set_response_timeout(None);

    // A scratch directory of our own, created and removed by this run.
    let _ = m_tree.create_directory(&mut m_conn, &cfg.dir).await;

    let mut m_watcher = m_tree
        .watch(&mut m_conn, &cfg.dir, true)
        .await
        .expect("measured watch");
    let m_events = Arc::new(AtomicU64::new(0));
    let m_events_task = Arc::clone(&m_events);
    tokio::spawn(async move {
        loop {
            match m_watcher.next_events().await {
                Ok(evs) => {
                    m_events_task.fetch_add(evs.len() as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    println!("MEASURED_WATCHER_ERR,{e}");
                    return;
                }
            }
        }
    });

    // ---- the shipping-config connection ------------------------------------
    let (mut s_conn, s_tree) = session_to(&cfg).await.expect("shipping session setup");
    let s_dir = format!("{}-shipping", cfg.dir);
    let _ = s_tree.create_directory(&mut s_conn, &s_dir).await;
    let mut s_watcher = s_tree
        .watch(&mut s_conn, &s_dir, true)
        .await
        .expect("shipping watch");
    let s_fault: Arc<Mutex<Option<(u128, String)>>> = Arc::new(Mutex::new(None));
    let s_fault_task = Arc::clone(&s_fault);
    tokio::spawn(async move {
        loop {
            if let Err(e) = s_watcher.next_events().await {
                *s_fault_task.lock().unwrap() = Some((t0.elapsed().as_millis(), format!("{e}")));
                return;
            }
        }
    });

    // Diagnostics sampler: the shipping connection's own view of its probes.
    let s_diag = s_conn.clone();
    tokio::spawn(async move {
        let mut last = (0u64, 0u64, 0u64);
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let d = s_diag.diagnostics();
            let now = (
                d.metrics.keepalive_probes_sent,
                d.metrics.keepalive_failures,
                d.metrics.keepalive_probes_skipped,
            );
            if now != last {
                println!(
                    "SHIPPING_KEEPALIVE,{},{},{},{}",
                    t0.elapsed().as_millis(),
                    now.0,
                    now.1,
                    now.2
                );
                last = now;
            }
        }
    });

    // ---- the probe cadence -------------------------------------------------
    let probes: Arc<Mutex<Vec<Probe>>> = Arc::new(Mutex::new(Vec::new()));
    let deadline = t0 + Duration::from_secs(cfg.secs);
    let mut seq = 0u64;
    while Instant::now() < deadline {
        let conn = m_conn.clone();
        let probes = Arc::clone(&probes);
        let cap = cfg.probe_cap;
        let sent_ms = t0.elapsed().as_millis();
        let my_seq = seq;
        seq += 1;
        tokio::spawn(async move {
            let outcome =
                tokio::time::timeout(cap, conn.execute(Command::Echo, &EchoRequest, None)).await;
            let replied_ms = match outcome {
                // Any answer counts, including an error the server sent us: the
                // liveness clock is refreshed by the frame's arrival, not by
                // its status.
                Ok(_) => Some(t0.elapsed().as_millis()),
                Err(_) => None,
            };
            probes.lock().unwrap().push(Probe {
                seq: my_seq,
                sent_ms,
                replied_ms,
            });
        });
        tokio::time::sleep(Duration::from_millis(cfg.probe_ms)).await;
    }

    // Give the last probes their bounded chance to land.
    tokio::time::sleep(cfg.probe_cap.min(Duration::from_secs(15))).await;

    // ---- report ------------------------------------------------------------
    let mut probes = probes.lock().unwrap().clone();
    probes.sort_by_key(|p| p.seq);
    for p in &probes {
        match p.replied_ms {
            Some(r) => println!(
                "PROBE,{},{},{},{},answered",
                cfg.label,
                p.seq,
                p.sent_ms,
                r - p.sent_ms
            ),
            None => println!("PROBE,{},{},{},,unanswered", cfg.label, p.seq, p.sent_ms),
        }
    }

    // The gap that decides the risk: consecutive frames arriving on the
    // measured connection. `t0` is the reference for the first one, since the
    // session setup itself was a frame.
    let mut arrivals: Vec<u128> = probes.iter().filter_map(|p| p.replied_ms).collect();
    arrivals.sort_unstable();
    let mut prev = 0u128;
    let mut max_gap = 0u128;
    let mut gaps: Vec<u128> = Vec::new();
    for a in &arrivals {
        let gap = a - prev;
        gaps.push(gap);
        max_gap = max_gap.max(gap);
        prev = *a;
    }

    // How many probes in a row the SHIPPING keepalive would have counted as
    // missed: it gives each probe the keepalive interval to answer.
    let budget = cfg.probe_ms as u128;
    let mut run = 0u32;
    let mut worst_run = 0u32;
    for p in &probes {
        let missed = match p.replied_ms {
            Some(r) => r - p.sent_ms > budget,
            None => true,
        };
        if missed {
            run += 1;
            worst_run = worst_run.max(run);
        } else {
            run = 0;
        }
    }

    let answered = probes.iter().filter(|p| p.replied_ms.is_some()).count();
    println!(
        "SUMMARY,{},probes={},answered={},within_{}ms={},longest_consecutive_miss={},max_silence_gap_ms={}",
        cfg.label,
        probes.len(),
        answered,
        budget,
        probes
            .iter()
            .filter(|p| p.replied_ms.map(|r| r - p.sent_ms <= budget).unwrap_or(false))
            .count(),
        worst_run,
        max_gap
    );
    let mut sorted = gaps.clone();
    sorted.sort_unstable();
    if !sorted.is_empty() {
        println!(
            "GAPS,{},p50={},p90={},p99={},max={}",
            cfg.label,
            sorted[sorted.len() / 2],
            sorted[sorted.len() * 9 / 10],
            sorted[sorted.len() * 99 / 100],
            sorted[sorted.len() - 1]
        );
    }
    match &*s_fault.lock().unwrap() {
        Some((at, e)) => println!("SHIPPING_WATCHER,{},died_at_ms={at},{e}", cfg.label),
        None => println!("SHIPPING_WATCHER,{},survived", cfg.label),
    }
    println!(
        "MEASURED_WATCHER_EVENTS,{},{}",
        cfg.label,
        m_events.load(Ordering::Relaxed)
    );

    // Leave the server as we found it. Both removals go through the measured
    // connection: the shipping one is the one under test, so on any run that
    // proves the point it is already torn down and would leave its directory
    // behind.
    let _ = m_tree.delete_directory(&mut m_conn, &s_dir).await;
    let _ = m_tree.delete_directory(&mut m_conn, &cfg.dir).await;
    drop((s_conn, s_tree));
}
