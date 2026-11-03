// List files in a directory on an SMB share.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example list_directory
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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example list_directory");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");
    let directory = ""; // empty = root of the share

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let mut share = client.connect_share(&share_name).await?;

    let entries = client.list_directory(&mut share, directory).await?;

    println!(
        "{} entries in \\\\{}\\{}\\{}:",
        entries.len(),
        addr,
        share_name,
        directory
    );
    for entry in &entries {
        let kind = if entry.is_directory { "DIR " } else { "    " };
        println!("  {} {:>12} bytes  {}", kind, entry.size, entry.name);
    }

    client.disconnect_share(&share).await?;

    Ok(())
}
