//! A handle that outlives its session refuses to talk to the next one.
//!
//! A reconnect revives a `Connection` in place, so every handle holding a clone
//! keeps a live connection through it. Its tree id and file id don't survive:
//! the new session numbers its own from scratch, and the same ids can name a
//! different file there. Each test here opens a handle, revives the connection
//! into a fresh mock server that WOULD answer the stale request (so the old
//! behavior shows up as a read or write that silently succeeds against the
//! wrong file), and checks that nothing reached that server.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::client::connection::{Connection, NegotiatedParams, SessionReviver};
use crate::client::stream::FileUpload;
use crate::client::test_helpers::{
    build_close_response, build_compound_response_frame, build_create_response,
    build_file_both_dir_info, build_flush_response, build_query_directory_response,
    build_query_info_error_response, build_read_response, build_write_response, setup_connection,
};
use crate::client::tree::Tree;
use crate::error::{Error, Result};
use crate::transport::{MockTransport, TransportReceive, TransportSend};
use crate::types::status::NtStatus;
use crate::types::{FileId, SessionId, TreeId};

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

const FILE: FileId = FileId {
    persistent: 0xAA,
    volatile: 0xBB,
};

/// What a reader's open gets back: CREATE, then class 18 and class 48, both
/// refused.
fn reader_opened(end_of_file: u64) -> Vec<u8> {
    build_compound_response_frame(&[
        build_create_response(FILE, end_of_file),
        build_query_info_error_response(NtStatus::NOT_SUPPORTED),
        build_query_info_error_response(NtStatus::NOT_SUPPORTED),
    ])
}

/// What a writer's open gets back: CREATE, then refused identity and class
/// 48 queries.
fn writer_opened() -> Vec<u8> {
    crate::client::test_helpers::build_writer_open_response(FILE, 0, None, None)
}

/// Revives into a fresh mock that answers whatever it was told to, as a new
/// session on a rebooted server would answer an id it happens to have issued.
struct FreshSession {
    params: NegotiatedParams,
    answers: Mutex<Vec<Vec<u8>>>,
    dialed: Mutex<Option<Arc<MockTransport>>>,
}

#[async_trait::async_trait]
impl SessionReviver for FreshSession {
    async fn dial(&self) -> Result<(Box<dyn TransportSend>, Box<dyn TransportReceive>)> {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        for answer in self.answers.lock().unwrap().drain(..) {
            mock.queue_response(answer);
        }
        *self.dialed.lock().unwrap() = Some(Arc::clone(&mock));
        Ok((Box::new(Arc::clone(&mock)), Box::new(mock)))
    }

    async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
        conn.set_test_params(self.params.clone());
        conn.set_session_id(SessionId(0x5678));
        conn.set_credits(512);
        Ok(())
    }
}

/// Kill `conn` and bring it back on a new session whose server answers
/// `answers`. Returns that server, to see what reached it.
async fn revive(conn: &Connection, answers: Vec<Vec<u8>>) -> Arc<MockTransport> {
    let reviver = Arc::new(FreshSession {
        params: conn.params().expect("setup_connection negotiates params"),
        answers: Mutex::new(answers),
        dialed: Mutex::new(None),
    });
    conn.set_reviver(Some(Arc::clone(&reviver) as Arc<dyn SessionReviver>));
    conn.mark_dead();
    conn.reconnect_if_needed().await.expect("the revival");
    assert_eq!(conn.generation(), 1);
    let fresh = reviver.dialed.lock().unwrap().clone();
    fresh.expect("the revival dialed")
}

#[track_caller]
fn assert_invalidated<T: std::fmt::Debug>(result: Result<T>, what: &str) {
    match result {
        Err(Error::HandleInvalidated {
            opened_on: 0,
            current: 1,
        }) => {}
        other => panic!("{what}: expected HandleInvalidated, got {other:?}"),
    }
}

/// Bounded, so a request that went out and waits for an answer that will
/// never come fails the test instead of hanging it.
async fn bounded<F: std::future::Future>(what: &str, fut: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("{what} hung: a request went out and is waiting for an answer"))
}

#[tokio::test]
async fn a_reader_from_before_a_reconnect_does_not_read_the_new_sessions_file() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(reader_opened(100));
    let conn = setup_connection(&mock);
    let reader = test_tree()
        .open_file_reader(conn.clone(), "mine.txt")
        .await
        .unwrap();

    let fresh = revive(&conn, vec![build_read_response(b"someone else's".to_vec())]).await;

    assert_invalidated(bounded("read_at", reader.read_at(0, 14)).await, "read_at");
    assert_eq!(fresh.sent_count(), 0, "no READ may reach the new session");

    bounded("close", reader.close())
        .await
        .expect("closing a handle the server already let go of has nothing to fail at");
    assert_eq!(
        fresh.sent_count(),
        0,
        "nor a CLOSE of somebody else's handle"
    );
}

#[tokio::test]
async fn a_writer_from_before_a_reconnect_writes_nothing_into_the_new_session() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(writer_opened());
    let conn = setup_connection(&mock);
    let mut writer = test_tree()
        .create_file_writer(conn.clone(), "mine.bin")
        .await
        .unwrap();

    let fresh = revive(
        &conn,
        vec![
            build_write_response(5),
            build_flush_response(),
            build_close_response(),
        ],
    )
    .await;

    assert_invalidated(
        bounded("write_chunk", writer.write_chunk(b"hello")).await,
        "write_chunk",
    );
    assert_invalidated(bounded("finish", writer.finish()).await, "finish");
    assert_eq!(
        fresh.sent_count(),
        0,
        "no WRITE, FLUSH, or CLOSE may reach it"
    );
}

#[tokio::test]
async fn aborting_a_writer_from_before_a_reconnect_sends_nothing() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(writer_opened());
    let conn = setup_connection(&mock);
    let writer = test_tree()
        .create_file_writer(conn.clone(), "mine.bin")
        .await
        .unwrap();

    let fresh = revive(&conn, vec![build_close_response()]).await;

    assert_eq!(bounded("abort", writer.abort()).await.unwrap(), 0);
    assert_eq!(fresh.sent_count(), 0, "no CLOSE of somebody else's handle");
}

#[tokio::test]
async fn a_download_from_before_a_reconnect_does_not_read_the_new_sessions_file() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_create_response(FILE, 14));
    let conn = setup_connection(&mock);
    let tree = test_tree();
    let mut caller = conn.clone();
    let mut download = tree.download(&mut caller, "mine.txt").await.unwrap();

    let fresh = revive(
        &conn,
        vec![
            build_read_response(b"someone else's".to_vec()),
            build_close_response(),
        ],
    )
    .await;

    let chunk = bounded("next_chunk", download.next_chunk())
        .await
        .expect("an error, not the end of the file");
    assert_invalidated(chunk, "next_chunk");
    assert_eq!(fresh.sent_count(), 0, "no READ may reach the new session");
}

#[tokio::test]
async fn an_upload_from_before_a_reconnect_writes_nothing_into_the_new_session() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    let tree = test_tree();
    let data = vec![0x42; 100];
    let mut upload = FileUpload::new(Cow::Borrowed(&*tree), conn.clone(), FILE, &data, 65536);

    let fresh = revive(
        &conn,
        vec![
            build_write_response(100),
            build_flush_response(),
            build_close_response(),
        ],
    )
    .await;

    assert_invalidated(
        bounded("write_next_chunk", upload.write_next_chunk()).await,
        "write_next_chunk",
    );
    assert_eq!(fresh.sent_count(), 0, "no WRITE may reach the new session");
}

#[tokio::test]
async fn a_watcher_from_before_a_reconnect_does_not_subscribe_with_a_dead_handle() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_create_response(FILE, 0));
    let mut conn = setup_connection(&mock);
    let mut watcher = test_tree().watch(&mut conn, "dir", false).await.unwrap();

    let fresh = revive(&conn, vec![]).await;

    assert_invalidated(
        bounded("next_events", watcher.next_events()).await,
        "next_events",
    );
    assert_eq!(
        fresh.sent_count(),
        0,
        "no CHANGE_NOTIFY may reach the new session"
    );

    bounded("close", watcher.close())
        .await
        .expect("closing a handle the server already let go of has nothing to fail at");
    assert_eq!(fresh.sent_count(), 0, "nor a CLOSE");
}

/// A server cursor doesn't survive its session, so a listing can't resume
/// across a reconnect: the caller opens a new reader.
#[tokio::test]
async fn a_directory_reader_from_before_a_reconnect_does_not_list_the_new_sessions_directory() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(build_create_response(FILE, 0));
    let conn = setup_connection(&mock);
    let mut reader = test_tree()
        .open_directory_reader(conn.clone(), "dir")
        .await
        .unwrap();

    let someone_elses = build_query_directory_response(
        NtStatus::SUCCESS,
        build_file_both_dir_info("someone else's.txt", 1, false, 0),
    );
    let fresh = revive(&conn, vec![someone_elses, build_close_response()]).await;

    assert_invalidated(
        bounded("next_batch", reader.next_batch()).await,
        "next_batch",
    );
    assert_eq!(
        fresh.sent_count(),
        0,
        "no QUERY_DIRECTORY, and no CLOSE of somebody else's handle"
    );
    assert!(
        bounded("next_batch", reader.next_batch())
            .await
            .unwrap()
            .is_none(),
        "the reader is finished after the error"
    );
    bounded("close", reader.close())
        .await
        .expect("closing a handle the server already let go of has nothing to fail at");
    assert_eq!(fresh.sent_count(), 0);
}

/// A second `Session::setup` on the same socket replaces the session without
/// a reconnect, so the generation never moves. The ids are just as dead.
#[tokio::test]
async fn a_new_session_on_the_same_socket_invalidates_handles_too() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(reader_opened(100));
    let conn = setup_connection(&mock);
    let reader = test_tree()
        .open_file_reader(conn.clone(), "mine.txt")
        .await
        .unwrap();
    mock.queue_response(build_read_response(b"someone else's".to_vec()));
    let sent_before = mock.sent_count();

    conn.clone().set_session_id(SessionId(0x9999));

    match bounded("read_at", reader.read_at(0, 14)).await {
        Err(Error::HandleInvalidated {
            opened_on: 0,
            current: 0,
        }) => {}
        other => panic!("expected HandleInvalidated on generation 0, got {other:?}"),
    }
    assert_eq!(mock.sent_count(), sent_before, "no READ may go out");
}

/// The other side of the guard: a handle opened on the revived session works.
#[tokio::test]
async fn a_handle_opened_after_the_reconnect_works() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);

    let fresh = revive(
        &conn,
        vec![reader_opened(5), build_read_response(b"fresh".to_vec())],
    )
    .await;
    let reader = test_tree()
        .open_file_reader(conn.clone(), "mine.txt")
        .await
        .unwrap();

    assert_eq!(
        bounded("read_at", reader.read_at(0, 5)).await.unwrap(),
        b"fresh"
    );
    assert_eq!(fresh.sent_count(), 2, "the open and the READ");
}

/// Pinning is how a handle takes its session, and handles re-pin what they're
/// given (`FileWriter::new`, the open-file continuations), so pinning a stale
/// clone must not launder it into a current one.
#[tokio::test]
async fn pinning_a_stale_clone_again_keeps_it_stale() {
    let mock = Arc::new(MockTransport::new());
    let conn = setup_connection(&mock);
    let pinned = conn.pinned_to_session();

    revive(&conn, vec![]).await;

    assert!(!pinned.pinned_session_is_current());
    assert!(
        !pinned.pinned_to_session().pinned_session_is_current(),
        "re-pinned onto the new session"
    );
    assert!(conn.pinned_to_session().pinned_session_is_current());
}

/// The tree ids every frame `mock` received carried.
fn tree_ids_sent(mock: &MockTransport) -> Vec<Option<TreeId>> {
    use crate::msg::header::Header;
    use crate::pack::{ReadCursor, Unpack};
    mock.sent_messages()
        .iter()
        .map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).unwrap().tree_id)
        .collect()
}

/// A `Tree` is a handle too: its tree id belongs to the session that
/// connected it. A new session numbers its tree connects from scratch, so the
/// old id can be a different share there, and a delete sent with it deletes
/// that share's file.
#[tokio::test]
async fn a_tree_from_before_a_reconnect_does_not_delete_in_another_share() {
    let mock = Arc::new(MockTransport::new());
    mock.queue_response(crate::client::test_helpers::build_tree_connect_response(
        TreeId(1),
        crate::msg::tree_connect::ShareType::Disk,
    ));
    let mut conn = setup_connection(&mock);
    let docs = Tree::connect(&mut conn, "Docs").await.unwrap();

    // The new session's server: tree id 1 is its first tree connect, which
    // is somebody else's share by now.
    let fresh = revive(
        &conn,
        vec![build_compound_response_frame(&[
            build_create_response(FILE, 0),
            crate::client::test_helpers::build_set_info_response(),
            build_close_response(),
        ])],
    )
    .await;

    let deleted = bounded("delete_file", docs.delete_file(&mut conn, "a.txt")).await;
    assert_eq!(
        tree_ids_sent(&fresh),
        Vec::<Option<TreeId>>::new(),
        "the dead session's tree id must not reach the new session"
    );
    assert_invalidated(deleted, "delete_file");
}

/// A DFS target's tree lives on its own connection, beside the primary. On
/// that connection it works; sent on another one, its tree id names whatever
/// that connection's session gave the id, so it's refused there even when
/// both connections are on generation 0 with the same session id.
#[tokio::test]
async fn a_tree_works_on_its_own_connection_and_is_refused_on_another() {
    let target_mock = Arc::new(MockTransport::new());
    target_mock.queue_response(crate::client::test_helpers::build_tree_connect_response(
        TreeId(7),
        crate::msg::tree_connect::ShareType::Disk,
    ));
    let mut target = setup_connection(&target_mock);
    let tree = Tree::connect(&mut target, "Target").await.unwrap();

    target_mock.queue_response(build_compound_response_frame(&[
        build_create_response(FILE, 0),
        crate::client::test_helpers::build_set_info_response(),
        build_close_response(),
    ]));
    bounded(
        "delete on its own connection",
        tree.delete_file(&mut target, "a.txt"),
    )
    .await
    .expect("a tree works on the connection it was connected on");

    let primary_mock = Arc::new(MockTransport::new());
    let mut primary = setup_connection(&primary_mock);
    assert_eq!(primary.session_id(), target.session_id());
    match bounded(
        "delete on the primary",
        tree.delete_file(&mut primary, "a.txt"),
    )
    .await
    {
        Err(Error::HandleInvalidated { .. }) => {}
        other => panic!("expected HandleInvalidated, got {other:?}"),
    }
    assert_eq!(primary_mock.sent_count(), 0, "nothing reaches the primary");
}
