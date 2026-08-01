//! One task per connected MPD client.
//!
//! Owns the socket, frames requests and responses, and handles the two
//! constructs the dispatcher can't: **command lists** (several commands
//! answered as one unit) and **`idle`** (a command that deliberately
//! doesn't answer until something changes).

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;

use super::{
    commands::{dispatch, Ctx, Session},
    idle::{self, Subsystem},
    protocol::{self, Ack, Command, Response},
};

/// Guard against a client feeding us an unbounded line. MPD's own limit
/// is 64 KiB; matching it keeps a malformed or hostile peer from
/// growing our buffer without bound on a LAN-exposed socket.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// How a command list wants its per-command separator rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    /// `command_list_begin` — one `OK` at the very end.
    Silent,
    /// `command_list_ok_begin` — `list_OK` after each command.
    Verbose,
}

pub async fn handle(ctx: Ctx, stream: TcpStream, cancel: CancellationToken) -> std::io::Result<()> {
    // Nagle would coalesce our small status replies into ~40 ms of
    // added latency, which a remote's play button feels as lag.
    let _ = stream.set_nodelay(true);

    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut session = Session::new(&ctx.config);

    write_half.write_all(protocol::GREETING).await?;
    write_half.flush().await?;

    // Connection-level carry for `read_line`: bytes consumed from the reader
    // but not yet part of a complete line live here, so they survive a
    // dropped `read_line` future (e.g. `run_idle`'s `select!`). Shared across
    // every `read_line` call on this socket.
    let mut carry: Vec<u8> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            n = read_line(&mut reader, &mut carry, &mut line) => n?,
        };
        if read == 0 {
            return Ok(()); // client hung up
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);

        match trimmed {
            "command_list_begin" => {
                run_list(
                    &ctx,
                    &mut session,
                    &mut reader,
                    &mut carry,
                    &mut write_half,
                    ListMode::Silent,
                    &cancel,
                )
                .await?;
                continue;
            }
            "command_list_ok_begin" => {
                run_list(
                    &ctx,
                    &mut session,
                    &mut reader,
                    &mut carry,
                    &mut write_half,
                    ListMode::Verbose,
                    &cancel,
                )
                .await?;
                continue;
            }
            _ => {}
        }

        let Some(cmd) = protocol::parse(trimmed) else {
            continue; // blank line — MPD ignores it
        };

        // `idle` owns the connection until something changes, so it
        // can't go through the dispatcher.
        if let Command::Idle(subsystems) = cmd {
            if !session.authenticated {
                write_ack(
                    &mut write_half,
                    &Ack::new(
                        protocol::ACK_ERROR_PERMISSION,
                        "idle",
                        "you don't have permission for this command",
                    ),
                )
                .await?;
                continue;
            }
            run_idle(
                &ctx.idle,
                &mut reader,
                &mut carry,
                &mut write_half,
                &subsystems,
                &cancel,
            )
            .await?;
            continue;
        }

        let is_close = matches!(cmd, Command::Close);
        match dispatch(&ctx, &mut session, cmd).await {
            Ok(response) => write_ok(&mut write_half, &response).await?,
            Err(ack) => write_ack(&mut write_half, &ack).await?,
        }
        if is_close {
            return Ok(());
        }
    }
}

/// Read one line, refusing anything past [`MAX_LINE_BYTES`].
///
/// Deliberately not `AsyncBufReadExt::read_line`: that grows the buffer
/// until it finds a newline, so a peer on a LAN-exposed socket could
/// make us allocate without bound and only *then* hit a length check.
/// This drains `fill_buf` chunk by chunk and bails the moment the
/// accumulated line crosses the cap.
///
/// Bytes are accumulated and converted **once**, at the end: a
/// per-chunk `from_utf8_lossy` would corrupt any multi-byte character
/// straddling a buffer boundary, and arguments carry file paths.
///
/// **Cancellation-safe.** Bytes already `consume`d from the `BufReader`
/// accumulate in the caller-owned `carry` buffer, not a local one, so if
/// this future is dropped mid-line (the `idle::wait` branch of `run_idle`'s
/// `select!` winning while a client's command is still arriving in fragments)
/// the partial data survives and the next `read_line` call resumes from it.
/// `carry` is cleared only once a complete line has been handed back.
///
/// An over-long line is an error rather than a truncation, because a
/// truncated command would parse as a different — valid — one.
async fn read_line<R>(
    reader: &mut BufReader<R>,
    carry: &mut Vec<u8>,
    out: &mut String,
) -> std::io::Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break; // EOF
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(index) => {
                carry.extend_from_slice(&available[..=index]);
                reader.consume(index + 1);
                break;
            }
            None => {
                let len = available.len();
                carry.extend_from_slice(available);
                reader.consume(len);
                if carry.len() > MAX_LINE_BYTES {
                    // The connection is torn down on this error, so the
                    // leftover `carry` never gets reused.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "command line too long",
                    ));
                }
            }
        }
    }
    let read = carry.len();
    out.push_str(&String::from_utf8_lossy(carry));
    carry.clear();
    Ok(read)
}

/// Collect commands until `command_list_end`, run them, answer once.
///
/// MPD semantics: the list aborts at the first failure, and the ACK
/// carries the 0-based index of the command that failed so the client
/// knows how far it got.
async fn run_list<R, W>(
    ctx: &Ctx,
    session: &mut Session,
    reader: &mut BufReader<R>,
    carry: &mut Vec<u8>,
    writer: &mut W,
    mode: ListMode,
    cancel: &CancellationToken,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut pending: Vec<Command> = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        let read = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            n = read_line(reader, carry, &mut line) => n?,
        };
        if read == 0 {
            return Ok(());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "command_list_end" {
            break;
        }
        if let Some(cmd) = protocol::parse(trimmed) {
            pending.push(cmd);
        }
    }

    let mut body = String::new();
    for (index, cmd) in pending.into_iter().enumerate() {
        match dispatch(ctx, session, cmd).await {
            Ok(response) => {
                body.push_str(&response.encode());
                if mode == ListMode::Verbose {
                    body.push_str("list_OK\n");
                }
            }
            Err(mut ack) => {
                ack.list_index = index as u32;
                body.push_str(&ack.encode());
                writer.write_all(body.as_bytes()).await?;
                writer.flush().await?;
                return Ok(());
            }
        }
    }
    body.push_str("OK\n");
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Hold the connection until a subsystem the client cares about
/// changes, or until it sends `noidle`.
///
/// An empty subsystem list means "any", which is what a bare `idle`
/// sends and what most remotes actually use.
async fn run_idle<R, W>(
    bus: &idle::IdleBus,
    reader: &mut BufReader<R>,
    carry: &mut Vec<u8>,
    writer: &mut W,
    requested: &[String],
    cancel: &CancellationToken,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let wanted: Vec<Subsystem> = if requested.is_empty() {
        Subsystem::ALL.to_vec()
    } else {
        requested
            .iter()
            .filter_map(|s| Subsystem::parse(s))
            .collect()
    };

    // Subscribe *before* awaiting so a change landing between the
    // client's `idle` and our wait isn't missed.
    let mut rx = bus.subscribe();
    let mut interrupt = String::new();

    let changed = tokio::select! {
        hits = idle::wait(&mut rx, &wanted, cancel) => hits,
        // Anything arriving on the socket ends the idle. MPD only
        // allows `noidle` here; other commands are undefined, and
        // answering a bare OK (as MPD does) keeps clients in sync.
        // `read_line` shares the connection carry, so if `idle::wait` wins
        // this race any partially-read command is preserved for the next read.
        read = read_line(reader, carry, &mut interrupt) => {
            match read {
                Ok(0) => return Ok(()),   // client hung up mid-idle
                Ok(_) => Vec::new(),
                Err(err) => return Err(err),
            }
        }
    };

    let mut response = Response::new();
    for subsystem in changed {
        response.push("changed", subsystem.as_str());
    }
    writer.write_all(response.encode().as_bytes()).await?;
    writer.write_all(b"OK\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn write_ok<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    response: &Response,
) -> std::io::Result<()> {
    writer.write_all(response.encode().as_bytes()).await?;
    writer.write_all(b"OK\n").await?;
    writer.flush().await
}

async fn write_ack<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    ack: &Ack,
) -> std::io::Result<()> {
    writer.write_all(ack.encode().as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpd::idle::IdleBus;
    use tokio::io::AsyncReadExt;

    /// Drive `run_idle` over an in-memory pipe. Returns what the server
    /// wrote back.
    ///
    /// `run_idle` is the one piece of the connection layer that owns
    /// non-trivial control flow (hold the socket, race the bus against
    /// the reader) *and* is free of `AppHandle`, so it is the part
    /// worth testing directly.
    async fn drive_idle(
        bus: &IdleBus,
        client_sends: &str,
        wanted: &[&str],
        after_subscribe: impl FnOnce(),
    ) -> String {
        let (client, server) = tokio::io::duplex(1024);
        let (server_read, mut server_write) = tokio::io::split(server);
        let mut reader = BufReader::new(server_read);
        let requested: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
        let cancel = CancellationToken::new();

        let (mut client_read, mut client_write) = tokio::io::split(client);
        if !client_sends.is_empty() {
            client_write
                .write_all(client_sends.as_bytes())
                .await
                .unwrap();
        }

        let task = tokio::spawn({
            let bus = bus.clone();
            async move {
                let mut carry: Vec<u8> = Vec::new();
                run_idle(
                    &bus,
                    &mut reader,
                    &mut carry,
                    &mut server_write,
                    &requested,
                    &cancel,
                )
                .await
                .unwrap();
            }
        });

        // Give the task a chance to subscribe before the change fires,
        // otherwise the notification lands before anyone is listening.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        after_subscribe();

        task.await.unwrap();

        let mut buf = vec![0u8; 256];
        let n = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client_read.read(&mut buf),
        )
        .await
        .unwrap_or(Ok(0))
        .unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    #[tokio::test]
    async fn read_line_splits_on_the_newline_and_keeps_utf8_intact() {
        // "Café" is deliberately multi-byte: a per-chunk lossy decode
        // would mangle it if the é straddled a buffer boundary.
        let (mut client, server) = tokio::io::duplex(64);
        client
            .write_all("play \"Café\"\nnext\n".as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(server);

        let mut carry: Vec<u8> = Vec::new();
        let mut line = String::new();
        read_line(&mut reader, &mut carry, &mut line).await.unwrap();
        assert_eq!(line, "play \"Café\"\n");

        line.clear();
        read_line(&mut reader, &mut carry, &mut line).await.unwrap();
        assert_eq!(line, "next\n");
    }

    #[tokio::test]
    async fn read_line_refuses_an_unterminated_flood() {
        // A peer that never sends a newline must not be able to grow our
        // buffer without bound — the cap has to bite during the read,
        // not after it.
        let (mut client, server) = tokio::io::duplex(8 * 1024);
        tokio::spawn(async move {
            let chunk = vec![b'a'; 8 * 1024];
            // Write past MAX_LINE_BYTES with no newline in sight.
            for _ in 0..16 {
                if client.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });

        let mut reader = BufReader::new(server);
        let mut carry: Vec<u8> = Vec::new();
        let mut line = String::new();
        let err = read_line(&mut reader, &mut carry, &mut line)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn idle_answers_with_the_changed_subsystem() {
        let bus = IdleBus::new();
        let out = drive_idle(&bus, "", &["player"], || {
            bus.notify(idle::Subsystem::Player);
        })
        .await;
        assert_eq!(out, "changed: player\nOK\n");
    }

    #[tokio::test]
    async fn noidle_ends_the_wait_with_a_bare_ok() {
        // No `changed` line: nothing actually changed, the client just
        // asked to stop waiting.
        let bus = IdleBus::new();
        let out = drive_idle(&bus, "noidle\n", &["player"], || {}).await;
        assert_eq!(out, "OK\n");
    }

    #[tokio::test]
    async fn a_bare_idle_listens_to_every_subsystem() {
        let bus = IdleBus::new();
        // Empty `wanted` means the client sent `idle` with no arguments,
        // which must not be read as "wants nothing".
        let out = drive_idle(&bus, "", &[], || {
            bus.notify(idle::Subsystem::Mixer);
        })
        .await;
        assert_eq!(out, "changed: mixer\nOK\n");
    }

    #[tokio::test]
    async fn an_unknown_subsystem_name_is_dropped_not_fatal() {
        let bus = IdleBus::new();
        // `database` is real MPD but something we never fire, so it is
        // filtered out; the `player` alongside it must still work.
        let out = drive_idle(&bus, "", &["database", "player"], || {
            bus.notify(idle::Subsystem::Player);
        })
        .await;
        assert_eq!(out, "changed: player\nOK\n");
    }
}
