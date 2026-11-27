//! Two machines finding and joining each other through `waveflow_core::lan`,
//! without the app: the check of RFC-010 lot 0 across a real network.
//!
//! On the first machine:
//!
//! ```text
//! cargo run -p waveflow-core --features lan --example lan_probe -- host
//! ```
//!
//! It prints a code. On the second:
//!
//! ```text
//! cargo run -p waveflow-core --features lan --example lan_probe -- join <CODE> [ADDR:PORT]
//! ```
//!
//! Without an address the guest looks for the host over mDNS. Each side sends
//! the other one line through the connection, checks the line it receives,
//! and exits; any other outcome is an error. `LAN_PROBE_VPN=1` turns on
//! _Allow private VPN addresses_ on either side.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{self, Message};
use waveflow_core::lan::address::PeerFilter;
use waveflow_core::lan::cert::{Fingerprint, Identity};
use waveflow_core::lan::client;
use waveflow_core::lan::code::Code;
use waveflow_core::lan::listener::{Admission, Config, Incoming, Listener};
use waveflow_core::lan::mdns::{Advertisement, Browser, Event};
use waveflow_core::lan::HANDSHAKE_TIMEOUT;

const SERVICE: &str = "_wfprobe._tcp.local.";
const CONTEXT: &str = "probe";
const FROM_HOST: &str = "hello from the host";
const FROM_GUEST: &str = "hello from the guest";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vpn = std::env::var_os("LAN_PROBE_VPN").is_some();
    match args.first().map(String::as_str) {
        Some("host") => host(vpn).await,
        Some("join") => {
            let code = args.get(1).ok_or("join <CODE> [ADDR:PORT]")?;
            join(code, args.get(2), vpn).await
        }
        _ => Err("usage: lan_probe host | join <CODE> [ADDR:PORT]".into()),
    }
}

async fn host(vpn: bool) -> Result<()> {
    let identity = Identity::generate()?;
    let (listener, mut incoming) = Listener::start(
        &identity,
        Config {
            context: CONTEXT.into(),
            allow_private_vpn: vpn,
            port: 0,
            handshake_timeout: HANDSHAKE_TIMEOUT,
        },
    )
    .await?;
    let code = Code::generate();
    listener.set_code(Some(code.clone()));
    let fingerprint = identity.fingerprint().to_string();
    let _advertised = Advertisement::start(
        SERVICE,
        listener.port(),
        &[("v", "1"), ("fp", fingerprint.as_str())],
    )?;
    println!(
        "listening on port {}, code {}, certificate {}",
        listener.port(),
        code.grouped(),
        identity.fingerprint().short()
    );
    while let Some(event) = incoming.recv().await {
        match event {
            Incoming::WrongCode { total } => println!("a wrong code ({total} so far)"),
            Incoming::Accepted(mut accepted) => {
                // Only a guest that proved the code counts for this check.
                if accepted.admission != Admission::Code {
                    println!("{} connected without the code: closed", accepted.peer);
                    continue;
                }
                println!("{} joined with the code", accepted.peer);
                accepted.socket.send(Message::text(FROM_HOST)).await?;
                expect(accepted.socket.next().await, FROM_GUEST)?;
                println!("the guest said hello: done");
                return Ok(());
            }
        }
    }
    Err("the listener stopped".into())
}

async fn join(code: &str, addr: Option<&String>, vpn: bool) -> Result<()> {
    let code = Code::parse(code).ok_or("not a code")?;
    let filter = PeerFilter::for_this_machine(vpn);
    let (addr, pin) = match addr {
        Some(addr) => (addr.parse::<SocketAddr>()?, None),
        None => find(filter).await?,
    };
    println!("joining {addr}");
    let mut joined = client::join(addr, pin, &code, CONTEXT, filter).await?;
    println!("joined, certificate {}", joined.fingerprint.short());
    expect(joined.socket.next().await, FROM_HOST)?;
    println!("the host said hello");
    joined.socket.send(Message::text(FROM_GUEST)).await?;
    let _ = joined.socket.close(None).await;
    Ok(())
}

/// The line the other side should have sent, or an error saying what came.
fn expect(got: Option<std::result::Result<Message, tungstenite::Error>>, line: &str) -> Result<()> {
    match got {
        Some(Ok(message)) if message == Message::text(line) => Ok(()),
        Some(Ok(message)) => Err(format!("expected {line:?}, got {message:?}").into()),
        Some(Err(e)) => Err(e.into()),
        None => Err("the connection closed".into()),
    }
}

/// The first host mDNS finds within 15 s, at an address this machine may
/// connect to (IPv4 first, as the browser orders them).
async fn find(filter: PeerFilter) -> Result<(SocketAddr, Option<Fingerprint>)> {
    let mut browser = Browser::start(SERVICE)?;
    let found = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = browser.next().await {
            if let Event::Found(found) = event {
                if let Some(addr) = found.addresses.iter().find(|a| filter.admits(a.ip())) {
                    let pin = found.txt.get("fp").and_then(|fp| fp.parse().ok());
                    return Some((*addr, pin));
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    found.ok_or_else(|| "no host found on the network".into())
}
