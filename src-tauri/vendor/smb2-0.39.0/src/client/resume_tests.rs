//! Checkpoints and verified resumes against a mock server.
//!
//! A checkpoint is the only thing an interrupted upload can safely carry on
//! from, so these pin what it promises: the offset moves only once a FLUSH
//! succeeded, a dropped checkpoint call loses nothing, and a WRITE the server
//! answered short is never counted as landed.
//!
//! The mock answers requests in the order they were sent, and a response
//! lands only once a test queues it, so "the FLUSH hasn't come back yet" is a
//! test that hasn't queued it.

use std::sync::Arc;
use std::time::Duration;

use crate::client::durable::FileIdentity;
use crate::client::stream::{FileWriter, WriteCheckpoint};
use crate::client::test_helpers::{
    build_close_response, build_compound_response_frame, build_create_error_response,
    build_flush_error_response, build_flush_response, build_query_info_error_response,
    build_set_info_response, build_write_response, build_writer_open_response, setup_connection,
};
use crate::client::tree::Tree;
use crate::error::{Error, ResumeRefusal};
use crate::msg::create::{CreateDisposition, CreateRequest};
use crate::msg::header::Header;
use crate::msg::set_info::SetInfoRequest;
use crate::msg::write::WriteRequest;
use crate::pack::{ReadCursor, Unpack};
use crate::transport::MockTransport;
use crate::types::status::NtStatus;
use crate::types::{Command, FileId, TreeId};
use crate::ErrorKind;

const KIB: u32 = 1024;

const FILE: FileId = FileId {
    persistent: 0xAA,
    volatile: 0xBB,
};

const IDENTITY: FileIdentity = FileIdentity {
    volume_serial: 0xBEEF,
    index_number: 0x77,
};

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

/// The commands of every frame sent, in order (a compound counts as its
/// first command).
fn sent_commands(mock: &MockTransport) -> Vec<Command> {
    mock.sent_messages()
        .iter()
        .map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).unwrap().command)
        .collect()
}

/// Offsets and lengths of every WRITE the mock saw, in send order.
fn sent_writes(mock: &MockTransport) -> Vec<(u64, usize)> {
    mock.sent_messages()
        .iter()
        .filter_map(|bytes| {
            let header = Header::unpack(&mut ReadCursor::new(bytes)).ok()?;
            if header.command != Command::Write {
                return None;
            }
            WriteRequest::unpack(&mut ReadCursor::new(&bytes[Header::SIZE..]))
                .ok()
                .map(|w| (w.offset, w.data.len()))
        })
        .collect()
}

// ── Checkpoints ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_checkpoint_flushes_without_closing_and_names_the_file() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_writer_open_response(FILE, 0, Some(IDENTITY), None));
    mock.queue_response(build_write_response(100));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_write_response(50));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());

    let mut writer = test_tree()
        .create_file_writer(setup_connection(&mock), "big.bin")
        .await
        .unwrap();
    assert_eq!(
        writer.identity(),
        Some(IDENTITY),
        "asked in the open round trip"
    );
    assert_eq!(
        writer.durable_offset(),
        None,
        "nothing is durable before a checkpoint"
    );

    writer.write_chunk(&[1u8; 100]).await.unwrap();
    let checkpoint = writer.flush_checkpoint().await.unwrap();
    assert_eq!(checkpoint, WriteCheckpoint::new(100, Some(IDENTITY)));
    assert_eq!(writer.durable_offset(), Some(100));

    writer.write_chunk(&[2u8; 50]).await.unwrap();
    assert_eq!(writer.finish().await.unwrap(), 150);
    assert_eq!(
        sent_commands(&mock),
        [
            Command::Create,
            Command::Write,
            Command::Flush,
            Command::Write,
            Command::Flush,
            Command::Close
        ],
        "a checkpoint is a FLUSH on the open handle, nothing more"
    );
}

#[tokio::test]
async fn the_writer_open_asks_for_the_identity_before_the_name() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_writer_open_response(FILE, 0, Some(IDENTITY), None));
    mock.queue_response(build_close_response());
    let writer = test_tree()
        .create_file_writer(setup_connection(&mock), "big.bin")
        .await
        .unwrap();
    // Class 48 last: it's the query most likely to be refused, and a server
    // that cascades a failure onto every related op after it (MS-SMB2
    // § 3.3.5.2.7.2) must not take the identity down with it.
    let sent = mock.sent_message(0).unwrap();
    let mut queries = Vec::new();
    let mut offset = 0usize;
    loop {
        let mut cursor = ReadCursor::new(&sent[offset..]);
        let header = Header::unpack(&mut cursor).unwrap();
        if header.command == Command::QueryInfo {
            let req = crate::msg::query_info::QueryInfoRequest::unpack(&mut cursor).unwrap();
            queries.push((req.info_type, req.file_info_class));
        }
        if header.next_command == 0 {
            break;
        }
        offset += header.next_command as usize;
    }
    use crate::msg::query_info::InfoType;
    assert_eq!(
        queries,
        vec![
            (InfoType::File, 6),
            (InfoType::Filesystem, 1),
            (InfoType::File, 48)
        ]
    );
    assert_eq!(
        mock.sent_count(),
        1,
        "the identity costs no extra round trip"
    );
    writer.abort().await.unwrap();
}

#[tokio::test]
async fn a_server_that_will_not_identify_the_file_still_opens_it() {
    for identity in [
        None,
        // A zero on either half is a server that answered without saying.
        Some(FileIdentity {
            volume_serial: 0,
            index_number: 0x77,
        }),
        Some(FileIdentity {
            volume_serial: 0xBEEF,
            index_number: 0,
        }),
    ] {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_writer_open_response(FILE, 0, identity, None));
        mock.queue_response(build_write_response(10));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());
        let mut writer = test_tree()
            .create_file_writer(setup_connection(&mock), "big.bin")
            .await
            .unwrap();
        assert_eq!(writer.identity(), None, "{identity:?}");
        writer.write_chunk(&[0u8; 10]).await.unwrap();
        assert_eq!(
            writer.flush_checkpoint().await.unwrap(),
            WriteCheckpoint::new(10, None),
            "the checkpoint still says how far the file is durable"
        );
        writer.abort().await.unwrap();
    }
}

#[tokio::test]
async fn the_durable_offset_moves_only_once_the_flush_succeeds() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_write_response(100));
    mock.queue_response(build_flush_error_response(NtStatus::DISK_FULL));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());

    let mut writer = FileWriter::new(test_tree(), setup_connection(&mock), FILE, 64 * KIB);
    writer.write_chunk(&[0u8; 100]).await.unwrap();
    let err = writer.flush_checkpoint().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DiskFull);
    assert_eq!(
        writer.durable_offset(),
        None,
        "the WRITE was answered, but nothing says it's on disk"
    );

    assert_eq!(writer.flush_checkpoint().await.unwrap().offset, 100);
    assert_eq!(writer.durable_offset(), Some(100));
    writer.abort().await.unwrap();
}

#[tokio::test]
async fn a_writer_at_an_offset_checkpoints_the_position_in_the_file() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_writer_open_response(FILE, 4096, Some(IDENTITY), None));
    mock.queue_response(build_write_response(50));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
    let mut writer = test_tree()
        .create_file_writer_at(setup_connection(&mock), "patched.bin", 4096)
        .await
        .unwrap();
    writer.write_chunk(&[7u8; 50]).await.unwrap();
    assert_eq!(writer.flush_checkpoint().await.unwrap().offset, 4146);
    assert_eq!(
        writer.bytes_written(),
        50,
        "bytes_written stays this writer's count"
    );
    writer.abort().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_dropped_checkpoint_loses_nothing() {
    let mock = Arc::new(MockTransport::new());
    let mut writer = FileWriter::new(test_tree(), setup_connection(&mock), FILE, 64 * KIB);
    let data: Vec<u8> = (0..4 * 64 * 1024).map(|i| (i % 251) as u8).collect();

    // Dropped while the cold window waits for WRITE 1's answer: three
    // WRITEs' worth of accepted bytes haven't gone out yet.
    let pushed = tokio::time::timeout(Duration::from_millis(500), writer.write_chunk(&data)).await;
    assert!(pushed.is_err());
    let checkpoint =
        tokio::time::timeout(Duration::from_millis(500), writer.flush_checkpoint()).await;
    assert!(checkpoint.is_err(), "nothing answered yet");
    assert_eq!(writer.durable_offset(), None);

    // Every WRITE lands, but the FLUSH doesn't come back before the caller
    // gives up again.
    for _ in 0..4 {
        mock.queue_response(build_write_response(64 * KIB));
    }
    let checkpoint =
        tokio::time::timeout(Duration::from_millis(500), writer.flush_checkpoint()).await;
    assert!(checkpoint.is_err(), "the FLUSH is still out");
    assert_eq!(
        writer.durable_offset(),
        None,
        "a FLUSH nobody heard back from proves nothing"
    );
    assert_eq!(sent_commands(&mock).last(), Some(&Command::Flush));

    // The late answer goes to the abandoned FLUSH; the next call sends its own.
    mock.queue_response(build_flush_response());
    mock.queue_response(build_flush_response());
    // And `finish` flushes once more before its CLOSE.
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
    assert_eq!(
        writer.flush_checkpoint().await.unwrap().offset,
        4 * 64 * 1024
    );
    assert_eq!(writer.finish().await.unwrap(), 4 * 64 * 1024);

    assert_eq!(
        sent_writes(&mock),
        vec![(0, 65536), (65536, 65536), (131072, 65536), (196608, 65536)],
        "every accepted byte went out once, in order"
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

#[tokio::test]
async fn a_write_the_server_answered_short_is_an_error() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_write_response(60));
    mock.queue_response(build_close_response());

    let mut writer = FileWriter::new(test_tree(), setup_connection(&mock), FILE, 64 * KIB);
    writer.write_chunk(&[0u8; 100]).await.unwrap();
    let err = writer.flush_checkpoint().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
    assert_eq!(writer.durable_offset(), None);
    assert_eq!(
        sent_commands(&mock),
        [Command::Write, Command::Close],
        "no FLUSH vouches for a file with a gap in it, and the handle is closed"
    );
    assert_eq!(
        writer.bytes_written(),
        0,
        "40 missing bytes make the 60 worthless"
    );
}

// ── Resuming ──────────────────────────────────────────────────────────

const CHECKPOINT: WriteCheckpoint = WriteCheckpoint {
    offset: 100,
    identity: Some(IDENTITY),
};

/// Resume `CHECKPOINT` against a server whose open answers with the file at
/// `end_of_file` bytes and `identity`.
async fn resume_against(
    end_of_file: u64,
    identity: Option<FileIdentity>,
) -> (Arc<MockTransport>, crate::Result<FileWriter>) {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_writer_open_response(
        FILE,
        end_of_file,
        identity,
        None,
    ));
    let resumed = test_tree()
        .resume_file_writer(setup_connection(&mock), "big.bin", &CHECKPOINT)
        .await;
    (mock, resumed)
}

/// [`resume_against`] for a resume that will be refused, with room for the
/// CLOSE the refusal sends.
async fn refused_against(
    end_of_file: u64,
    identity: Option<FileIdentity>,
) -> (Arc<MockTransport>, ResumeRefusal) {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_writer_open_response(
        FILE,
        end_of_file,
        identity,
        None,
    ));
    mock.queue_response(build_close_response());
    let resumed = test_tree()
        .resume_file_writer(setup_connection(&mock), "big.bin", &CHECKPOINT)
        .await;
    (mock, refusal(resumed))
}

fn refusal(result: crate::Result<FileWriter>) -> ResumeRefusal {
    match result {
        Ok(_) => panic!("the resume must be refused"),
        Err(Error::ResumeRefused {
            path,
            offset,
            reason,
        }) => {
            assert_eq!((path.as_str(), offset), ("big.bin", 100));
            reason
        }
        Err(e) => panic!("expected ResumeRefused, got {e:?}"),
    }
}

/// The SET_INFO a frame carries: its class and buffer.
fn set_info(bytes: &[u8]) -> (u8, Vec<u8>) {
    let req = SetInfoRequest::unpack(&mut ReadCursor::new(&bytes[Header::SIZE..])).unwrap();
    (req.file_info_class, req.buffer)
}

#[tokio::test]
async fn a_resume_reopens_the_same_file_and_writes_from_the_checkpoint() {
    let (mock, resumed) = resume_against(100, Some(IDENTITY)).await;
    let mut writer = resumed.unwrap();
    assert_eq!(mock.sent_count(), 1, "the proof rides on the CREATE");
    let create = CreateRequest::unpack(&mut ReadCursor::new(
        &mock.sent_message(0).unwrap()[Header::SIZE..],
    ))
    .unwrap();
    assert_eq!(
        create.create_disposition,
        CreateDisposition::FileOpen,
        "a resume never creates a file"
    );
    assert_eq!(
        writer.durable_offset(),
        Some(100),
        "the checkpoint was durable"
    );
    assert_eq!(writer.identity(), Some(IDENTITY));

    mock.queue_response(build_write_response(50));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
    writer.write_chunk(&[9u8; 50]).await.unwrap();
    assert_eq!(
        writer.flush_checkpoint().await.unwrap(),
        WriteCheckpoint::new(150, Some(IDENTITY))
    );
    assert_eq!(writer.finish().await.unwrap(), 50);
    assert_eq!(sent_writes(&mock), vec![(100, 50)]);
    assert_eq!(
        sent_commands(&mock),
        [
            Command::Create,
            Command::Write,
            Command::Flush,
            Command::Flush,
            Command::Close
        ],
        "a file no longer than the last write needs no trimming"
    );
}

#[tokio::test]
async fn a_resume_trims_what_the_crash_left_past_the_last_write() {
    // WRITEs that landed after the checkpoint but before the crash leave the
    // file longer than the checkpoint. The resume overwrites them, and
    // whatever is past its own last byte is trimmed at `finish`.
    let (mock, resumed) = resume_against(300, Some(IDENTITY)).await;
    let mut writer = resumed.expect("a longer file is fine");
    mock.queue_response(build_write_response(50));
    mock.queue_response(build_set_info_response());
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
    writer.write_chunk(&[9u8; 50]).await.unwrap();
    assert_eq!(writer.finish().await.unwrap(), 50);

    assert_eq!(
        sent_commands(&mock),
        [
            Command::Create,
            Command::Write,
            Command::SetInfo,
            Command::Flush,
            Command::Close
        ],
        "trimmed before the FLUSH, so the flush covers the new length too"
    );
    assert_eq!(
        set_info(&mock.sent_message(2).unwrap()),
        (20, 150u64.to_le_bytes().to_vec()),
        "FileEndOfFileInformation at the resumed writer's last byte"
    );
}

#[tokio::test]
async fn a_resume_that_writes_past_the_old_end_trims_nothing() {
    let (mock, resumed) = resume_against(120, Some(IDENTITY)).await;
    let mut writer = resumed.unwrap();
    mock.queue_response(build_write_response(50));
    mock.queue_response(build_flush_response());
    mock.queue_response(build_close_response());
    writer.write_chunk(&[9u8; 50]).await.unwrap();
    writer.finish().await.unwrap();
    assert!(!sent_commands(&mock).contains(&Command::SetInfo));
}

#[tokio::test]
async fn a_resume_finds_the_file_gone() {
    for status in [
        NtStatus::OBJECT_NAME_NOT_FOUND,
        NtStatus::OBJECT_PATH_NOT_FOUND,
    ] {
        let mock = Arc::new(MockTransport::new());
        let refused = || build_query_info_error_response(status);
        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(status),
            refused(),
            refused(),
            refused(),
        ]));
        let resumed = test_tree()
            .resume_file_writer(setup_connection(&mock), "big.bin", &CHECKPOINT)
            .await;
        assert_eq!(refusal(resumed), ResumeRefusal::Missing, "{status:?}");
        assert_eq!(mock.sent_count(), 1, "nothing opened, nothing to close");
    }
}

#[tokio::test]
async fn a_resume_finds_another_file_at_the_path() {
    for other in [
        FileIdentity {
            index_number: 0x78,
            ..IDENTITY
        },
        FileIdentity {
            volume_serial: 0xF00D,
            ..IDENTITY
        },
    ] {
        let (mock, reason) = refused_against(100, Some(other)).await;
        assert_eq!(reason, ResumeRefusal::Replaced);
        assert_eq!(
            sent_commands(&mock),
            [Command::Create, Command::Close],
            "refused before any WRITE, and the handle closed"
        );
    }
}

#[tokio::test]
async fn a_resume_finds_the_file_shorter_than_the_checkpoint() {
    let (mock, reason) = refused_against(60, Some(IDENTITY)).await;
    assert_eq!(reason, ResumeRefusal::Truncated { length: 60 });
    assert_eq!(sent_commands(&mock), [Command::Create, Command::Close]);
}

#[tokio::test]
async fn a_resume_refuses_a_file_the_server_wont_identify() {
    let (mock, reason) = refused_against(100, None).await;
    assert_eq!(reason, ResumeRefusal::IdentityUnavailable);
    assert_eq!(sent_commands(&mock), [Command::Create, Command::Close]);
}

#[tokio::test]
async fn a_checkpoint_without_an_identity_is_refused_before_anything_is_sent() {
    let mock = Arc::new(MockTransport::new());
    let resumed = test_tree()
        .resume_file_writer(
            setup_connection(&mock),
            "big.bin",
            &WriteCheckpoint::new(100, None),
        )
        .await;
    assert_eq!(refusal(resumed), ResumeRefusal::IdentityUnavailable);
    assert_eq!(mock.sent_count(), 0);
}

#[test]
fn a_refused_resume_is_its_own_kind_and_not_worth_retrying() {
    let err = Error::ResumeRefused {
        path: "big.bin".to_string(),
        offset: 100,
        reason: ResumeRefusal::Replaced,
    };
    assert_eq!(err.kind(), ErrorKind::ResumeRefused);
    assert!(
        !err.is_retryable(),
        "the same checkpoint can only be refused again"
    );
}

/// A checkpoint is persisted between runs, so with the `serde` feature it has
/// to load back as the same checkpoint, identity included.
#[cfg(feature = "serde")]
#[test]
fn a_checkpoint_round_trips_through_json() {
    let identities = [
        Some(FileIdentity {
            volume_serial: 0xBEEF,
            index_number: u64::MAX,
        }),
        None,
    ];
    for identity in identities {
        let checkpoint = WriteCheckpoint::new(1 << 40, identity);
        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let loaded: WriteCheckpoint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(loaded, checkpoint, "{json}");
    }
}
