//! Uploads against a mock server: the adaptive write-behind window, its
//! chunk size, the fixed-window escape hatch, and the upload rate a writer
//! leaves on its connection.
//!
//! The mock answers WRITEs in the order they were sent (FIFO pairing), and a
//! response only lands once a test queues it, so "the WRITE took a second" is
//! a test sleeping a second before queueing it. Everything runs on tokio's
//! paused clock, so that costs no wall time. The writer runs in a spawned
//! task and the test watches what reached the mock meanwhile.

use std::sync::Arc;
use std::time::Duration;

use crate::client::connection::Connection;
use crate::client::stream::{FileWriter, WriteBehind};
use crate::client::test_helpers::{
    build_close_response, build_create_response, build_flush_response, build_write_response,
    setup_connection,
};
use crate::client::tree::Tree;
use crate::msg::header::Header;
use crate::msg::write::WriteRequest;
use crate::pack::{ReadCursor, Unpack};
use crate::transport::MockTransport;
use crate::types::{Command, FileId, TreeId};

const KIB: u32 = 1024;
const MIB: u32 = 1024 * 1024;

fn test_tree() -> Arc<Tree> {
    Arc::new(Tree {
        pin: None,
        tree_id: TreeId(10),
        share_name: "test".to_string(),
        server: "test-server".to_string(),
        is_dfs: false,
        encrypt_data: false,
        dfs_origin: None,
    })
}

fn test_file_id() -> FileId {
    FileId {
        persistent: 0xAA,
        volatile: 0xBB,
    }
}

/// A connection to a server that negotiated `max_write` bytes per WRITE.
fn connection_with_max_write(mock: &Arc<MockTransport>, max_write: u32) -> Connection {
    let mut conn = setup_connection(mock);
    let mut params = conn.params().expect("setup_connection negotiates params");
    params.max_write_size = max_write;
    conn.set_test_params(params);
    conn
}

/// Offsets and lengths of every WRITE the mock saw, in send order.
fn sent_writes(mock: &MockTransport) -> Vec<(u64, u32)> {
    mock.sent_messages()
        .iter()
        .filter_map(|bytes| {
            let header = Header::unpack(&mut ReadCursor::new(bytes)).ok()?;
            if header.command != Command::Write {
                return None;
            }
            WriteRequest::unpack(&mut ReadCursor::new(&bytes[Header::SIZE..]))
                .ok()
                .map(|w| (w.offset, w.data.len() as u32))
        })
        .collect()
}

/// Answer `n` WRITEs of `len` bytes each.
fn answer_writes(mock: &MockTransport, n: usize, len: u32) {
    for _ in 0..n {
        mock.queue_response(build_write_response(len));
    }
}

/// Answer the FLUSH and CLOSE that end a finished writer.
fn answer_finish(mock: &MockTransport) {
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
}

/// Push `data` into `writer` and finish it, on a task of its own.
fn upload(mut writer: FileWriter, data: Vec<u8>) -> tokio::task::JoinHandle<crate::Result<u64>> {
    tokio::spawn(async move {
        writer.write_chunk(&data).await?;
        writer.finish().await
    })
}

// ── Chunk size ─────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_writer_sends_512_kib_writes_whatever_the_server_allows() {
    let mock = Arc::new(MockTransport::new());
    answer_writes(&mock, 2, 512 * KIB);
    answer_finish(&mock);
    let conn = connection_with_max_write(&mock, 8 * MIB);

    let writer = FileWriter::new(test_tree(), conn, test_file_id(), 8 * MIB);
    assert_eq!(writer.chunk_size(), 512 * KIB);
    let written = upload(writer, vec![7; MIB as usize])
        .await
        .unwrap()
        .unwrap();

    assert_eq!(written, u64::from(MIB));
    assert_eq!(
        sent_writes(&mock),
        vec![(0, 512 * KIB), (512 * 1024, 512 * KIB)],
        "one 8 MiB WRITE is 8 MiB queued ahead of everything else on a slow link"
    );
}

// ── Adaptive window ────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_cold_writer_sends_one_write_until_the_first_answer() {
    let mock = Arc::new(MockTransport::new());
    let conn = connection_with_max_write(&mock, 8 * MIB);
    let writer = FileWriter::new(test_tree(), conn, test_file_id(), 8 * MIB);
    assert_eq!(writer.write_behind(), WriteBehind::Adaptive);

    let task = upload(writer, vec![7; 2 * MIB as usize]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        sent_writes(&mock).len(),
        1,
        "nothing is measured yet, and on a slow link a second WRITE doubles the queue"
    );
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn a_writer_opens_its_window_to_4_mib_on_a_fast_link() {
    let mock = Arc::new(MockTransport::new());
    answer_writes(&mock, 16, 512 * KIB);
    answer_finish(&mock);
    let conn = connection_with_max_write(&mock, 8 * MIB);
    conn.set_estimated_rtt(Some(Duration::from_millis(1)));

    // The paused clock answers the first WRITE at once: as fast as a link gets.
    let mut writer = FileWriter::new(test_tree(), conn, test_file_id(), 8 * MIB);
    writer
        .write_chunk(&vec![7; 8 * MIB as usize])
        .await
        .unwrap();
    assert_eq!(writer.peak_in_flight_bytes(), 4 * u64::from(MIB));
    assert_eq!(writer.finish().await.unwrap(), 8 * u64::from(MIB));
    assert_eq!(sent_writes(&mock).len(), 16);
}

#[tokio::test(start_paused = true)]
async fn the_next_write_leaves_shortly_before_the_last_one_lands_on_a_slow_link() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::ZERO));
    let chunk = 64 * KIB;
    let writer = FileWriter::new(test_tree(), conn, test_file_id(), chunk);
    let task = upload(writer, vec![7; 4 * chunk as usize]);

    // WRITE 1 takes a second: a 64 KiB/s uplink.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(sent_writes(&mock).len(), 1);
    answer_writes(&mock, 1, chunk);

    // WRITE 2 goes straight out. The target is 250 ms at 64 KiB/s (16 KiB),
    // so WRITE 3 is due once 48 KiB of WRITE 2 have drained: 0.75 s later.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(sent_writes(&mock).len(), 2, "not due yet");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        sent_writes(&mock).len(),
        3,
        "sent while WRITE 2 is still on its way, so the uplink never idles"
    );

    answer_writes(&mock, 3, chunk);
    answer_finish(&mock);
    assert_eq!(task.await.unwrap().unwrap(), 4 * u64::from(chunk));
    assert_eq!(
        sent_writes(&mock),
        vec![(0, chunk), (65536, chunk), (131072, chunk), (196608, chunk)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_fixed_window_keeps_that_many_writes_in_flight() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    let writer = FileWriter::new(test_tree(), conn, test_file_id(), 64 * KIB)
        .with_write_behind(WriteBehind::Fixed(3));
    assert_eq!(writer.write_behind(), WriteBehind::Fixed(3));
    let task = upload(writer, vec![7; 5 * 64 * 1024]);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(sent_writes(&mock).len(), 3);
    answer_writes(&mock, 1, 64 * KIB);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(sent_writes(&mock).len(), 4, "one answered, one more out");

    answer_writes(&mock, 4, 64 * KIB);
    answer_finish(&mock);
    assert_eq!(task.await.unwrap().unwrap(), 5 * 64 * 1024);
}

#[tokio::test(start_paused = true)]
async fn the_old_default_is_one_knob_away() {
    // 0.24's writer: MaxWriteSize chunks, 32 in flight.
    let mock = Arc::new(MockTransport::new());
    let conn = connection_with_max_write(&mock, 2 * MIB);
    let writer = FileWriter::new(test_tree(), conn, test_file_id(), 2 * MIB)
        .with_chunk_size(2 * MIB)
        .with_write_behind(WriteBehind::Fixed(32));
    let task = upload(writer, vec![7; 8 * MIB as usize]);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        sent_writes(&mock),
        vec![
            (0, 2 * MIB),
            (2 << 20, 2 * MIB),
            (4 << 20, 2 * MIB),
            (6 << 20, 2 * MIB)
        ]
    );
    answer_writes(&mock, 4, 2 * MIB);
    answer_finish(&mock);
    assert_eq!(task.await.unwrap().unwrap(), 8 * u64::from(MIB));
}

// ── The connection's upload rate ───────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_writer_leaves_its_upload_rate_on_the_connection() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    assert_eq!(conn.upload_rate_hint(), None);

    // One WRITE's rate is mostly its round trip: not shared.
    answer_writes(&mock, 1, 64 * KIB);
    answer_finish(&mock);
    let one = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB);
    upload(one, vec![7; 64 * 1024]).await.unwrap().unwrap();
    assert_eq!(conn.upload_rate_hint(), None);

    answer_writes(&mock, 4, 64 * KIB);
    answer_finish(&mock);
    let four = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB);
    upload(four, vec![7; 4 * 64 * 1024]).await.unwrap().unwrap();
    assert!(conn.upload_rate_hint().is_some());
    assert!(
        conn.write_link_hint().lateness.is_some(),
        "and what it learned about lateness"
    );
    assert_eq!(
        conn.download_rate_hint(),
        None,
        "an upload says nothing about the other direction"
    );
    assert!(conn.read_link_hint().lateness.is_none());
}

#[tokio::test(start_paused = true)]
async fn the_upload_rate_times_answers_by_their_arrival_not_by_when_the_writer_looks() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(10)));
    let mut writer = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB)
        .with_write_behind(WriteBehind::Fixed(4));
    writer.write_chunk(&vec![7; 4 * 64 * 1024]).await.unwrap();
    assert_eq!(sent_writes(&mock).len(), 4);

    // The answers land a second apart (a 64 KiB/s uplink) while the
    // producer is busy elsewhere; nothing looks at them until `finish` at
    // t = 10 s. Timed by when the writer looked, the uplink read 26 KiB/s.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        answer_writes(&mock, 1, 64 * KIB);
    }
    tokio::time::sleep(Duration::from_secs(6)).await;
    answer_finish(&mock);
    assert_eq!(writer.finish().await.unwrap(), 4 * 64 * 1024);

    let rate = conn.upload_rate_hint().expect("four WRITEs leave a rate");
    assert!((64_000..67_000).contains(&rate), "{rate} B/s");
}

#[tokio::test(start_paused = true)]
async fn the_next_writer_starts_with_the_window_the_last_one_measured() {
    let mock = Arc::new(MockTransport::new());
    answer_writes(&mock, 4, 64 * KIB);
    answer_finish(&mock);
    let conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(60)));
    let first = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB);
    upload(first, vec![7; 4 * 64 * 1024])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent_writes(&mock).len(), 4);

    // A two-chunk file next: both WRITEs go out before either is answered,
    // instead of the second waiting a round trip for the first.
    let second = FileWriter::new(test_tree(), conn, test_file_id(), 64 * KIB);
    let task = upload(second, vec![7; 2 * 64 * 1024]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sent_writes(&mock).len(), 6);
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn the_upload_rate_sets_the_quick_write_limit_until_it_expires() {
    let mock = Arc::new(MockTransport::new());
    let conn = connection_with_max_write(&mock, 8 * MIB);
    assert_eq!(
        conn.quick_write_limit(),
        u64::from(512 * KIB),
        "no rate: one chunk"
    );

    conn.note_write_rate(16e6);
    assert_eq!(conn.upload_rate_hint(), Some(16_000_000));
    assert_eq!(conn.quick_write_limit(), 4_000_000, "250 ms at 16 MB/s");
    assert_eq!(conn.download_rate_hint(), None);
    assert_eq!(
        conn.quick_read_limit(),
        65536,
        "a fast uplink says nothing about reads"
    );

    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(conn.upload_rate_hint(), Some(16_000_000));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(conn.upload_rate_hint(), None, "30 s without a measurement");
    assert_eq!(conn.quick_write_limit(), u64::from(512 * KIB));
}

#[tokio::test]
async fn the_quick_write_limit_stays_within_the_compound_write_limit() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    conn.note_write_rate(1e9);
    assert_eq!(
        conn.quick_write_limit(),
        65536,
        "one WRITE carries at most MaxWriteSize"
    );

    let conn = connection_with_max_write(&mock, 8 * MIB);
    conn.note_write_rate(1e9);
    conn.set_credit_ceiling(64);
    assert_eq!(
        conn.quick_write_limit(),
        conn.compound_write_limit(),
        "what half the window funds next to the CREATE and FLUSH"
    );
    conn.set_credit_ceiling(4);
    assert_eq!(
        conn.quick_write_limit(),
        0,
        "no compound fits: stream everything"
    );
}

// ── The round trip ─────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_small_request_corrects_an_overstated_round_trip() {
    // NEGOTIATE overstates the round trip whenever the first exchange on a
    // connection is slow: a server starting a process for it, a proxy dialing
    // the far end. 175 ms against a 66 ms link on the benchmark rig, which
    // kept a 3 MB/s uplink two or three WRITEs deep. Any small request's
    // answer is a round trip too, so the smallest one seen wins.
    let mock = Arc::new(MockTransport::new());
    answer_finish(&mock);
    let conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(500)));
    let writer = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB);
    upload(writer, Vec::new()).await.unwrap().unwrap();
    let rtt = conn.estimated_rtt().expect("a round trip");
    assert!(rtt < Duration::from_millis(500), "still {rtt:?}");
}

#[tokio::test(start_paused = true)]
async fn a_write_never_counts_as_a_round_trip() {
    // A WRITE's answer also waits for its payload to cross the uplink and
    // reach the disk, so it says nothing about the round trip.
    let mock = Arc::new(MockTransport::new());
    answer_writes(&mock, 1, 64 * KIB);
    let conn = setup_connection(&mock);
    conn.set_estimated_rtt(Some(Duration::from_millis(500)));
    let mut writer = FileWriter::new(test_tree(), conn.clone(), test_file_id(), 64 * KIB)
        .with_write_behind(WriteBehind::Fixed(1));
    writer.write_chunk(&vec![7; 64 * 1024]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(sent_writes(&mock).len(), 1);
    assert_eq!(conn.estimated_rtt(), Some(Duration::from_millis(500)));
}

// ── Every pipelined write path paces the same way ──────────────────────

#[tokio::test(start_paused = true)]
async fn a_pipelined_tree_write_paces_like_a_writer() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_create_response(test_file_id(), 0));
    let mut conn = connection_with_max_write(&mock, 8 * MIB);
    let tree = test_tree();
    let data = vec![7u8; 2 * MIB as usize];
    let task = {
        let tree = Arc::clone(&tree);
        let mut conn = conn.clone();
        tokio::spawn(async move { tree.write_file_pipelined(&mut conn, "f.bin", &data).await })
    };

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        sent_writes(&mock),
        vec![(0, 512 * KIB)],
        "cold: one 512 KiB WRITE"
    );
    answer_writes(&mock, 4, 512 * KIB);
    answer_finish(&mock);
    assert_eq!(task.await.unwrap().unwrap(), 2 * u64::from(MIB));
    assert_eq!(sent_writes(&mock).len(), 4);
    assert!(conn.upload_rate_hint().is_some());
    let _ = &mut conn;
}

#[tokio::test(start_paused = true)]
async fn a_streamed_tree_write_paces_like_a_writer() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_create_response(test_file_id(), 0));
    let conn = connection_with_max_write(&mock, 8 * MIB);
    let tree = test_tree();
    let task = {
        let tree = Arc::clone(&tree);
        let mut conn = conn.clone();
        tokio::spawn(async move {
            let mut pieces = vec![vec![7u8; MIB as usize], vec![8u8; MIB as usize]].into_iter();
            let mut next = || pieces.next().map(Ok);
            tree.write_file_streamed(&mut conn, "f.bin", &mut next)
                .await
        })
    };

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        sent_writes(&mock),
        vec![(0, 512 * KIB)],
        "cold: one 512 KiB WRITE"
    );
    answer_writes(&mock, 4, 512 * KIB);
    answer_finish(&mock);
    assert_eq!(task.await.unwrap().unwrap(), 2 * u64::from(MIB));
    assert_eq!(sent_writes(&mock).len(), 4);
}

// ── Cancel safety ──────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn dropping_write_chunk_while_it_waits_for_room_loses_nothing() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    let mut writer = FileWriter::new(test_tree(), conn, test_file_id(), 64 * KIB);
    let data: Vec<u8> = (0..4 * 64 * 1024).map(|i| (i % 251) as u8).collect();

    // A consumer's `select!` gives up on the push while the cold window
    // waits for WRITE 1's answer.
    let pushed = tokio::time::timeout(Duration::from_millis(500), writer.write_chunk(&data)).await;
    assert!(pushed.is_err());
    assert_eq!(sent_writes(&mock).len(), 1);

    answer_writes(&mock, 4, 64 * KIB);
    answer_finish(&mock);
    assert_eq!(writer.finish().await.unwrap(), 4 * 64 * 1024);
    assert_eq!(
        sent_writes(&mock),
        vec![
            (0, 64 * KIB),
            (65536, 64 * KIB),
            (131072, 64 * KIB),
            (196608, 64 * KIB)
        ],
        "the rest went out with finish, in order, once each"
    );
    let payloads: Vec<u8> = mock
        .sent_messages()
        .iter()
        .filter_map(|bytes| {
            let header = Header::unpack(&mut ReadCursor::new(bytes)).ok()?;
            (header.command == Command::Write).then(|| {
                WriteRequest::unpack(&mut ReadCursor::new(&bytes[Header::SIZE..]))
                    .unwrap()
                    .data
            })
        })
        .flatten()
        .collect();
    assert_eq!(payloads, data);
}

// ── FileUpload ─────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_step_by_step_upload_paces_like_a_writer() {
    use crate::client::stream::FileUpload;

    let mock = Arc::new(MockTransport::new());
    let conn = connection_with_max_write(&mock, 8 * MIB);
    conn.set_estimated_rtt(Some(Duration::from_millis(1)));
    answer_writes(&mock, 4, 512 * KIB);
    answer_finish(&mock);
    let tree = test_tree();
    let data = vec![7u8; 2 * MIB as usize];

    let mut upload = FileUpload::new(
        std::borrow::Cow::Borrowed(&tree),
        conn.clone(),
        test_file_id(),
        &data,
        8 * MIB,
    );
    let mut steps = 0;
    while upload.write_next_chunk().await.unwrap() {
        steps += 1;
    }
    assert_eq!(
        steps, 3,
        "four 512 KiB WRITEs, the last one finishing the upload"
    );
    assert_eq!(upload.bytes_written(), 2 * u64::from(MIB));
    assert!((upload.progress().fraction() - 1.0).abs() < f64::EPSILON);
    drop(upload);
    assert_eq!(sent_writes(&mock).len(), 4);
    assert!(conn.upload_rate_hint().is_some());
}
