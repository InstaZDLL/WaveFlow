// Show disk space for an SMB share.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example disk_space
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
// Set RUST_LOG=smb2=debug for protocol-level logging.

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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example disk_space");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let mut share = client.connect_share(&share_name).await?;

    let info = client.fs_info(&mut share).await?;
    println!("Share: {share_name}");
    println!("Total: {:.1} GB", info.total_bytes as f64 / 1e9);
    println!("Free:  {:.1} GB", info.free_bytes as f64 / 1e9);
    println!(
        "Used:  {:.1}%",
        (1.0 - info.free_bytes as f64 / info.total_bytes as f64) * 100.0
    );

    client.disconnect_share(&share).await?;

    Ok(())
}
