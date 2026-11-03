// Watch the diagnostics tick in real time while N file downloads run in parallel
// on a single SMB session.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example diagnostics_live -- --parallel 7 --file big.bin
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
// Flags:    --parallel N      (default 4)        — how many concurrent downloads
//           --file PATH       (env SMB2_FILE)    — file to download repeatedly
//           --interval MS     (default 150)      — how often to dump diagnostics
//           --json                               — final dump as JSON (needs --features serde)
//
// Set RUST_LOG=smb2=info for the lib's own log lines on top.

use std::sync::Arc;
use std::time::{Duration, Instant};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_val(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn arg_flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let parallel: usize = arg_val("--parallel")
        .unwrap_or_else(|| "4".to_string())
        .parse()?;
    let interval_ms: u64 = arg_val("--interval")
        .unwrap_or_else(|| "150".to_string())
        .parse()?;
    let want_json = arg_flag("--json");
    let path = arg_val("--file")
        .or_else(|| std::env::var("SMB2_FILE").ok())
        .ok_or("set --file <path> or SMB2_FILE env var")?;

    if want_json && !cfg!(feature = "serde") {
        eprintln!("--json requires building with `--features serde`");
        std::process::exit(2);
    }

    let addr = env_or("SMB2_HOST", "192.168.1.100:445");
    let user = env_or("SMB2_USER", "user");
    let pass = std::env::var("SMB2_PASS").unwrap_or_else(|_| {
        eprintln!("Set SMB2_PASS to your SMB password.");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");

    eprintln!("→ {addr} share {share_name:?} file {path:?}  ({parallel}× in parallel)\n");

    // Connect once. Each download task gets a clone of the connection so
    // they all multiplex over the same SMB session (this is exactly the
    // Phase-3 design: many concurrent execute() calls on one connection).
    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let tree = Arc::new(client.connect_share(&share_name).await?);
    let conn_template = client.connection_mut().clone();

    // Spawn the dumper task. It uses the per-connection diagnostics so it
    // doesn't need to share the SmbClient mutably with the download tasks.
    let conn_for_diag = conn_template.clone();
    let dumper_stop = Arc::new(tokio::sync::Notify::new());
    let dumper_stop_2 = dumper_stop.clone();
    let dumper = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(interval_ms));
        let start = Instant::now();
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let d = conn_for_diag.diagnostics();
                    let m = &d.metrics;
                    eprintln!(
                        "[{:>5} ms] credits={:>5}  in_flight={:>2}  next_id={:>4}  \
                         sent={:>4}  ok={:>4}  bytes ↑{:>10}  ↓{:>10}",
                        start.elapsed().as_millis(),
                        d.credits.available,
                        d.credits.in_flight,
                        d.credits.next_message_id,
                        m.requests_sent,
                        m.responses_routed_ok,
                        m.wire_bytes_sent,
                        m.wire_bytes_received,
                    );
                }
                _ = dumper_stop_2.notified() => break,
            }
        }
    });

    // Spawn N parallel downloads on connection clones.
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(parallel);
    for i in 0..parallel {
        let mut conn = conn_template.clone();
        let tree = tree.clone();
        let path = path.clone();
        tasks.push(tokio::spawn(async move {
            let r = tree.read_file_compound(&mut conn, &path).await;
            (i, r)
        }));
    }

    // Wait for them all.
    let mut total_bytes = 0u64;
    let mut errors = 0usize;
    for t in tasks {
        let (i, r) = t.await?;
        match r {
            Ok(data) => {
                eprintln!("  ✓ task #{i:>2}  {} bytes", data.len());
                total_bytes += data.len() as u64;
            }
            Err(e) => {
                eprintln!("  ✗ task #{i:>2}  {e}");
                errors += 1;
            }
        }
    }

    let elapsed = start.elapsed();
    dumper_stop.notify_one();
    let _ = dumper.await;

    eprintln!(
        "\n→ {} files in {:.2} s  ({:.1} MiB total, {:.1} MiB/s, {} errors)\n",
        parallel,
        elapsed.as_secs_f64(),
        total_bytes as f64 / (1024.0 * 1024.0),
        total_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64().max(0.001),
        errors,
    );

    // Final snapshot — the full client tree.
    let final_diag = client.diagnostics();
    if want_json {
        #[cfg(feature = "serde")]
        {
            println!("{}", serde_json::to_string_pretty(&final_diag)?);
        }
    } else {
        println!("{}", final_diag);
    }

    Ok(())
}
