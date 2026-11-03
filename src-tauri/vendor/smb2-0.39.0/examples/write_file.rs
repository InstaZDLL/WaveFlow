// Write a local file to an SMB share.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example write_file
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
// Set RUST_LOG=smb2=debug for protocol-level logging.

use std::time::Instant;

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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example write_file");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");
    let local_path = "local_file.txt";
    let remote_path = "uploaded_file.txt";

    let data = std::fs::read(local_path)?;
    println!("Read {} bytes from {}", data.len(), local_path);

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let share = client.connect_share(&share_name).await?;

    let start = Instant::now();
    let written = client
        .write_file_pipelined(&share, remote_path, &data)
        .await?;
    let elapsed = start.elapsed();

    println!(
        "Uploaded {} bytes to {} in {:.2?}",
        written, remote_path, elapsed,
    );
    if elapsed.as_secs_f64() > 0.0 {
        println!(
            "  {:.1} MB/s",
            written as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
        );
    }

    client.disconnect_share(&share).await?;

    Ok(())
}
