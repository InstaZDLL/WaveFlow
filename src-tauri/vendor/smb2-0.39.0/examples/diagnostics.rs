// Dump the diagnostics tree for an SMB connection.
//
// Usage:
//   SMB2_PASS=secret cargo run -p smb2 --example diagnostics
//   SMB2_PASS=secret cargo run -p smb2 --example diagnostics --features serde -- --json
//
// Env vars: SMB2_HOST (default "192.168.1.100:445"), SMB2_USER (default "user"),
//           SMB2_PASS (required), SMB2_SHARE (default "Documents").
//
// The default output uses the `Display` impl on `Diagnostics`, with byte
// counts humanised in this binary (the library itself prints raw u64s).
// Pass `--json` to emit JSON instead — requires building with `--features
// serde`. Set RUST_LOG=smb2=debug for protocol-level logging on top.

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let want_json = args.iter().any(|a| a == "--json");

    if want_json && !cfg!(feature = "serde") {
        eprintln!(
            "--json requires building with `--features serde`. Re-run as:\n  \
             cargo run -p smb2 --example diagnostics --features serde -- --json"
        );
        std::process::exit(2);
    }

    let addr = env_or("SMB2_HOST", "192.168.1.100:445");
    let user = env_or("SMB2_USER", "user");
    let pass = std::env::var("SMB2_PASS").unwrap_or_else(|_| {
        eprintln!("Set SMB2_PASS to your SMB password. Example:");
        eprintln!("  SMB2_PASS=secret cargo run -p smb2 --example diagnostics");
        std::process::exit(1);
    });
    let share_name = env_or("SMB2_SHARE", "Documents");

    // Make a connection, do a small amount of work so the counters have
    // something to show, then dump the snapshot.
    let mut client = smb2::connect(&addr, &user, &pass).await?;
    let mut share = client.connect_share(&share_name).await?;
    let _entries = client.list_directory(&mut share, "").await?;
    client.disconnect_share(&share).await?;

    let diag = client.diagnostics();

    if want_json {
        #[cfg(feature = "serde")]
        {
            let json = serde_json::to_string_pretty(&diag)?;
            println!("{}", json);
        }
    } else {
        print_humanised(&diag);
    }

    Ok(())
}

fn print_humanised(diag: &smb2::Diagnostics) {
    // The library's Display prints raw bytes. Re-render with IEC byte
    // formatting for human eyes. We could in principle keep the raw
    // output for tooling, but this is the example binary's job.
    let raw = format!("{}", diag);
    let humanised = raw
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("  wire bytes:        ") {
                // "X sent · Y received" — replace with humanised forms.
                rewrite_byte_line(rest)
                    .map(|s| format!("  wire bytes:        {}", s))
                    .unwrap_or_else(|| line.to_string())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    println!("{}", humanised);
}

fn rewrite_byte_line(rest: &str) -> Option<String> {
    let mut parts = rest.split(" · ");
    let sent = parts.next()?.trim_end_matches(" sent");
    let recv = parts.next()?.trim_end_matches(" received");
    let sent_n: u64 = sent.parse().ok()?;
    let recv_n: u64 = recv.parse().ok()?;
    Some(format!(
        "{} sent · {} received",
        humanize_bytes(sent_n),
        humanize_bytes(recv_n)
    ))
}

fn humanize_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}
