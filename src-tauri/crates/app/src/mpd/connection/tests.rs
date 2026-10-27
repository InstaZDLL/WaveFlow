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

/// What a browser sends when a web page posts to the port: the request
/// line ends the connection before any header or body line is read.
#[test]
fn an_http_request_is_a_foreign_line() {
    for line in [
        "GET / HTTP/1.1",
        "POST / HTTP/1.1",
        "OPTIONS * HTTP/1.1",
        "PRI * HTTP/2.0",
        "Host: 192.168.1.42:6600",
        "Content-Type: text/plain",
        "  POST / HTTP/1.1",
        "1",
        "\"play\"",
    ] {
        assert!(is_foreign_line(line), "{line:?} must close the connection");
        assert!(refuse_foreign_line(line).is_err());
    }
}

/// Every real command starts lowercase, list markers included; a blank
/// line stays skipped rather than closing.
#[test]
fn mpd_commands_are_not_foreign() {
    for (i, line) in [
        "status",
        "play 3",
        "setvol 100",
        "password \"Hunter2\"",
        "command_list_ok_begin",
        "command_list_end",
        "  status",
        "",
        "   ",
    ]
    .iter()
    .enumerate()
    {
        // Named by index: one of these lines carries a fixture password.
        assert!(!is_foreign_line(line), "command #{i} must be served");
        assert!(refuse_foreign_line(line).is_ok());
    }
}
