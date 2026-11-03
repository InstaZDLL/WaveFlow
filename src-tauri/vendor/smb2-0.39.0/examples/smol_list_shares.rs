// List available shares on an SMB server, on the smol runtime instead of tokio.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example smol_list_shares --features smol
//
// A consumer that only uses smol turns tokio off entirely:
//   smb2 = { version = "...", default-features = false, features = ["smol"] }
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required).
// Set RUST_LOG=smb2=debug for protocol-level logging.

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr = env_or("SMB2_HOST", "192.168.1.100:445");
    let user = env_or("SMB2_USER", "user");
    let pass = std::env::var("SMB2_PASS").unwrap_or_else(|_| {
        eprintln!("Set SMB2_PASS to your SMB password. Example:");
        eprintln!(
            "  SMB2_PASS=secret cargo run -p smb2 --example smol_list_shares --features smol"
        );
        std::process::exit(1);
    });

    smol::block_on(async {
        let mut client = smb2::connect(&addr, &user, &pass).await?;

        println!("Connected to {addr}");
        if let Some(params) = client.params() {
            println!("Dialect: {}", params.dialect);
        }

        let shares = client.list_shares().await?;

        println!("\nShares ({} total):", shares.len());
        for share in &shares {
            if share.comment.is_empty() {
                println!("  {}", share.name);
            } else {
                println!("  {} - {}", share.name, share.comment);
            }
        }

        Ok(())
    })
}
