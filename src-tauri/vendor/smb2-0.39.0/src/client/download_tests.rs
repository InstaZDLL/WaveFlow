//! `FileDownload` against a mock server: delivery order, short reads, EOF,
//! errors, the window bound, cancel safety, closing, the adaptive window's
//! timing, and the rate hint a download leaves on its connection.
//!
//! The mock answers READs in the order they were sent (FIFO pairing), so a
//! test controls what each READ gets by the order it queues responses; one
//! addressed to a READ's `MessageId` (`read_answer_for`) answers out of order. Tests
//! about timing run on tokio's paused clock, so "the READ took a second" is
//! exact and costs no wall time.

use std::sync::Arc;
use std::time::Duration;

use crate::client::read_ahead::{Outstanding, Window};
use crate::client::stream::{FileDownload, ReadAhead};
use crate::client::test_helpers::{
    build_close_error_response, build_close_response, build_read_error_response,
    build_read_response, setup_connection, setup_connection_with_max_read,
};
use crate::client::tree::Tree;
use crate::msg::header::Header;
use crate::msg::read::ReadRequest;
use crate::pack::{ReadCursor, Unpack};
use crate::transport::MockTransport;
use crate::types::status::NtStatus;
use crate::types::{Command, FileId, TreeId};
use crate::Error;

const CHUNK: u32 = 65536;

fn test_tree() -> Tree {
    Tree {
        pin: None,
        tree_id: TreeId(10),
        share_name: "test".to_string(),
        server: "test-server".to_string(),
        is_dfs: false,
        encrypt_data: false,
        dfs_origin: None,
    }
}

fn test_file_id() -> FileId {
    FileId {
        persistent: 0xAA,
        volatile: 0xBB,
    }
}

/// Offsets and lengths of every READ the mock saw, in send order.
fn sent_reads(mock: &MockTransport) -> Vec<(u64, u32)> {
    mock.sent_messages()
        .iter()
        .filter_map(|bytes| {
            let header = Header::unpack(&mut ReadCursor::new(bytes)).ok()?;
            if header.command != Command::Read {
                return None;
            }
            ReadRequest::unpack(&mut ReadCursor::new(&bytes[Header::SIZE..]))
                .ok()
                .map(|r| (r.offset, r.length))
        })
        .collect()
}

fn chunk_of(i: u8) -> Vec<u8> {
    vec![i; CHUNK as usize]
}

// ── Fixed windows ──────────────────────────────────────────────────────

#[tokio::test]
async fn read_ahead_delivers_chunks_in_file_order() {
    let mock = Arc::new(MockTransport::new());
    for i in 1..=4u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(4));

    let mut chunks = Vec::new();
    while let Some(chunk) = download.next_chunk().await {
        chunks.push(chunk.unwrap());
    }
    assert_eq!(download.peak_in_flight_bytes(), 4 * 65536);
    drop(download);

    assert_eq!(chunks, (1..=4u8).map(chunk_of).collect::<Vec<_>>());
    assert_eq!(
        sent_reads(&mock),
        vec![(0, CHUNK), (65536, CHUNK), (131072, CHUNK), (196608, CHUNK)]
    );
    // 4 READs + CLOSE.
    assert_eq!(mock.sent_count(), 5);
}

#[tokio::test]
async fn read_ahead_short_read_rerequests_remainder_before_later_chunks() {
    let mock = Arc::new(MockTransport::new());
    // FIFO pairing: READ@0 gets 32 KiB of 1s, READ@64K gets 64 KiB of 2s,
    // the remainder READ@32K gets 32 KiB of 3s.
    mock.queue_response(build_read_response(vec![1u8; 32768]));
    mock.queue_response(build_read_response(vec![2u8; 65536]));
    mock.queue_response(build_read_response(vec![3u8; 32768]));
    mock.queue_response(build_close_response());

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 2 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(2));

    let mut chunks = Vec::new();
    while let Some(chunk) = download.next_chunk().await {
        chunks.push(chunk.unwrap());
    }
    assert_eq!(download.bytes_received(), 2 * 65536);
    drop(download);

    assert_eq!(
        chunks,
        vec![vec![1u8; 32768], vec![3u8; 32768], vec![2u8; 65536]]
    );
    assert_eq!(
        sent_reads(&mock),
        vec![(0, CHUNK), (65536, CHUNK), (32768, 32768)]
    );
}

#[tokio::test]
async fn read_ahead_end_of_file_mid_window_stops_and_closes() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_response(chunk_of(1)));
    mock.queue_response(build_read_error_response(NtStatus::END_OF_FILE));
    mock.queue_response(build_read_error_response(NtStatus::END_OF_FILE));
    mock.queue_response(build_close_response());

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 3 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(3));

    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    assert!(download.next_chunk().await.is_none());
    assert!(download.next_chunk().await.is_none());
    drop(download);

    // 3 READs + 1 CLOSE, and nothing after.
    assert_eq!(sent_reads(&mock).len(), 3);
    assert_eq!(mock.sent_count(), 4);
}

#[tokio::test]
async fn read_ahead_error_stops_without_more_reads() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_error_response(NtStatus::ACCESS_DENIED));
    mock.queue_response(build_read_response(chunk_of(2)));

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(2));

    assert!(matches!(
        download.next_chunk().await,
        Some(Err(Error::Protocol {
            status: NtStatus::ACCESS_DENIED,
            command: Command::Read,
        }))
    ));
    assert!(download.next_chunk().await.is_none());
    drop(download);
    assert_eq!(sent_reads(&mock).len(), 2);
}

#[tokio::test]
async fn read_ahead_window_bounds_reads_in_flight() {
    let mock = Arc::new(MockTransport::new());
    for i in 1..=6u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 6 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(2));

    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    // Two sent up front, one more after the first delivery.
    assert_eq!(sent_reads(&mock).len(), 3);
    assert_eq!(download.peak_in_flight_bytes(), 2 * 65536);
    let mut n = 1;
    while let Some(chunk) = download.next_chunk().await {
        n += 1;
        assert_eq!(chunk.unwrap(), chunk_of(n));
    }
    assert_eq!(n, 6);
}

// ── Whatever the policy ────────────────────────────────────────────────

#[tokio::test]
async fn a_file_that_fits_one_chunk_costs_one_read() {
    for policy in [
        ReadAhead::Adaptive,
        ReadAhead::SEQUENTIAL,
        ReadAhead::Fixed(8),
    ] {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_read_response(vec![7u8; 1000]));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let data = FileDownload::new(&tree, &mut conn, test_file_id(), 1000, CHUNK)
            .with_read_ahead(policy)
            .collect()
            .await
            .unwrap();

        assert_eq!(data, vec![7u8; 1000]);
        assert_eq!(sent_reads(&mock), vec![(0, 1000)], "{policy:?}");
        // READ + CLOSE.
        assert_eq!(mock.sent_count(), 2, "{policy:?}");
    }
}

#[tokio::test]
async fn dropping_next_chunk_while_a_read_is_out_loses_nothing() {
    let mock = Arc::new(MockTransport::new());
    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 3 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(2));

    // A `select!` arm losing: the future goes away with two READs out.
    let first = tokio::time::timeout(Duration::from_millis(50), download.next_chunk()).await;
    assert!(first.is_err(), "nothing was answered yet");
    assert_eq!(sent_reads(&mock).len(), 2);

    for i in 1..=3u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());
    let mut chunks = Vec::new();
    while let Some(chunk) = download.next_chunk().await {
        chunks.push(chunk.unwrap());
    }
    // Still in order and complete, with no READ sent twice.
    assert_eq!(chunks, (1..=3u8).map(chunk_of).collect::<Vec<_>>());
    assert_eq!(sent_reads(&mock).len(), 3);
}

/// `MessageId`s of every READ the mock saw, in send order.
fn sent_read_ids(mock: &MockTransport) -> Vec<u64> {
    mock.sent_messages()
        .iter()
        .filter_map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).ok())
        .filter(|header| header.command == Command::Read)
        .map(|header| header.message_id.0)
        .collect()
}

/// A READ answer addressed to `msg_id`, so it can come back out of order.
fn read_answer_for(msg_id: u64, data: Vec<u8>) -> Vec<u8> {
    let mut answer = build_read_response(data);
    answer[24..32].copy_from_slice(&msg_id.to_le_bytes());
    answer
}

#[tokio::test]
async fn answers_that_land_out_of_order_are_seen_without_being_taken() {
    // What the window corrects its estimate against: READs whose answers
    // haven't come off the wire. Chunks are delivered in file order, so an
    // answer the server sent early waits behind the head, and it has to count
    // as arrived (Samba answers concurrent READs in whatever order its disk
    // gives them up). Looking must not take it: it's still the next chunks.
    let mock = Arc::new(MockTransport::new());
    let mut conn = setup_connection(&mock);
    // Keep every id nonzero, so the mock routes each answer to the id it names.
    conn.set_next_message_id(100);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(4));

    let pending = tokio::time::timeout(Duration::from_millis(50), download.next_chunk()).await;
    assert!(pending.is_err(), "nothing was answered yet");
    let ids = sent_read_ids(&mock);
    assert_eq!(ids.len(), 4);
    let everything = 4 * u64::from(CHUNK);
    let outstanding = download.outstanding();
    assert_eq!(
        (
            outstanding.bytes,
            outstanding.unanswered,
            outstanding.last_arrival
        ),
        (everything, everything, None)
    );

    // READ 3 is answered, then READ 2: neither is the head.
    mock.queue_response(read_answer_for(ids[2], chunk_of(3)));
    mock.queue_response(read_answer_for(ids[1], chunk_of(2)));
    let mut outstanding = download.outstanding();
    for _ in 0..100 {
        if outstanding.unanswered < everything - u64::from(CHUNK) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        outstanding = download.outstanding();
    }
    assert_eq!(outstanding.bytes, everything, "still undelivered");
    assert_eq!(
        outstanding.unanswered,
        2 * u64::from(CHUNK),
        "READs 1 and 4 are the only ones still on their way"
    );
    assert!(outstanding.last_arrival.is_some());
    assert_eq!(
        download.outstanding().unanswered,
        2 * u64::from(CHUNK),
        "looking again changes nothing"
    );

    // The rest arrive, and every chunk comes out in file order, intact.
    mock.queue_response(read_answer_for(ids[0], chunk_of(1)));
    mock.queue_response(read_answer_for(ids[3], chunk_of(4)));
    mock.queue_response(build_close_response());
    let mut chunks = Vec::new();
    while let Some(chunk) = download.next_chunk().await {
        chunks.push(chunk.unwrap());
    }
    assert_eq!(chunks, (1..=4u8).map(chunk_of).collect::<Vec<_>>());
    assert_eq!(sent_reads(&mock).len(), 4, "no READ sent twice");
}

// ── Closing ────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_last_chunk_returns_without_waiting_for_the_close_response() {
    let mock = Arc::new(MockTransport::new());
    // The READ is answered; the CLOSE isn't, yet.
    mock.queue_response(build_read_response(chunk_of(1)));

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 65536, CHUNK)
        .with_read_ahead(ReadAhead::SEQUENTIAL);

    // Pre-fix, `next_chunk` awaited the CLOSE before handing out the last
    // chunk, so every download's last chunk landed one round trip late.
    let last = tokio::time::timeout(Duration::from_millis(50), download.next_chunk())
        .await
        .expect("the last chunk doesn't wait for the CLOSE's answer");
    assert_eq!(last.unwrap().unwrap(), chunk_of(1));
    assert_eq!(mock.sent_count(), 2, "READ + CLOSE, both on the wire");

    // The `None` call collects the CLOSE. Dropping it (a `select!` arm
    // losing) loses nothing and sends no second CLOSE.
    let pending = tokio::time::timeout(Duration::from_millis(50), download.next_chunk()).await;
    assert!(
        pending.is_err(),
        "the CLOSE is still waiting for its answer"
    );
    mock.queue_response(build_close_response());
    assert!(download.next_chunk().await.is_none());
    assert!(download.next_chunk().await.is_none());
    assert_eq!(mock.sent_count(), 2);
}

#[tokio::test]
async fn a_close_error_surfaces_on_the_call_after_the_last_chunk() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_response(chunk_of(1)));
    mock.queue_response(build_close_error_response(NtStatus::FILE_CLOSED));

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 65536, CHUNK);

    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    assert!(matches!(
        download.next_chunk().await,
        Some(Err(Error::Protocol {
            status: NtStatus::FILE_CLOSED,
            command: Command::Close,
        }))
    ));
    assert!(download.next_chunk().await.is_none());
}

#[tokio::test]
async fn a_close_that_cant_be_sent_still_hands_out_the_last_chunk() {
    let mock = Arc::new(MockTransport::new());
    // The READ spends the only credit and its answer grants none back, so
    // there's nothing to send the CLOSE with.
    let mut starved = build_read_response(chunk_of(1));
    starved[14..16].copy_from_slice(&0u16.to_le_bytes());
    mock.queue_response(starved);

    let mut conn = setup_connection(&mock);
    conn.set_credits(1);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 65536, CHUNK);

    // The data is good, so it's handed out; the CLOSE's trouble comes next.
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    assert!(matches!(download.next_chunk().await, Some(Err(_))));
    assert!(download.next_chunk().await.is_none());
    assert_eq!(mock.sent_count(), 1, "no CLOSE went out");
}

#[tokio::test]
async fn a_download_dropped_after_its_last_chunk_has_still_sent_the_close() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_response(chunk_of(1)));
    mock.queue_response(build_read_response(chunk_of(2)));

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 2 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(2));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(2));
    // The consumer has every byte and walks away without the `None` call.
    drop(download);

    let commands: Vec<Command> = mock
        .sent_messages()
        .iter()
        .map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).unwrap().command)
        .collect();
    assert_eq!(commands, vec![Command::Read, Command::Read, Command::Close]);
    assert!(
        conn.outstanding_requests().is_empty(),
        "the CLOSE's waiter went with it"
    );
}

#[tokio::test]
async fn cancelling_collect_after_the_last_chunk_waits_for_the_close_already_out() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_response(chunk_of(1)));
    mock.queue_response(build_close_response());

    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let result = FileDownload::new(&tree, &mut conn, test_file_id(), 65536, CHUNK)
        .collect_with_progress(|_| std::ops::ControlFlow::Break(()))
        .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    // READ + one CLOSE, answered.
    assert_eq!(mock.sent_count(), 2);
    assert!(conn.outstanding_requests().is_empty());
}

// ── Adaptive window ────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn adaptive_sends_one_read_until_the_first_answer() {
    let mock = Arc::new(MockTransport::new());
    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK);
    assert_eq!(download.read_ahead(), ReadAhead::Adaptive);

    let first = tokio::time::timeout(Duration::from_millis(500), download.next_chunk()).await;
    assert!(first.is_err());
    assert_eq!(
        sent_reads(&mock).len(),
        1,
        "nothing is measured yet, and on a slow link a second READ doubles the queue"
    );
}

#[tokio::test(start_paused = true)]
async fn adaptive_opens_the_window_to_4_mib_on_a_fast_link() {
    const BIG: u32 = 512 * 1024;
    let mock = Arc::new(MockTransport::new());
    for i in 1..=16u8 {
        mock.queue_response(build_read_response(vec![i; BIG as usize]));
    }
    mock.queue_response(build_close_response());

    let mut conn = setup_connection_with_max_read(&mock, 8 << 20);
    conn.set_estimated_rtt(Some(Duration::from_millis(1)));
    let tree = test_tree();
    let mut download =
        FileDownload::new(&tree, &mut conn, test_file_id(), 16 * u64::from(BIG), BIG);

    // The paused clock makes the first READ instant: as fast as a link gets.
    assert_eq!(download.next_chunk().await.unwrap().unwrap()[0], 1);
    assert_eq!(
        sent_reads(&mock).len(),
        9,
        "one delivered, then eight 512 KiB READs: the 4 MiB cap"
    );
    let mut n = 1u8;
    while let Some(chunk) = download.next_chunk().await {
        n += 1;
        assert_eq!(chunk.unwrap(), vec![n; BIG as usize]);
    }
    assert_eq!(n, 16);
    assert_eq!(download.peak_in_flight_bytes(), 4 << 20);
}

#[tokio::test(start_paused = true)]
async fn adaptive_sends_the_next_read_shortly_before_the_head_lands_on_a_slow_link() {
    let mock = Arc::new(MockTransport::new());
    let mut conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::ZERO));
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK);

    // READ 1 takes a second: a 64 KiB/s link.
    let first = tokio::time::timeout(Duration::from_secs(1), download.next_chunk()).await;
    assert!(first.is_err());
    mock.queue_response(build_read_response(chunk_of(1)));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));

    // One READ goes straight out. The target is 250 ms at 64 KiB/s (16 KiB),
    // so the next one is due once 48 KiB of it have drained: 0.75 s later.
    assert_eq!(sent_reads(&mock).len(), 2);
    let early = tokio::time::timeout(Duration::from_millis(700), download.next_chunk()).await;
    assert!(early.is_err());
    assert_eq!(sent_reads(&mock).len(), 2, "not due yet");
    let due = tokio::time::timeout(Duration::from_millis(100), download.next_chunk()).await;
    assert!(due.is_err());
    assert_eq!(
        sent_reads(&mock).len(),
        3,
        "sent while still waiting for the head, so the link never idles"
    );

    for i in 2..=4u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());
    let mut n = 1u8;
    while let Some(chunk) = download.next_chunk().await {
        n += 1;
        assert_eq!(chunk.unwrap(), chunk_of(n));
    }
    assert_eq!(n, 4);
    assert_eq!(
        sent_reads(&mock),
        vec![(0, CHUNK), (65536, CHUNK), (131072, CHUNK), (196608, CHUNK)]
    );
}

#[tokio::test(start_paused = true)]
async fn the_next_download_on_a_connection_starts_with_the_window_the_last_one_measured() {
    let mock = Arc::new(MockTransport::new());
    for i in 1..=4u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());
    let mut conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(60)));
    let tree = test_tree();
    FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .collect()
        .await
        .unwrap();
    assert_eq!(sent_reads(&mock).len(), 4);

    // A two-chunk file next: both READs go out before either is answered,
    // instead of the second waiting a round trip for the first.
    let mut second = FileDownload::new(&tree, &mut conn, test_file_id(), 2 * 65536, CHUNK);
    let first = tokio::time::timeout(Duration::from_millis(50), second.next_chunk()).await;
    assert!(first.is_err());
    assert_eq!(sent_reads(&mock).len(), 6);
}

// ── The connection's rate hint ─────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_rate_hint_sets_the_quick_read_limit_until_it_expires() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection_with_max_read(&mock, 8 << 20);
    assert_eq!(conn.download_rate_hint(), None);
    assert_eq!(conn.quick_read_limit(), 512 * 1024, "no hint: one chunk");

    conn.note_read_rate(16e6);
    assert_eq!(conn.download_rate_hint(), Some(16_000_000));
    assert_eq!(conn.quick_read_limit(), 4_000_000, "250 ms at 16 MB/s");

    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(conn.download_rate_hint(), Some(16_000_000));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        conn.download_rate_hint(),
        None,
        "30 s without a measurement"
    );
    assert_eq!(conn.quick_read_limit(), 512 * 1024);
}

#[tokio::test(start_paused = true)]
async fn a_download_leaves_its_learned_headroom_on_the_connection_until_it_expires() {
    let mock = Arc::new(MockTransport::new());
    for i in 1..=4u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());
    let mut conn = setup_connection(&mock);
    let tree = test_tree();
    assert!(conn.read_link_hint().lateness.is_none());
    FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .collect()
        .await
        .unwrap();
    assert!(conn.read_link_hint().lateness.is_some());
    assert!(
        conn.write_link_hint().lateness.is_none(),
        "a download says nothing about the other direction"
    );

    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(
        conn.read_link_hint().lateness.is_none(),
        "the same 30 s as the rate"
    );
}

#[tokio::test]
async fn a_learned_headroom_leaves_the_quick_read_limit_alone() {
    // The one-frame cut-off is a latency budget for whatever waits behind
    // the frame, not a transfer's margin: however small the headroom learned
    // on a quiet link, 16 MB/s still makes 4 MB one READ.
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection_with_max_read(&mock, 8 << 20);
    // One READ, then 64 sent together once it's answered (the first flight
    // teaches the headroom nothing), answered back to back a millisecond
    // apart.
    let mut window = Window::new(ReadAhead::Adaptive, CHUNK, conn.read_link_hint());
    let t0 = tokio::time::Instant::now();
    window.on_dispatch(t0, CHUNK);
    let t1 = t0 + Duration::from_millis(5);
    window.on_delivery(t1, t0, t1, CHUNK, Outstanding::all_unanswered(0));
    for _ in 0..64 {
        window.on_dispatch(t1, CHUNK);
    }
    for i in 0..64u64 {
        let at = t1 + Duration::from_millis(5 + i);
        window.on_delivery(
            at,
            t1,
            at,
            CHUNK,
            Outstanding::all_unanswered((63 - i) * u64::from(CHUNK)),
        );
    }
    conn.note_read(&window);
    assert!(
        window.headroom() < Duration::from_millis(50),
        "learned {:?}",
        window.headroom()
    );
    conn.note_read_rate(16e6);
    assert_eq!(conn.quick_read_limit(), 4_000_000);
}

#[tokio::test]
async fn the_quick_read_limit_stays_within_max_read_size() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    conn.note_read_rate(1e9);
    assert_eq!(
        conn.quick_read_limit(),
        65536,
        "one READ carries at most this"
    );
}

#[tokio::test(start_paused = true)]
async fn the_rate_hint_times_chunks_by_their_arrival_not_by_when_the_consumer_takes_them() {
    let mock = Arc::new(MockTransport::new());
    let mut conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(10)));
    let tree = test_tree();
    let mut download = FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .with_read_ahead(ReadAhead::Fixed(4));

    // All four READs go out at once, and the answers land a second apart:
    // a 64 KiB/s link.
    let pending = tokio::time::timeout(Duration::from_secs(1), download.next_chunk()).await;
    assert!(pending.is_err());
    assert_eq!(sent_reads(&mock).len(), 4);
    mock.queue_response(build_read_response(chunk_of(1)));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(1));
    tokio::time::sleep(Duration::from_secs(1)).await;
    mock.queue_response(build_read_response(chunk_of(2)));
    assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(2));
    // The consumer gets busy while chunks 3 and 4 land, and takes both at
    // t = 10 s. Timed by when it took them, the link read 26 KiB/s.
    for i in 3..=4u8 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    tokio::time::sleep(Duration::from_secs(6)).await;
    mock.queue_response(build_close_response());
    for i in 3..=4u8 {
        assert_eq!(download.next_chunk().await.unwrap().unwrap(), chunk_of(i));
    }
    assert!(download.next_chunk().await.is_none());
    drop(download);

    let rate = conn.download_rate_hint().expect("four chunks leave a rate");
    assert!((64_000..67_000).contains(&rate), "{rate} B/s");
}

#[tokio::test]
async fn only_a_multi_chunk_download_leaves_a_rate_hint() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_read_response(chunk_of(1)));
    mock.queue_response(build_close_response());
    for i in 1..=4u8 {
        mock.queue_response(build_read_response(chunk_of(i)));
    }
    mock.queue_response(build_close_response());
    let mut conn = setup_connection(&mock);
    let tree = test_tree();

    FileDownload::new(&tree, &mut conn, test_file_id(), 65536, CHUNK)
        .collect()
        .await
        .unwrap();
    assert_eq!(
        conn.download_rate_hint(),
        None,
        "one READ's rate is mostly its round trip"
    );

    FileDownload::new(&tree, &mut conn, test_file_id(), 4 * 65536, CHUNK)
        .collect()
        .await
        .unwrap();
    assert!(conn.download_rate_hint().is_some());
}
