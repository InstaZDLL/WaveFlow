//! Measures reading one xattr from many files, the way a file manager's tag
//! sweep does, and prints files per second plus per-item p50, p99, and max.
//! Results and method: `docs/notes/named-streams-and-xattrs.md`.
//!
//! ```sh
//! cargo run --release -p smb2 --example xattr_bulk_bench -- \
//!     setup HOST:445 SHARE scratch-dir --user NAME --password-command 'CMD' \
//!     --files 2000 --tagged-every 20
//! cargo run --release -p smb2 --example xattr_bulk_bench -- \
//!     run HOST:445 SHARE scratch-dir --user NAME --password-command 'CMD' \
//!     --mode assume --window 64
//! cargo run --release -p smb2 --example xattr_bulk_bench -- \
//!     clean HOST:445 SHARE scratch-dir --user NAME --password-command 'CMD'
//! ```
//!
//! `run` modes:
//!
//! - `assume`: `BatchOptions::assume_paths_exist`: read the stream, and "not
//!   found" means no attribute.
//! - `list`: the default: list each file's streams, then read the one that exists.
//! - `probe`: the strategy `list` beat: read the stream, then check the file is
//!   there for each "not found".
//! - `serial`: `list`, one at a time.
//!
//! `--conns N` deals the items over N connections, to see whether that keeps a
//! batch moving through a stall on one of them.

use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use smb2::client::{ClientConfig, Connection, SmbClient, Tree};
use smb2::BatchOptions;

const TAG: &str = "com.apple.metadata:_kMDItemUserTags";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// The password `--password-command` prints, so it stays out of `ps`.
fn password(args: &[String]) -> String {
    let Some(command) = flag(args, "--password-command") else {
        return String::new();
    };
    let output = std::process::Command::new("sh")
        .args(["-c", &command])
        .output()
        .expect("run --password-command");
    String::from_utf8(output.stdout)
        .expect("utf-8 password")
        .trim()
        .to_string()
}

async fn connect(addr: &str, share: &str, user: &str, pass: &str) -> (SmbClient, Tree, Connection) {
    let mut client = SmbClient::connect(ClientConfig {
        addr: addr.to_string(),
        timeout: Duration::from_secs(10),
        username: user.to_string(),
        password: pass.to_string(),
        ..Default::default()
    })
    .await
    .expect("connect");
    let tree = client.connect_share(share).await.expect("share");
    let conn = client.connection_for(&tree).expect("connection");
    (client, tree, conn)
}

fn file_path(dir: &str, index: usize) -> String {
    format!("{dir}/file-{index:05}.txt")
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, addr, share, dir) = (&args[0], &args[1], &args[2], &args[3]);
    let user = flag(&args, "--user").unwrap_or_default();
    let pass = password(&args);
    let (_client, tree, mut conn) = connect(addr, share, &user, &pass).await;

    match command.as_str() {
        "setup" => {
            let files: usize = flag(&args, "--files").map_or(2000, |v| v.parse().unwrap());
            let every: usize = flag(&args, "--tagged-every").map_or(20, |v| v.parse().unwrap());
            tree.create_directory(&mut conn, dir).await.expect("mkdir");
            let paths: Vec<String> = (0..files).map(|i| file_path(dir, i)).collect();
            let started = Instant::now();
            let created: Vec<_> = stream::iter(paths.iter())
                .map(|path| {
                    let (tree, mut conn) = (tree.clone(), conn.clone());
                    async move { tree.write_file(&mut conn, path, b"x").await }
                })
                .buffer_unordered(32)
                .collect()
                .await;
            let failed = created.iter().filter(|r| r.is_err()).count();
            println!(
                "created {files} files in {:?}, {failed} failed",
                started.elapsed()
            );
            let plist = vec![0x42u8; 63];
            let items: Vec<(&str, &[u8])> = paths
                .iter()
                .step_by(every.max(1))
                .map(|p| (p.as_str(), plist.as_slice()))
                .collect();
            let tagged = tree
                .set_xattr_many(&mut conn, &items, TAG, BatchOptions::new())
                .await;
            println!(
                "tagged {} files, {} failed",
                items.len(),
                tagged.iter().filter(|r| r.is_err()).count()
            );
        }
        "run" => run(&args, addr, share, dir, &user, &pass).await,
        "clean" => {
            let entries = tree.list_directory(&mut conn, dir).await.expect("list");
            let paths: Vec<String> = entries
                .iter()
                .filter(|e| e.name != "." && e.name != "..")
                .map(|e| format!("{dir}/{}", e.name))
                .collect();
            let deleted: Vec<_> = stream::iter(paths.iter())
                .map(|path| {
                    let (tree, mut conn) = (tree.clone(), conn.clone());
                    async move { tree.delete_file(&mut conn, path).await }
                })
                .buffer_unordered(16)
                .collect()
                .await;
            println!(
                "deleted {} of {}",
                deleted.iter().filter(|r| r.is_ok()).count(),
                paths.len()
            );
            println!("rmdir: {:?}", tree.delete_directory(&mut conn, dir).await);
        }
        other => panic!("unknown command {other}"),
    }
}

async fn run(args: &[String], addr: &str, share: &str, dir: &str, user: &str, pass: &str) {
    let mode = flag(args, "--mode").unwrap_or_else(|| "assume".to_string());
    let window: usize = flag(args, "--window").map_or(64, |v| v.parse().unwrap());
    let conns: usize = flag(args, "--conns").map_or(1, |v| v.parse().unwrap());
    let repeats: usize = flag(args, "--repeats").map_or(1, |v| v.parse().unwrap());

    let mut pool = Vec::new();
    for _ in 0..conns {
        pool.push(connect(addr, share, user, pass).await);
    }
    let names: Vec<String> = pool[0]
        .1
        .list_directory(&mut pool[0].2.clone(), dir)
        .await
        .expect("list")
        .iter()
        .filter(|e| e.name != "." && e.name != "..")
        .map(|e| format!("{dir}/{}", e.name))
        .collect();

    for _ in 0..repeats {
        let started = Instant::now();
        let mut latencies = Vec::with_capacity(names.len());
        let mut found = 0usize;
        let mut failed = 0usize;
        let mut note = |result: smb2::Result<Option<Vec<u8>>>, took: Duration| {
            latencies.push(took);
            match result {
                Ok(Some(_)) => found += 1,
                Ok(None) => {}
                Err(_) => failed += 1,
            }
        };

        if mode == "serial" {
            let (_, tree, conn) = &mut pool[0];
            for path in &names {
                let begun = Instant::now();
                let result = tree.get_xattr(conn, path, TAG).await;
                note(result, begun.elapsed());
            }
        } else {
            let items = names.iter().enumerate().map(|(index, path)| {
                let (_, tree, conn) = &pool[index % conns];
                let (tree, mut conn) = (tree.clone(), conn.clone());
                let mode = mode.as_str();
                async move {
                    let begun = Instant::now();
                    let result = match mode {
                        "probe" => match tree.read_stream(&mut conn, path, TAG).await {
                            Ok(value) => Ok(Some(value)),
                            Err(e) if e.kind() == smb2::ErrorKind::NotFound => {
                                tree.stat(&mut conn, path).await.map(|_| None)
                            }
                            Err(e) => Err(e),
                        },
                        _ => {
                            let options = BatchOptions::new().assume_paths_exist(mode == "assume");
                            tree.get_xattr_many(&mut conn, &[path.as_str()], TAG, options)
                                .await
                                .remove(0)
                        }
                    };
                    (result, begun.elapsed())
                }
            });
            let mut in_flight = stream::iter(items).buffer_unordered(window);
            while let Some((result, took)) = in_flight.next().await {
                note(result, took);
            }
        }

        let wall = started.elapsed();
        latencies.sort();
        let at = |p: f64| latencies[((latencies.len() - 1) as f64 * p) as usize];
        let over_300ms = latencies
            .iter()
            .filter(|l| **l > Duration::from_millis(300))
            .count();
        println!(
            "{mode:7} window={window:<4} conns={conns} files={} found={found} failed={failed} wall={:>7.0}ms {:>7.0} files/s p50={:>6.1}ms p99={:>7.1}ms max={:>7.1}ms over300ms={over_300ms}",
            latencies.len(),
            wall.as_secs_f64() * 1000.0,
            latencies.len() as f64 / wall.as_secs_f64(),
            at(0.5).as_secs_f64() * 1000.0,
            at(0.99).as_secs_f64() * 1000.0,
            at(1.0).as_secs_f64() * 1000.0,
        );
    }
}
