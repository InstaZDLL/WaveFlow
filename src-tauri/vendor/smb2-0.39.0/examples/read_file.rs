// Read a file from an SMB share and save it to disk.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example read_file
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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example read_file");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");
    let remote_path = "report.pdf";
    let local_path = "report.pdf";

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let mut share = client.connect_share(&share_name).await?;

    let start = Instant::now();
    let data = client.read_file(&mut share, remote_path).await?;
    let elapsed = start.elapsed();

    std::fs::write(local_path, &data)?;

    println!(
        "Downloaded {} ({} bytes) in {:.2?}",
        remote_path,
        data.len(),
        elapsed,
    );
    if elapsed.as_secs_f64() > 0.0 {
        println!(
            "  {:.1} MB/s",
            data.len() as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
        );
    }

    client.disconnect_share(&share).await?;

    Ok(())
}
