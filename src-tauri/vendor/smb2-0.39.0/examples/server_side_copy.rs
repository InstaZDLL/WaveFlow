// Copy a file to another path on the same share entirely on the server, using
// server-side copy (FSCTL_SRV_COPYCHUNK). The file's bytes never cross the wire
// -- only small control messages do -- so a multi-gigabyte copy is nearly
// instant. Falls back to a read-then-write copy on servers that lack the
// feature (older Samba, some NAS firmware).
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example server_side_copy
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents"),
//           SMB2_SRC (default "source.bin"), SMB2_DST (default "copy.bin").
// Set RUST_LOG=smb2=debug for protocol-level logging.

use std::time::Instant;

use smb2::ErrorKind;

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
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example server_side_copy");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");
    let src = env_or("SMB2_SRC", "source.bin");
    let dst = env_or("SMB2_DST", "copy.bin");

    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let mut share = client.connect_share(&share_name).await?;

    let start = Instant::now();
    match client.server_side_copy_file(&share, &src, &dst).await {
        Ok(bytes) => {
            let elapsed = start.elapsed();
            println!(
                "Copied {bytes} bytes {src} -> {dst} server-side in {elapsed:.2?} \
                 (the data never crossed the wire)"
            );
        }
        Err(e) if e.kind() == ErrorKind::Unsupported => {
            // The server doesn't implement server-side copy; do it the slow way.
            eprintln!("Server has no server-side copy; falling back to read+write.");
            let data = client.read_file(&mut share, &src).await?;
            client.write_file_pipelined(&share, &dst, &data).await?;
            println!(
                "Copied {} bytes {src} -> {dst} client-side in {:.2?}",
                data.len(),
                start.elapsed()
            );
        }
        Err(e) => return Err(e.into()),
    }

    client.disconnect_share(&share).await?;
    Ok(())
}
