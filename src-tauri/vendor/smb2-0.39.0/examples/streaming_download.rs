// Download a large file with progress reporting, without buffering everything in memory.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example streaming_download
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
// Set RUST_LOG=smb2=debug for protocol-level logging.

use std::io::Write;

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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example streaming_download");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");
    let remote_path = "big_file.zip";

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let share = client.connect_share(&share_name).await?;

    let mut download = client.download(&share, remote_path).await?;
    println!("Downloading {} bytes...", download.size());

    let mut file = std::fs::File::create(remote_path)?;
    while let Some(chunk) = download.next_chunk().await {
        let bytes = chunk?;
        file.write_all(&bytes)?;
        print!("\r{:.1}%", download.progress().percent());
    }
    println!("\nDone!");

    Ok(())
}
