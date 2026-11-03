// Watch a directory for file changes (long-poll via CHANGE_NOTIFY).
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example watch_directory
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
// Set RUST_LOG=smb2=debug for protocol-level logging.

use std::time::Duration;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr = env_or("SMB2_HOST", "192.168.1.100:445");
    let user = env_or("SMB2_USER", "user");
    let pass = std::env::var("SMB2_PASS").unwrap_or_else(|_| {
        eprintln!("Set SMB2_PASS to your SMB password. Example:");
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example watch_directory");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let share = client.connect_share(&share_name).await?;

    let mut watcher = client.watch(&share, "", true).await?;
    println!("Watching for changes (press Ctrl+C to stop)...\n");

    loop {
        match tokio::time::timeout(Duration::from_secs(30), watcher.next_events()).await {
            Ok(Ok(events)) => {
                for event in &events {
                    println!("  {:?}: {}", event.action, event.filename);
                }
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
                break;
            }
            Err(_) => println!("  (no changes in 30s, still watching...)"),
        }
    }

    Ok(())
}
