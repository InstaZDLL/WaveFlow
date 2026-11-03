//! Fault injection: hostile-but-plausible servers.
//!
//! This class of bug survived a year of testing because nothing ever simulated
//! a server that stays connected and stops cooperating. Every failure here is
//! one that has actually been observed against real hardware — a QNAP TS-464
//! that answered nothing while TCP stayed `ESTABLISHED`, a macOS laptop
//! roaming between access points mid-copy — and none of them are reachable
//! from a mock that only replays a canned conversation.
//!
//! **Every wait in this file is bounded and panics on expiry.** A test that
//! can hang is worse than no test: it turns a red build into a wedged one, and
//! the failure this module exists to catch is a hang.
//!
//! The shared shape is [`ScriptedServer`]: a transport whose send half always
//! succeeds (the way a kernel socket buffer does after the peer has vanished)
//! and whose receive half answers according to a policy the test can change
//! mid-flight. Nothing in it ever returns an error or EOF, so a client that
//! only notices dead connections by reading one will wait forever — which is
//! precisely the bug.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Notify;

use crate::client::connection::{
    pack_message, Connection, ReconnectEvent, ReconnectPolicy, SessionReviver,
};
use crate::client::read_ahead::{Outstanding, ReadAhead, Window};
use crate::error::{Error, Result};
use crate::msg::echo::EchoResponse;
use crate::msg::header::Header;
use crate::msg::write::{WriteRequest, WriteResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::transport::{TransportReceive, TransportSend};
use crate::types::status::NtStatus;
use crate::types::{Command, MessageId, SessionId, TreeId};

// ── Timings ────────────────────────────────────────────────────────────────
//
// Scaled-down versions of the shipping defaults, keeping the same ratios so a
// test proves something about the real configuration rather than about a shape
// that only exists in tests. A probe goes out and is answered well inside the
// response deadline, exactly as the defaults do.

/// Server silence that triggers a probe. Real default: 5 s.
const KEEPALIVE: Duration = Duration::from_millis(100);
/// Silence budget for one request. Real default: 30 s.
///
/// Six probe thresholds, the shipping ratio. It has to be more than
/// `LIVENESS_WINDOW_PROBES` of them or a request would run out of budget
/// before the connection could be judged either way, which is a shape the
/// defaults deliberately do not have.
const BASE_DEADLINE: Duration = Duration::from_millis(600);
/// Outer bound on any single test. Generously above every deadline above, so
/// tripping it means something genuinely hung rather than ran slowly on a
/// loaded machine.
const TEST_BUDGET: Duration = Duration::from_secs(20);
/// How long a long poll is left registered before it is retired for a fresh
/// one. Real default: 10 minutes.
///
/// Above the response deadline, the shipping relationship: the refresh is the
/// slowest clock on a connection by a wide margin, because everything faster
/// than it is about detecting death, and a refresh detects nothing.
const LONG_POLL_REFRESH: Duration = Duration::from_millis(900);

// ── The scripted server ────────────────────────────────────────────────────

/// What the server does with each request it receives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    /// Answer everything promptly. A healthy server.
    Everything,
    /// Answer ECHO and nothing else.
    ///
    /// The whole reason the keepalive exists: the server is unmistakably
    /// alive and processing requests, while one operation is taking far
    /// longer than any fixed deadline would allow. A loaded spinning-disk NAS
    /// mid-write looks exactly like this, and killing it is the failure mode
    /// an aggressive deadline buys you if it cannot tell slow from dead.
    EchoOnly,
    /// Answer ECHO with `STATUS_NETWORK_SESSION_EXPIRED` and nothing else.
    ///
    /// A bad answer is still an answer: the server put a frame on the wire, so
    /// it is unmistakably processing requests, which is the only question the
    /// probe asks. A consumer re-running `Session::setup` on this connection
    /// must not be left with a keepalive that quietly retired.
    EchoExpired,
    /// Answer nothing at all, while the socket keeps accepting writes.
    ///
    /// Covers every cause at once — NAS reboot, share offline, disk stall,
    /// Wi-Fi roam with no RST — because from the client's side they are one
    /// state, and detection that depends on knowing which is not detection.
    Nothing,
}

/// A transport that lets a test decide, per moment, what the server does.
struct ScriptedServer {
    /// Responses ready for the client to read.
    outbox: Mutex<VecDeque<Vec<u8>>>,
    ready: Notify,
    answer: Mutex<Answer>,
    /// Requests seen, in order, and whether they have been answered.
    seen: Mutex<Vec<(Command, MessageId, bool)>>,
    /// CANCELs received, as the client addressed them: `(MessageId, AsyncId)`.
    ///
    /// The `AsyncId` is the part that decides whether a cancel does anything
    /// at all (MS-SMB2 § 3.2.4.24), so it is recorded rather than counted.
    cancels: Mutex<Vec<(MessageId, Option<u64>)>>,
    echoes: AtomicUsize,
}

impl ScriptedServer {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            outbox: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            answer: Mutex::new(answer),
            seen: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
            echoes: AtomicUsize::new(0),
        })
    }

    /// The `AsyncId` this harness hands out for a request it intends to hold,
    /// derived from the `MessageId` so a test can predict it.
    fn async_id_for(msg_id: MessageId) -> u64 {
        0xA5A5_0000_0000_0000 | msg_id.0
    }

    fn cancels(&self) -> Vec<(MessageId, Option<u64>)> {
        self.cancels.lock().unwrap().clone()
    }

    fn set_answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn echo_count(&self) -> usize {
        self.echoes.load(Ordering::Relaxed)
    }

    /// How many requests of this command the server has been sent.
    fn count_of(&self, command: Command) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _, _)| *c == command)
            .count()
    }

    /// Every command seen, in arrival order. For assertions about ORDERING,
    /// which is what "the wire is never left unarmed" comes down to.
    fn command_order(&self) -> Vec<Command> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(c, _, _)| *c)
            .collect()
    }

    /// Answer every request seen so far that has not been answered yet.
    ///
    /// The "the server was busy, not dead, and here is your data" path.
    fn answer_everything_outstanding(&self) {
        let pending: Vec<(Command, MessageId)> = {
            let mut seen = self.seen.lock().unwrap();
            seen.iter_mut()
                .filter(|(_, _, answered)| !*answered)
                .map(|entry| {
                    entry.2 = true;
                    (entry.0, entry.1)
                })
                .collect()
        };
        for (command, msg_id) in pending {
            if let Some(frame) = build_response(command, msg_id) {
                self.push(frame);
            }
        }
    }

    /// Send an interim `STATUS_PENDING` for `msg_id`: "still working on it".
    ///
    /// A sign of life that is not an answer, which is the third state the
    /// deadline has to cope with alongside "answered" and "silent".
    fn push_pending(&self, command: Command, msg_id: MessageId) {
        let mut h = Header::new_request(command);
        h.flags.set_response();
        h.message_id = msg_id;
        h.credits = 8;
        h.status = NtStatus::PENDING;
        self.push(pack_message(&h, &EchoResponse));
    }

    fn push(&self, frame: Vec<u8>) {
        self.outbox.lock().unwrap().push_back(frame);
        self.ready.notify_one();
    }

    /// The `MessageId` of the first request of this command, waiting (bounded)
    /// for it to arrive.
    async fn wait_for_request(&self, command: Command) -> MessageId {
        let deadline = Instant::now() + TEST_BUDGET;
        loop {
            if let Some((_, id, _)) = self
                .seen
                .lock()
                .unwrap()
                .iter()
                .find(|(c, _, _)| *c == command)
            {
                return *id;
            }
            assert!(
                Instant::now() < deadline,
                "the client never sent a {command:?} request"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

#[async_trait]
impl TransportSend for ScriptedServer {
    async fn send(&self, data: &[u8]) -> Result<()> {
        // Always succeeds. A socket whose peer has vanished keeps accepting
        // writes until the kernel buffer fills, which is why the send side
        // cannot be the thing that notices.
        let mut cursor = ReadCursor::new(data);
        let Ok(header) = Header::unpack(&mut cursor) else {
            return Ok(()); // not an SMB2 frame; nothing to script
        };
        let answer = *self.answer.lock().unwrap();
        let is_echo = header.command == Command::Echo;
        if is_echo {
            self.echoes.fetch_add(1, Ordering::Relaxed);
        }
        let will_answer = match answer {
            Answer::Everything => true,
            Answer::EchoOnly | Answer::EchoExpired => is_echo,
            Answer::Nothing => false,
        };
        self.seen
            .lock()
            .unwrap()
            .push((header.command, header.message_id, will_answer));
        if header.command == Command::Cancel {
            self.cancels
                .lock()
                .unwrap()
                .push((header.message_id, header.async_id));
        }
        // A server that intends to HOLD a request says so with an interim
        // STATUS_PENDING carrying an AsyncId, and every real one does this for
        // CHANGE_NOTIFY. It is the only frame that ever states the id a CANCEL
        // has to carry. A server playing dead sends nothing at all, so this is
        // conditional on the policy rather than on the command.
        if header.command == Command::ChangeNotify && answer != Answer::Nothing {
            let mut h = Header::new_request(Command::ChangeNotify);
            h.flags.set_response();
            h.flags.set_async();
            h.message_id = header.message_id;
            h.credits = 8;
            h.status = NtStatus::PENDING;
            h.async_id = Some(Self::async_id_for(header.message_id));
            self.push(pack_message(&h, &EchoResponse));
        }
        if will_answer {
            let frame = if answer == Answer::EchoExpired && is_echo {
                Some(build_expired_response(header.message_id))
            } else {
                build_response(header.command, header.message_id)
            };
            if let Some(frame) = frame {
                self.push(frame);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TransportReceive for ScriptedServer {
    async fn receive(&self) -> Result<Vec<u8>> {
        loop {
            let queued = self.outbox.lock().unwrap().pop_front();
            match queued {
                Some(frame) => return Ok(frame),
                // Never `Err`, never EOF. The receive half of a black-holed
                // link parks forever, and a client that relies on this
                // returning to notice trouble never notices.
                None => self.ready.notified().await,
            }
        }
    }
}

/// Build the server's answer to `command`, or `None` for a command this
/// harness has no canned reply for.
fn build_response(command: Command, msg_id: MessageId) -> Option<Vec<u8>> {
    let mut h = Header::new_request(command);
    h.flags.set_response();
    h.message_id = msg_id;
    // Generous, so nothing in these tests is accidentally credit-bound —
    // credit starvation has its own tests and is not what is under test here.
    h.credits = 8;
    match command {
        Command::Echo => Some(pack_message(&h, &EchoResponse)),
        Command::Write => Some(pack_message(
            &h,
            &WriteResponse {
                count: 4,
                remaining: 0,
                write_channel_info_offset: 0,
                write_channel_info_length: 0,
            },
        )),
        _ => None,
    }
}

/// An ECHO answered with `STATUS_NETWORK_SESSION_EXPIRED`.
fn build_expired_response(msg_id: MessageId) -> Vec<u8> {
    let mut h = Header::new_request(Command::Echo);
    h.flags.set_response();
    h.message_id = msg_id;
    h.credits = 8;
    h.status = NtStatus::NETWORK_SESSION_EXPIRED;
    pack_message(
        &h,
        &crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        },
    )
}

// ── Harness ────────────────────────────────────────────────────────────────

/// A connection wired to `server`, tuned to the scaled-down timings above.
fn connect(server: &Arc<ScriptedServer>) -> Connection {
    let mut conn = Connection::from_transport(
        Box::new(Arc::clone(server)),
        Box::new(Arc::clone(server)),
        "scripted-server",
    );
    conn.set_credits(512);
    // A connection past SESSION_SETUP has a session id, and some paths read it
    // as the answer to "is there a session on this wire at all" -- a CANCEL
    // refuses to go out without one.
    conn.set_session_id(SessionId(0x5E55));
    conn.set_response_timeout(Some(BASE_DEADLINE));
    conn.set_keepalive(Some(KEEPALIVE));
    conn
}

/// A WRITE body. The command matters (ECHO is what the keepalive sends, so
/// using it for the payload request would make the two indistinguishable);
/// the bytes do not.
fn a_write() -> WriteRequest {
    WriteRequest {
        data_offset: 0x70,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
        offset: 0,
        file_id: crate::types::FileId {
            persistent: 1,
            volatile: 2,
        },
        channel: 0,
        remaining_bytes: 0,
        flags: 0,
        data: vec![0xAB; 4],
    }
}

/// Issue a WRITE on its own task, so the test can observe the connection while
/// it is outstanding.
fn spawn_write(conn: &Connection) -> tokio::task::JoinHandle<Result<crate::client::Frame>> {
    let c = conn.clone();
    tokio::spawn(async move { c.execute(Command::Write, &a_write(), Some(TreeId(1))).await })
}

/// Park a long-poll CHANGE_NOTIFY on the wire, the way a `Watcher` does.
fn spawn_change_notify(conn: &Connection) -> tokio::task::JoinHandle<Result<crate::client::Frame>> {
    let c = conn.clone();
    tokio::spawn(async move {
        let req = crate::msg::change_notify::ChangeNotifyRequest {
            flags: 0,
            output_buffer_length: 4096,
            file_id: crate::types::FileId {
                persistent: 1,
                volatile: 2,
            },
            completion_filter: 0xFF,
        };
        c.execute(Command::ChangeNotify, &req, Some(TreeId(1)))
            .await
    })
}

/// Poll until `cond` holds, panicking rather than hanging if it never does.
///
/// Every timing assertion in this file is one-sided on purpose. A loaded
/// machine can only ever make things take LONGER, so "wait for X, bounded" is
/// stable where "sleep N, then assert X" is a coin flip — and a flaky test
/// about hangs is worth less than no test at all.
async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + TEST_BUDGET;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Await a task, panicking rather than hanging if it never resolves.
async fn finish<T>(task: tokio::task::JoinHandle<T>, what: &str) -> T {
    tokio::time::timeout(TEST_BUDGET, task)
        .await
        .unwrap_or_else(|_| panic!("{what} never resolved -- it hung, which is the bug"))
        .expect("task panicked")
}

// ── The tests ──────────────────────────────────────────────────────────────

/// The point of the whole exercise: a request the server has not answered, on
/// a connection the server is demonstrably still running, must not be killed
/// by the deadline that exists for dead connections.
///
/// Without the ECHO probe the client has one number to work with and no way to
/// tell these two servers apart, so it has to choose: short enough to catch
/// the dead one and it murders this one, long enough to spare this one and the
/// dead one freezes a transfer for minutes.
#[tokio::test]
async fn a_slow_write_outlives_the_deadline_while_echo_proves_the_server_alive() {
    let server = ScriptedServer::new(Answer::EchoOnly);
    let conn = connect(&server);

    let started = Instant::now();
    // Bounded, not forgiving: alive is a reason for more patience, not for
    // unlimited patience. A server that answers ECHO but never this request is
    // stalled in a way waiting cannot fix, so the ceiling still ends it.
    let outcome = finish(spawn_write(&conn), "the write").await;
    let took = started.elapsed();

    assert!(
        matches!(outcome, Err(Error::Timeout)),
        "expected the ceiling to fire with a typed timeout, got {outcome:?}"
    );
    assert_eq!(
        conn.metrics().response_deadline_extensions,
        1,
        "the write should have been granted the extension exactly once (once per \
         request, not once per tick); it took {took:?}"
    );
    assert!(
        took >= BASE_DEADLINE * 2,
        "the write was cut off after {took:?}, near the plain deadline of \
         {BASE_DEADLINE:?} — the answered ECHOes should have bought it far more room"
    );
    assert!(
        server.echo_count() >= 2,
        "the extension has to rest on probes, saw {} ECHO(es)",
        server.echo_count()
    );
    assert_eq!(conn.metrics().response_timeouts, 1);
}

/// The happy ending of the same story: the server was busy, not broken, and
/// the write completes.
///
/// Pre-keepalive this write was cut off at the base deadline and the transfer
/// failed, for no reason other than that the client could not tell.
#[tokio::test]
async fn a_write_the_server_was_merely_slow_about_completes_successfully() {
    let server = ScriptedServer::new(Answer::EchoOnly);
    let conn = connect(&server);
    // Room to spare above the base deadline, so the ceiling is nowhere near
    // and only the extension can explain the write surviving.
    conn.set_response_timeout(Some(BASE_DEADLINE * 4));

    let write = spawn_write(&conn);
    let msg_id = server.wait_for_request(Command::Write).await;

    // Wait for the write to actually outlive its plain deadline, then let the
    // data land.
    let metrics = conn.clone();
    wait_until("the write to outlive its plain deadline", || {
        metrics.metrics().response_deadline_extensions == 1
    })
    .await;
    assert!(!write.is_finished(), "cut off while the server was alive");
    server.set_answer(Answer::Everything);
    server.answer_everything_outstanding();

    let outcome = finish(write, "the write").await;
    assert!(
        outcome.is_ok(),
        "the server answered; the write must succeed, got {outcome:?}"
    );
    assert_eq!(
        conn.metrics().response_timeouts,
        0,
        "nothing was abandoned: msg_id={} came back",
        msg_id.0
    );
}

/// A server that dies mid-transfer, the way a NAS reboot or a share going
/// offline looks: it was answering, and then it is not, and the socket never
/// says a word about it.
///
/// Two facts have to line up before anything is declared: a request ran out of
/// its own deadline, AND the server put nothing at all on the wire while it
/// did, ECHO probes included. Either alone is not enough — the first on its own
/// is one stalled operation, and the second on its own is the false positive
/// that killed healthy transfers on a busy NAS.
#[tokio::test]
async fn a_server_that_dies_mid_transfer_is_declared_dead_and_every_waiter_told() {
    let server = ScriptedServer::new(Answer::Everything);
    let conn = connect(&server);

    // A healthy exchange first, so the connection has real proof of life to
    // lose. Detection has to survive the transition, not just the cold case.
    let warmup = spawn_write(&conn);
    assert!(finish(warmup, "the warm-up write").await.is_ok());

    server.set_answer(Answer::Nothing);
    let first = spawn_write(&conn);
    let second = spawn_write(&conn);

    let first = finish(first, "the first write").await;
    let second = finish(second, "the second write").await;
    for (which, outcome) in [("first", first), ("second", second)] {
        assert!(
            matches!(outcome, Err(Error::ServerUnresponsive { .. })),
            "the {which} write should name the dead session rather than blaming itself, \
             got {outcome:?}"
        );
    }
    assert!(
        conn.metrics().keepalive_failures >= 1,
        "the probes are what separate this from an ordinary stalled request"
    );
    assert!(
        conn.diagnostics().disconnected,
        "a session declared dead must be marked dead, so nothing new parks on it"
    );
}

/// The Wi-Fi-roam / laptop-sleep case: TCP goes to a black hole with no RST,
/// no FIN, and no error. Routine on macOS, and indistinguishable from a dead
/// server — which is the point, because the client should not need to
/// distinguish them.
///
/// The extra thing this pins beyond the test above: once the verdict is in,
/// **new** work fails immediately instead of parking on a corpse. A detector
/// that only rescues the requests that happened to be in flight would let the
/// next one hang all over again.
#[tokio::test]
async fn a_link_that_goes_black_without_a_reset_fails_current_and_future_requests() {
    let server = ScriptedServer::new(Answer::Everything);
    let conn = connect(&server);

    let warmup = spawn_write(&conn);
    assert!(finish(warmup, "the warm-up write").await.is_ok());

    server.set_answer(Answer::Nothing); // the access point changed underneath us
    let stranded = finish(spawn_write(&conn), "the stranded write").await;
    assert!(
        matches!(stranded, Err(Error::ServerUnresponsive { .. })),
        "expected the session to be declared dead, got {stranded:?}"
    );

    let afterwards = tokio::time::timeout(
        TEST_BUDGET,
        conn.execute(Command::Write, &a_write(), Some(TreeId(1))),
    )
    .await
    .expect("a request on a connection already known to be dead must fail at once, not park");
    assert!(
        matches!(afterwards, Err(Error::Disconnected)),
        "expected an immediate rejection, got {afterwards:?}"
    );
}

/// A connection with nothing on the wire is not probed.
///
/// There is no work to protect, so the probe would buy nothing and cost a
/// round trip; the next request brings its own deadlines with it. Consumers
/// hold idle connections open for a long time (a file manager with a mounted
/// share and nobody looking at it), and background chatter on all of them is a
/// real cost.
#[tokio::test]
async fn an_idle_connection_is_never_probed() {
    let server = ScriptedServer::new(Answer::Everything);
    let conn = connect(&server);

    tokio::time::sleep(KEEPALIVE * 8).await;

    assert_eq!(
        server.echo_count(),
        0,
        "an idle connection has nothing to keep alive"
    );
    assert_eq!(conn.metrics().keepalive_probes_sent, 0);
}

/// A server saying `STATUS_PENDING` is a server talking. The keepalive
/// measures silence, so it stays quiet, and the deadline never fires.
///
/// This is what keeps the feature free on a busy connection: a transfer at
/// full pipeline depth has frames arriving constantly, so it never probes at
/// all.
#[tokio::test]
async fn a_server_that_keeps_saying_it_is_working_is_never_probed() {
    let server = ScriptedServer::new(Answer::EchoOnly);
    let conn = connect(&server);

    let write = spawn_write(&conn);
    let msg_id = server.wait_for_request(Command::Write).await;

    // "Still working on it" eight times more often than the probe threshold,
    // for well past the base deadline. The wide margin is deliberate: a
    // loaded machine overshooting one sleep must not be able to look like a
    // quiet wire.
    for _ in 0..80 {
        tokio::time::sleep(KEEPALIVE / 8).await;
        server.push_pending(Command::Write, msg_id);
    }
    assert!(!write.is_finished(), "an acknowledged request was cut off");
    assert_eq!(
        server.echo_count(),
        0,
        "the wire was never quiet, so there was nothing to probe about"
    );
    assert_eq!(
        conn.metrics().response_deadline_extensions,
        0,
        "STATUS_PENDING restarts the deadline outright; no extension is needed"
    );

    server.set_answer(Answer::Everything);
    server.answer_everything_outstanding();
    assert!(finish(write, "the write").await.is_ok());
}

/// A probe that cannot be sent is evidence of nothing, and must never be
/// counted against the server.
///
/// The trap this guards: the credit window is fully spent exactly when the
/// pipeline is deepest, which is exactly when a server goes quiet under load.
/// Treating "I could not ask" as "it did not answer" would put the busiest
/// healthy transfers first in line for every harsh conclusion the client
/// draws — the starvation hang from the client side, wearing a different hat.
///
/// What still ends the write here is its own deadline, at its own length. The
/// connection is torn down afterwards, because a server owing both a response
/// and a credit grant and producing neither is what a wedge looks like, but
/// nothing gets cut short to reach that: the skipped probes moved nothing.
#[tokio::test]
async fn a_probe_that_cannot_get_a_credit_is_skipped_rather_than_called_a_death() {
    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    // Exactly enough for the write and nothing left over, and no response will
    // ever bring a credit back.
    conn.set_credits(1);
    conn.set_response_timeout(Some(BASE_DEADLINE * 8));

    let started = Instant::now();
    let write = spawn_write(&conn);
    let metrics = conn.clone();
    wait_until("a probe round to be skipped for want of a credit", || {
        metrics.metrics().keepalive_probes_skipped >= 1
    })
    .await;
    assert_eq!(
        conn.metrics().keepalive_failures,
        0,
        "a probe that was never sent cannot have gone unanswered"
    );

    let outcome = finish(write, "the write").await;
    assert!(
        started.elapsed() >= BASE_DEADLINE * 8,
        "the write was cut short of its own deadline by probes that never went out"
    );
    assert!(
        matches!(outcome, Err(Error::ServerUnresponsive { .. })),
        "a server that answered neither the request nor a credit grant is a dead link, \
         got {outcome:?}"
    );
    assert_eq!(
        server.echo_count(),
        0,
        "there was no credit to send an ECHO with"
    );
    assert_eq!(
        conn.metrics().keepalive_failures,
        0,
        "a probe that was never sent cannot be a probe that went unanswered"
    );
}

/// The extension is granted on evidence, never on assumption.
///
/// With the keepalive off nothing refreshes the liveness clock, so a recent
/// frame is luck rather than proof — and a deadline extended on luck is the
/// hang coming back through the front door.
#[tokio::test]
async fn a_request_with_no_liveness_evidence_gets_the_plain_deadline() {
    let server = ScriptedServer::new(Answer::Everything);
    let conn = connect(&server);
    conn.set_keepalive(None);

    // A healthy exchange, so the liveness clock is as fresh as it ever gets.
    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());

    server.set_answer(Answer::Nothing);
    let outcome = finish(spawn_write(&conn), "the write").await;

    assert!(
        matches!(outcome, Err(Error::Timeout)),
        "expected the plain deadline, got {outcome:?}"
    );
    assert_eq!(
        conn.metrics().response_deadline_extensions,
        0,
        "the deadline was extended with no probe to justify it"
    );
    assert_eq!(
        server.echo_count(),
        0,
        "the keepalive was turned off and must stay off"
    );
}

/// The one wait the response deadline cannot protect: a long-poll
/// CHANGE_NOTIFY, which is exempt by design because it waits for an event that
/// may never come.
///
/// A `Watcher` holds one open on the wire at all times, so before the
/// keepalive a dead server left it waiting for an event that could never
/// arrive — for hours, silently, with the connection looking idle rather than
/// broken. What ends it is the connection going silent rather than the request
/// running long: hours of silence on an open watch is the healthy case, and
/// only the probes make the difference visible.
#[tokio::test]
async fn a_long_poll_waiting_on_a_dead_server_is_told_instead_of_waiting_forever() {
    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    // Room for several probe rounds inside the budget, so the verdict rests on
    // probes rather than on the first one that happened to be late.
    conn.set_response_timeout(Some(BASE_DEADLINE * 4));

    let watching = spawn_change_notify(&conn);

    let outcome = finish(watching, "the CHANGE_NOTIFY").await;
    assert!(
        matches!(outcome, Err(Error::ServerUnresponsive { .. })),
        "a watcher on a dead session has to be told, got {outcome:?}"
    );
    assert!(
        server.echo_count() >= 2,
        "the verdict has to rest on probes, saw {}",
        server.echo_count()
    );
}

/// The same watch, with probing off, waits forever — deliberately.
///
/// "The server has said nothing" is a claim only the ECHO probes can support.
/// With nothing asking, a silent connection is indistinguishable from a
/// watched directory nobody has touched, and ending the wait would be a guess
/// dressed as a diagnosis.
#[tokio::test]
async fn a_long_poll_with_the_keepalive_off_is_never_given_up_on() {
    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    conn.set_response_timeout(Some(BASE_DEADLINE));
    conn.set_keepalive(None);

    let watching = spawn_change_notify(&conn);

    tokio::time::sleep(BASE_DEADLINE * 6).await;
    assert!(
        !watching.is_finished(),
        "a long poll was given up on with nothing but silence to go on"
    );
    assert_eq!(server.echo_count(), 0, "the keepalive was turned off");
    assert!(!conn.diagnostics().disconnected);
    watching.abort();
}

// ── When the stall is on our side of the wire ──────────────────────────────

/// Freeze the whole process, the way a starved or napped one is frozen.
///
/// `#[tokio::test]` runs on a current-thread runtime, so blocking it stops
/// every task and every timer while `Instant` keeps advancing. That IS the
/// fault being simulated: a process nobody scheduled cannot hear a server
/// that is answering perfectly, and afterwards its clocks read as if the
/// server had been silent the whole time.
fn stall_the_process(how_long: Duration) {
    std::thread::sleep(how_long);
}

/// A process that stopped running is not a server that stopped answering.
///
/// Every liveness clock in this file measures wall time, so a stretch the
/// process spent unscheduled reads exactly like a server gone quiet — and the
/// verdict it produces blames a machine that was answering the whole time.
/// Cmdr hit this on 2026-08-08 with three freezes of 62 s, 175 s, and 355 s
/// (a concurrent `cargo build` starving the dev app), each one ending in a
/// declared-dead session against a perfectly healthy NAS. A closed laptop lid
/// is the same shape, so this is a user-facing case rather than a dev-box one.
#[tokio::test]
async fn a_stall_on_our_side_is_not_blamed_on_a_server_that_kept_answering() {
    let server = ScriptedServer::new(Answer::Everything);
    let conn = connect(&server);
    let watching = spawn_change_notify(&conn);
    wait_until("the CHANGE_NOTIFY to reach the wire", || {
        server.count_of(Command::ChangeNotify) >= 1
    })
    .await;

    // Well past the response deadline, so the only thing standing between a
    // healthy connection and a death verdict is knowing who went quiet.
    stall_the_process(BASE_DEADLINE * 3);
    tokio::time::sleep(KEEPALIVE * 4).await;

    assert!(
        !watching.is_finished(),
        "a server that answered everything was declared dead because WE stopped running"
    );
    assert!(!conn.diagnostics().disconnected);
    assert!(
        conn.metrics().scheduling_stalls >= 1,
        "the stall has to be recognized as ours, not waited out by luck"
    );
    watching.abort();
}

/// Forgiving a stall must not blind the connection to the death after it.
///
/// The guard says "we were not listening", which is only ever a reason to ask
/// again — the ECHO probes restart and reach the same verdict a moment later.
/// A guard that instead suppressed the verdict outright would trade a false
/// death for a permanent hang, which is the worse of the two by far.
#[tokio::test]
async fn a_server_that_really_is_dead_is_still_told_of_after_a_stall() {
    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    conn.set_response_timeout(Some(BASE_DEADLINE * 4));
    let watching = spawn_change_notify(&conn);
    wait_until("the CHANGE_NOTIFY to reach the wire", || {
        server.count_of(Command::ChangeNotify) >= 1
    })
    .await;

    stall_the_process(BASE_DEADLINE * 3);

    let outcome = finish(watching, "the CHANGE_NOTIFY").await;
    assert!(
        matches!(outcome, Err(Error::ServerUnresponsive { .. })),
        "a dead server still has to be declared dead once we are running again, got {outcome:?}"
    );
}

/// The same verdict, reached through the type a consumer actually holds.
///
/// The two tests above drive CHANGE_NOTIFY through `Connection::execute`,
/// which is not how anything watches a directory: `Watcher` dispatches its own
/// request and keeps the next one pre-issued, so it holds a `WaiterGuard`
/// rather than calling `execute`. Awaiting that guard directly walked straight
/// past the long-poll bound, and a `Watcher` on a silent-but-open session
/// waited forever — measured against a real Samba with its `smbd` suspended:
/// 90.3 s of connection-wide silence and 17 consecutive unanswered ECHO probes
/// left the watcher none the wiser (2026-08-02, Raspberry Pi 4, Samba 4.9.5).
/// ❌ Don't "simplify" `next_events` back to a bare `recv()`.
#[tokio::test]
async fn a_real_watcher_on_a_dead_server_is_told_instead_of_waiting_forever() {
    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    conn.set_response_timeout(Some(BASE_DEADLINE * 4));

    let mut watcher = crate::client::watcher::Watcher::new(
        crate::client::tree::Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "test".to_string(),
            server: "scripted-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        },
        conn.clone(),
        crate::types::FileId {
            persistent: 1,
            volatile: 2,
        },
        crate::client::watcher::WatchOptions::new().recursive(true),
    );
    let watching = tokio::spawn(async move { watcher.next_events().await });

    let outcome = finish(watching, "the Watcher").await;
    assert!(
        matches!(outcome, Err(Error::ServerUnresponsive { .. })),
        "a Watcher on a dead session has to be told, got {outcome:?}"
    );
    assert!(
        server.echo_count() >= 2,
        "the verdict has to rest on probes, saw {}",
        server.echo_count()
    );
}

/// The gap the connection-level bound cannot cover: the server is answering
/// everything except the one subscription it has quietly forgotten.
///
/// Every liveness verdict this crate reaches is about the CONNECTION —
/// `quiet_for`, the ECHO probes, `unresponsive_for`, and the long-poll bound
/// built on them. On a link where responses keep flowing they all read
/// "healthy", correctly, and a CHANGE_NOTIFY the server dropped on the floor
/// waits behind them forever. A QNAP TS-464 did exactly this for 6,186 s while
/// answering `fs_info` in 4 ms on the same connection (2026-08-03).
///
/// ❌ There is nothing here to detect: a forgotten subscription and an
/// untouched directory both look like silence. What ends it is refusing to
/// trust one subscription indefinitely — retire it on a cycle and issue a
/// fresh one, which heals the loss without ever having to be right about it.
#[tokio::test]
async fn a_subscription_the_server_forgot_is_re_issued_rather_than_trusted_forever() {
    let server = ScriptedServer::new(Answer::EchoOnly);
    let conn = connect(&server);
    conn.set_long_poll_refresh(Some(LONG_POLL_REFRESH));

    let mut watcher = crate::client::watcher::Watcher::new(
        crate::client::tree::Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "test".to_string(),
            server: "scripted-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        },
        conn.clone(),
        crate::types::FileId {
            persistent: 1,
            volatile: 2,
        },
        crate::client::watcher::WatchOptions::new().recursive(true),
    );
    let watching = tokio::spawn(async move { watcher.next_events().await });

    // A watcher opens with two CHANGE_NOTIFY requests: the one it awaits and
    // the successor it pre-issues so the server always has somewhere to put an
    // event. A third means the refresh cycle ran.
    let watched = Arc::clone(&server);
    wait_until("the watcher to re-issue its subscription", || {
        watched.count_of(Command::ChangeNotify) >= 3
    })
    .await;

    let cancels = server.cancels();
    assert!(
        cancels.len() >= 2,
        "a retired subscription has to be CANCELled, or the server accumulates one \
         abandoned CHANGE_NOTIFY per cycle for the life of the watch; saw {cancels:?}"
    );
    // The part that decides whether a cancel does anything at all. A server
    // that has sent an interim STATUS_PENDING has assigned the request an
    // AsyncId, and a cancel that does not carry it matches nothing (MS-SMB2
    // § 3.2.4.24) — so a client that forgot the id would think it was retiring
    // subscriptions while the server quietly kept every one of them.
    for (msg_id, async_id) in &cancels {
        assert_eq!(
            *async_id,
            Some(ScriptedServer::async_id_for(*msg_id)),
            "the CANCEL for msg_id={} has to carry the AsyncId the server assigned it, \
             or the server never lets the subscription go",
            msg_id.0
        );
    }
    // The whole point of the pipelined watcher is that the server is never
    // without an outstanding request, so the replacement must go out BEFORE
    // the retirements. A strict server drops events that land in that gap.
    let order = server.command_order();
    let third_notify = order
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == Command::ChangeNotify)
        .nth(2)
        .map(|(i, _)| i)
        .expect("a third CHANGE_NOTIFY was just waited for");
    let first_cancel = order
        .iter()
        .position(|c| *c == Command::Cancel)
        .expect("a CANCEL was just asserted");
    assert!(
        third_notify < first_cancel,
        "the replacement must reach the wire before the stale requests are retired, \
         or the refresh opens the very loss window the pipelined watcher exists to \
         close; saw {order:?}"
    );
    assert!(
        conn.metrics().long_poll_refreshes >= 1,
        "a consumer has to be able to see cycles happening"
    );
    assert!(
        !watching.is_finished(),
        "a refresh is a handover, not a failure: the watch continues"
    );
    watching.abort();
}

/// The same watch with the refresh turned off keeps one subscription forever.
///
/// Deliberate, and the reason the knob exists: a consumer that re-creates its
/// own watchers on a schedule has no use for a second cycle underneath it, and
/// the crate should not force wire chatter on it. ❌ The cost is that a
/// subscription the server drops stays dropped, so this is opt-in silence.
#[tokio::test]
async fn a_long_poll_with_the_refresh_off_keeps_one_subscription_forever() {
    let server = ScriptedServer::new(Answer::EchoOnly);
    let conn = connect(&server);
    conn.set_long_poll_refresh(None);

    let mut watcher = crate::client::watcher::Watcher::new(
        crate::client::tree::Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "test".to_string(),
            server: "scripted-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        },
        conn.clone(),
        crate::types::FileId {
            persistent: 1,
            volatile: 2,
        },
        crate::client::watcher::WatchOptions::new().recursive(true),
    );
    let watching = tokio::spawn(async move { watcher.next_events().await });

    let watched = Arc::clone(&server);
    wait_until("the watcher to arm itself", || {
        watched.count_of(Command::ChangeNotify) >= 2
    })
    .await;
    tokio::time::sleep(LONG_POLL_REFRESH * 4).await;

    assert_eq!(
        server.count_of(Command::ChangeNotify),
        2,
        "with the refresh off nothing should re-issue"
    );
    assert_eq!(conn.metrics().long_poll_refreshes, 0);
    assert!(!watching.is_finished());
    watching.abort();
}

/// A probe answered with an error is still a probe answered.
///
/// `STATUS_NETWORK_SESSION_EXPIRED` is the realistic case: the session needs
/// re-establishing, but the server unmistakably put a frame on the wire, which
/// is the only question the probe asks. Reading it as death would retire the
/// keepalive for the life of a connection a consumer is about to re-authenticate
/// on — and silently, since nothing announces a keepalive that stopped.
#[tokio::test]
async fn a_probe_the_server_answers_with_an_error_still_counts_as_alive() {
    let server = ScriptedServer::new(Answer::EchoExpired);
    let conn = connect(&server);
    conn.set_response_timeout(None); // the deadline is not what is under test

    let write = spawn_write(&conn);
    server.wait_for_request(Command::Write).await;

    // Well past the point where two unanswered probes would have declared the
    // session dead.
    let metrics = conn.clone();
    wait_until("several probe rounds to go by", || {
        metrics.metrics().keepalive_probes_sent >= 4
    })
    .await;

    assert_eq!(
        conn.metrics().keepalive_failures,
        0,
        "an answered probe is an answered probe, whatever status it carried"
    );
    assert!(
        !conn.diagnostics().disconnected,
        "the session was torn down over a server that was demonstrably talking"
    );
    assert!(
        !write.is_finished(),
        "the write should still be outstanding"
    );
    write.abort();
}

// ── A NAS you can bounce ───────────────────────────────────────────────────
//
// The `ScriptedServer` above can go silent, which is what M2 needed. Surviving
// the blip needs one more thing: a server that goes away and then *comes
// back*, on a different socket, with none of the old session's state. That is
// a NAS reboot, a share remounting, and a laptop finding the network again
// after a roam — three causes, one shape.

/// Hands out a fresh [`ScriptedServer`] per dial, and can be told to fail or
/// hang instead.
///
/// Doubles as the [`SessionReviver`] the connection is armed with. Its
/// `reauthenticate` stages credits and a session id rather than running real
/// NTLM: the negotiate and SESSION_SETUP flows already have their own
/// coverage, and mixing them in here would test the wrong thing. What is under
/// test is the revival machinery — the transport swap, the state reset, and
/// the bounds.
struct BouncingNas {
    /// Every generation handed out, oldest first.
    generations: Mutex<Vec<Arc<ScriptedServer>>>,
    /// What the next generation answers when it comes up.
    next_answer: Mutex<Answer>,
    /// Dials that must fail before one is allowed to succeed.
    dials_that_fail: AtomicUsize,
    /// When set, `dial` parks forever. Only the reconnect budget can end it,
    /// which is the point.
    dial_hangs: AtomicBool,
    /// When set, the socket comes up but authentication is refused.
    auth_fails: AtomicBool,
    dials: AtomicUsize,
    /// Never notified. The park a hanging dial waits on.
    never: Notify,
}

impl BouncingNas {
    /// Power on with one generation already running.
    fn new(answer: Answer) -> Arc<Self> {
        let nas = Arc::new(Self {
            generations: Mutex::new(Vec::new()),
            next_answer: Mutex::new(answer),
            dials_that_fail: AtomicUsize::new(0),
            dial_hangs: AtomicBool::new(false),
            auth_fails: AtomicBool::new(false),
            dials: AtomicUsize::new(0),
            never: Notify::new(),
        });
        nas.spin_up();
        nas
    }

    fn spin_up(&self) -> Arc<ScriptedServer> {
        let server = ScriptedServer::new(*self.next_answer.lock().unwrap());
        self.generations.lock().unwrap().push(Arc::clone(&server));
        server
    }

    /// The generation currently serving.
    fn current(&self) -> Arc<ScriptedServer> {
        self.generations.lock().unwrap().last().unwrap().clone()
    }

    fn generation(&self, n: usize) -> Arc<ScriptedServer> {
        self.generations.lock().unwrap()[n].clone()
    }

    fn generations_handed_out(&self) -> usize {
        self.generations.lock().unwrap().len()
    }

    fn dial_count(&self) -> usize {
        self.dials.load(Ordering::Relaxed)
    }

    /// The server goes away without a word: the socket stays up and nothing is
    /// ever answered again. Indistinguishable, from the client's side, from a
    /// reboot, a share going offline, or an access point handover.
    fn goes_away(&self) {
        self.current().set_answer(Answer::Nothing);
    }
}

#[async_trait]
impl SessionReviver for BouncingNas {
    // Rust 1.99 renamed `fetch_update` to `try_update`, which the 1.87 MSRV lacks.
    #[allow(deprecated)]
    async fn dial(&self) -> Result<(Box<dyn TransportSend>, Box<dyn TransportReceive>)> {
        self.dials.fetch_add(1, Ordering::Relaxed);
        if self.dial_hangs.load(Ordering::Relaxed) {
            self.never.notified().await;
            unreachable!("the budget must end this, not the notify");
        }
        if self
            .dials_that_fail
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(Error::Disconnected);
        }
        let server = self.spin_up();
        Ok((Box::new(Arc::clone(&server)), Box::new(Arc::clone(&server))))
    }

    async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
        if self.auth_fails.load(Ordering::Relaxed) {
            return Err(Error::Auth {
                message: "the password changed while we were away".into(),
            });
        }
        conn.set_credits(512);
        conn.set_session_id(SessionId(0xBEEF));
        Ok(())
    }
}

/// Bounds scaled to the harness's timings, keeping the shipping shape: several
/// attempts, growing backoff, one wall-clock ceiling over the lot.
fn test_policy() -> ReconnectPolicy {
    ReconnectPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(80),
        total_budget: Duration::from_secs(2),
        failure_cooldown: Duration::from_millis(300),
    }
}

/// A connection to `nas`'s current generation, armed to reconnect.
///
/// The response deadline is pushed far out on purpose. These tests are about
/// what happens once a session is *declared dead*, and only the keepalive
/// declares that — a plain response timeout abandons one request and leaves
/// the connection standing.
fn connect_to(nas: &Arc<BouncingNas>) -> Connection {
    let conn = connect(&nas.current());
    conn.set_reviver(Some(Arc::clone(nas) as Arc<dyn SessionReviver>));
    conn.set_reconnect_policy(test_policy());
    conn
}

/// Collects every [`ReconnectEvent`] the connection announces.
fn watch_events(conn: &Connection) -> Arc<Mutex<Vec<ReconnectEvent>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    conn.on_reconnect(Some(Arc::new(move |e| sink.lock().unwrap().push(e))));
    seen
}

// ── Surviving the blip ─────────────────────────────────────────────────────

/// The headline: a server that vanishes mid-transfer and comes back does not
/// end the transfer.
///
/// Before this, the best available outcome was a clean, typed error — better
/// than the hang it replaced, and still a failed copy. The write here fails
/// once, the connection comes back on a fresh socket, and the retry lands on
/// the new server.
#[tokio::test]
async fn a_write_that_died_with_the_server_succeeds_once_the_connection_is_revived() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    let events = watch_events(&conn);

    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());

    nas.goes_away();
    let stranded = finish(spawn_write(&conn), "the stranded write").await;
    assert!(
        matches!(stranded, Err(Error::ServerUnresponsive { .. })),
        "the dead session should be named first, got {stranded:?}"
    );

    conn.reconnect_if_needed()
        .await
        .expect("the NAS is answering again; the revival must succeed");

    let retried = finish(spawn_write(&conn), "the retried write").await;
    assert!(
        retried.is_ok(),
        "a revived connection has to carry real work, got {retried:?}"
    );
    assert_eq!(
        nas.generations_handed_out(),
        2,
        "exactly one fresh socket was dialed"
    );
    assert_eq!(
        conn.metrics().reconnects_succeeded,
        1,
        "the counter is what makes a survived blip visible after the fact"
    );
    assert_eq!(conn.generation(), 1);
    assert!(!conn.is_disconnected());

    let events = events.lock().unwrap().clone();
    assert!(
        matches!(
            events.first(),
            Some(ReconnectEvent::Started { attempt: 1, .. })
        ),
        "got {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(ReconnectEvent::Succeeded { attempts: 1, .. })
        ),
        "a consumer that wants to say 'reconnected, resuming' needs the push, got {events:?}"
    );
}

/// A frame built for the session that died must never land on the new socket.
///
/// This is a data-corruption boundary, not a tidiness one: the new server has
/// its own message-id sequence, its own credit window, and its own signing
/// keys, so a stale frame arriving there is at best rejected and at worst
/// desynchronizes the stream for everything behind it.
#[tokio::test]
async fn a_frame_built_for_the_dead_session_cannot_reach_the_new_socket() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);

    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());
    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    let sent_to_the_corpse = nas.generation(0).seen.lock().unwrap().len();
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the revival hung")
        .expect("revival");
    assert!(finish(spawn_write(&conn), "the retried write")
        .await
        .is_ok());

    assert_eq!(
        nas.generation(0).seen.lock().unwrap().len(),
        sent_to_the_corpse,
        "the dead generation received a frame after the connection moved on"
    );
    assert!(
        !nas.generation(1).seen.lock().unwrap().is_empty(),
        "the new generation should have received the retry"
    );
}

/// Every scrap of the dead session is gone, not carried over.
///
/// Each of these has its own failure mode if it survives a revival: a stale
/// credit window over-spends the new server's budget (the original 2026-07-31
/// wedge), a stale message id makes the server drop the connection for a
/// sequence gap, stale signing keys fail verification on every frame, stale
/// negotiated sizes chunk writes against a server that no longer exists, and a
/// stale download rate tells a consumer that one big READ is quick on a link
/// that may have changed under the revival (Wi-Fi roaming, a VPN coming up).
#[tokio::test]
async fn a_revival_leaves_no_state_belonging_to_the_dead_session() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);

    // Stage state that must not survive: signing keys, DFS trees, a message-id
    // sequence well past zero, and a wide-open credit window. The keys go in
    // after the warm-up write, whose answer this fake NAS doesn't sign.
    let mut staged = conn.clone();
    staged.register_dfs_tree(TreeId(7));
    conn.note_read_rate(50e6);
    let mut window = Window::new(ReadAhead::Adaptive, 65536, conn.read_link_hint());
    let t0 = tokio::time::Instant::now();
    window.on_dispatch(t0, 65536);
    window.on_delivery(t0, t0, t0, 65536, Outstanding::all_unanswered(0));
    conn.note_read(&window);
    assert!(conn.read_link_hint().lateness.is_some());
    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());
    assert!(conn.next_message_id() > 0);
    staged.activate_signing(
        vec![0xAB; 16],
        crate::crypto::signing::SigningAlgorithm::AesCmac,
    );

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the revival hung")
        .expect("revival");

    assert_eq!(
        conn.next_message_id(),
        0,
        "message ids restart with the session; a gap makes the server drop us"
    );
    assert_eq!(
        conn.session_id(),
        SessionId(0xBEEF),
        "the session id must be the new session's, not the dead one's"
    );
    assert!(
        conn.params().is_none(),
        "negotiated sizes belong to the server we were talking to, not this one"
    );
    assert_eq!(
        conn.read_rate_hint(),
        None,
        "the dead link's download rate describes a socket that's gone"
    );
    assert!(
        conn.read_link_hint().lateness.is_none(),
        "and so does how late its answers came"
    );
    let d = conn.diagnostics();
    assert!(
        !d.signing.active,
        "signing keys derived from a dead session verify nothing"
    );
    assert!(!d.disconnected);
}

/// A revived connection is fully plumbed, not just re-socketed.
///
/// The trap: the keepalive and the stale-request sweeper both exit for good
/// when the connection is marked dead. A revival that only swapped the
/// transport would come back with no liveness detection at all — and it would
/// come back *silently*, so the next wedge would look exactly like the one
/// this whole effort started with.
#[tokio::test]
async fn a_revived_connection_can_still_detect_the_next_death() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);

    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());
    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the revival hung")
        .expect("revival");
    assert!(finish(spawn_write(&conn), "the retried write")
        .await
        .is_ok());

    let probes_before = conn.metrics().keepalive_probes_sent;
    nas.goes_away(); // the second generation dies too
    let stranded_again = finish(spawn_write(&conn), "the second stranded write").await;

    assert!(
        matches!(stranded_again, Err(Error::ServerUnresponsive { .. })),
        "the revived connection must still be able to notice a dead server, got \
         {stranded_again:?}"
    );
    assert!(
        conn.metrics().keepalive_probes_sent > probes_before,
        "the keepalive did not come back with the connection"
    );
}

// ── The bounds ─────────────────────────────────────────────────────────────

/// A reconnect that cannot work gives up, inside its budget, with a typed
/// error.
///
/// ❌ The one thing this must never do is keep trying. An unbounded retry loop
/// is the infinite hang this entire effort exists to delete, wearing a
/// different costume.
#[tokio::test]
async fn a_reconnect_that_cannot_work_gives_up_rather_than_retrying_forever() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    let events = watch_events(&conn);
    nas.dials_that_fail.store(usize::MAX, Ordering::Relaxed);

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    let started = Instant::now();
    let outcome = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the revival hung, which is the bug this whole effort is about");
    let took = started.elapsed();

    assert!(
        matches!(
            outcome,
            Err(Error::ReconnectFailed {
                attempts: 3,
                cause: crate::ErrorKind::ConnectionLost,
                ..
            })
        ),
        "expected a typed give-up naming the attempt count, got {outcome:?}"
    );
    assert!(
        took < test_policy().total_budget,
        "the attempts should have run out before the budget did; took {took:?}"
    );
    assert_eq!(nas.dial_count(), 3, "exactly max_attempts dials");
    assert_eq!(conn.metrics().reconnects_failed, 1);
    assert!(
        conn.is_disconnected(),
        "a connection whose revival failed must be unambiguously dead, never \
         half-alive: a caller that thinks it is usable will send unauthenticated \
         frames at it"
    );
    let events = events.lock().unwrap().clone();
    assert!(
        matches!(
            events.last(),
            Some(ReconnectEvent::Failed { attempts: 3, .. })
        ),
        "got {events:?}"
    );
}

/// A reviver that never returns is ended by the budget.
///
/// The attempt counter cannot save us here — one attempt that parks forever
/// never reaches the second. Only a wall-clock ceiling over the whole revival
/// can, which is why the bound sits outside the attempt loop rather than
/// inside it.
#[tokio::test]
async fn a_reviver_that_parks_forever_is_cut_off_by_the_wall_clock_budget() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    nas.dial_hangs.store(true, Ordering::Relaxed);

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    let started = Instant::now();
    let outcome = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("a dial that parks forever must not park the caller forever");
    let took = started.elapsed();

    assert!(
        matches!(outcome, Err(Error::ReconnectFailed { .. })),
        "expected the budget to end it with a typed error, got {outcome:?}"
    );
    assert!(
        took >= test_policy().total_budget,
        "it gave up before the budget was spent; took {took:?}"
    );
    assert!(
        took < test_policy().total_budget * 2,
        "the budget is not bounding anything if it overshoots this far: {took:?}"
    );
    assert!(conn.is_disconnected());
}

/// A whole pipeline discovers the same dead session at once. It dials once.
///
/// Thirty-two concurrent revivals would be 32 sockets, 32 authentications, and
/// 31 of them immediately thrown away — against a server that has just proven
/// it is struggling.
#[tokio::test]
async fn a_pipeline_that_all_notices_the_same_death_dials_once() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);

    assert!(finish(spawn_write(&conn), "the warm-up write")
        .await
        .is_ok());
    nas.goes_away();

    let stranded: Vec<_> = (0..32).map(|_| spawn_write(&conn)).collect();
    for (i, task) in stranded.into_iter().enumerate() {
        let outcome = finish(task, "a stranded write").await;
        assert!(outcome.is_err(), "write {i} should have failed");
    }

    let revivals: Vec<_> = (0..32)
        .map(|_| {
            let c = conn.clone();
            tokio::spawn(async move { c.reconnect_if_needed().await })
        })
        .collect();
    for (i, task) in revivals.into_iter().enumerate() {
        finish(task, "a revival")
            .await
            .unwrap_or_else(|e| panic!("caller {i} should see the shared success, got {e:?}"));
    }

    assert_eq!(nas.dial_count(), 1, "one death, one dial");
    assert_eq!(conn.metrics().reconnects_succeeded, 1);
}

/// A failed revival's verdict stands for a cooldown, so a deep pipeline does
/// not pay the full budget once per caller.
///
/// Thirty-two callers × a 60 s budget is half an hour of a frozen transfer.
/// The bound has to hold for the pipeline, not just for one caller.
#[tokio::test]
async fn a_failed_revival_is_not_re_attempted_by_every_caller_in_turn() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    nas.dials_that_fail.store(usize::MAX, Ordering::Relaxed);

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    let first = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the first revival hung");
    assert!(matches!(first, Err(Error::ReconnectFailed { .. })));
    let dials_after_first = nas.dial_count();

    let started = Instant::now();
    for i in 0..32 {
        let outcome = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
            .await
            .unwrap_or_else(|_| panic!("caller {i} hung"));
        assert!(
            matches!(outcome, Err(Error::ReconnectFailed { .. })),
            "caller {i} should get the standing verdict, got {outcome:?}"
        );
    }
    assert_eq!(
        nas.dial_count(),
        dials_after_first,
        "the cooldown should have answered from the stored verdict, not redialed"
    );
    assert!(
        started.elapsed() < test_policy().total_budget,
        "32 callers took {:?}; the whole point is that they don't each pay the budget",
        started.elapsed()
    );
}

/// Once the cooldown lapses, a caller may try again — a NAS finishing a reboot
/// has to be reachable eventually.
#[tokio::test]
async fn the_cooldown_lapses_so_a_server_that_comes_back_late_is_still_found() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    nas.dials_that_fail.store(usize::MAX, Ordering::Relaxed);

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;
    assert!(matches!(
        tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
            .await
            .expect("hung"),
        Err(Error::ReconnectFailed { .. })
    ));

    // The NAS finishes booting.
    nas.dials_that_fail.store(0, Ordering::Relaxed);
    tokio::time::sleep(test_policy().failure_cooldown + Duration::from_millis(50)).await;

    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("hung")
        .expect("the server is back; the cooldown must not latch the connection dead");
    assert!(finish(spawn_write(&conn), "the write").await.is_ok());
}

/// A socket that comes up but refuses the credentials is a failure, not a
/// half-open session.
///
/// The dangerous shape: the transport is installed and live, so the connection
/// looks usable, but no session was ever established. A caller that believes
/// it would send unauthenticated frames until the server closed the door.
#[tokio::test]
async fn a_dial_that_succeeds_but_cannot_authenticate_leaves_the_connection_dead() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);
    nas.auth_fails.store(true, Ordering::Relaxed);

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    let outcome = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("hung");
    assert!(
        matches!(
            outcome,
            Err(Error::ReconnectFailed {
                cause: crate::ErrorKind::AuthRequired,
                ..
            })
        ),
        "the cause has to survive so a consumer can ask for a password rather \
         than retry a network it never lost, got {outcome:?}"
    );
    assert!(
        conn.is_disconnected(),
        "an unauthenticated connection must not look usable"
    );
    let afterwards = tokio::time::timeout(
        TEST_BUDGET,
        conn.execute(Command::Write, &a_write(), Some(TreeId(1))),
    )
    .await
    .expect("a request on a connection with no session must fail, not park");
    assert!(matches!(afterwards, Err(Error::Disconnected)));
}

/// With no reviver installed, a dead connection stays dead and says so
/// plainly. Auto-reconnect is opt-in; nothing dials on a consumer's behalf
/// without an address and credentials it chose to hand over.
#[tokio::test]
async fn a_connection_with_no_reviver_reports_the_disconnect_rather_than_dialing() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect(&nas.current());

    nas.goes_away();
    let _ = finish(spawn_write(&conn), "the stranded write").await;

    assert!(!conn.can_reconnect());
    let outcome = tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("hung");
    assert!(
        matches!(outcome, Err(Error::Disconnected)),
        "got {outcome:?}"
    );
    assert_eq!(nas.dial_count(), 0, "nothing was dialed");
}

/// Asking a healthy connection to reconnect is free and does nothing.
#[tokio::test]
async fn reconnecting_a_connection_that_never_died_is_a_no_op() {
    let nas = BouncingNas::new(Answer::Everything);
    let conn = connect_to(&nas);

    assert!(finish(spawn_write(&conn), "the write").await.is_ok());
    conn.reconnect_if_needed().await.expect("nothing to do");

    assert_eq!(nas.dial_count(), 0);
    assert_eq!(conn.metrics().reconnects_succeeded, 0);
    assert_eq!(conn.generation(), 0);
}

// ── A NAS with a disk, and a transfer that survives it rebooting ────────────
//
// [`BouncingNas`] above answers nothing but ECHO and WRITE, which is all the
// revival machinery needs. Proving that a transfer *resumes* rather than
// *restarts* needs a server that actually keeps files: a durable handle
// survives the connection, not the storage behind it, so every generation of
// the connection has to see the same disk and the same open table.

use std::collections::HashMap;

use crate::client::durable::DurableHandle;
use crate::client::tree::Tree;
use crate::msg::close::CloseResponse;
use crate::msg::create::{CreateAction, CreateRequest, CreateResponse};
use crate::msg::create_context::{self};
use crate::msg::query_info::QueryInfoRequest;
use crate::pack::{FileTime, Guid};
use crate::types::flags::Capabilities;
use crate::types::{Dialect, FileId, OplockLevel};

/// One durable open the server is holding on the client's behalf.
#[derive(Clone)]
struct HeldOpen {
    path: String,
    /// What the client sent in `DH2Q`. A reclaim must match it.
    create_guid: Vec<u8>,
    /// The file's identity on this NAS's disk.
    inode: u64,
}

/// The storage behind every generation of the connection.
#[derive(Default)]
struct NasDisk {
    /// path → contents.
    files: Mutex<HashMap<String, Vec<u8>>>,
    /// path → inode, stable for the life of the NAS.
    inodes: Mutex<HashMap<String, u64>>,
    /// The durable opens the server is holding, keyed by the persistent id it
    /// handed out.
    opens: Mutex<HashMap<u64, HeldOpen>>,
    next_id: AtomicUsize,
    /// Every byte the server has ever accepted. The number that tells a
    /// resumed transfer from a restarted one: restarting a 40 KB file makes
    /// this 80 KB.
    bytes_accepted: AtomicUsize,
    /// When set, the next reclaim is answered with a handle to a DIFFERENT
    /// file. Not something a correct server does; the client must refuse it
    /// regardless.
    hand_back_the_wrong_file: AtomicBool,
    /// When set, durable opens are forgotten on disconnect, which is what a
    /// server that actually restarted would do.
    forget_opens_on_disconnect: AtomicBool,
}

impl NasDisk {
    fn inode_of(&self, path: &str) -> u64 {
        let mut inodes = self.inodes.lock().unwrap();
        let next = inodes.len() as u64 + 1000;
        *inodes.entry(path.to_string()).or_insert(next)
    }

    fn bytes_of(&self, path: &str) -> usize {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(Vec::len)
            .unwrap_or(0)
    }
}

/// A transport for one connection to [`NasDisk`], answering CREATE (durable
/// contexts and all), WRITE, CLOSE, and ECHO.
struct NasLink {
    disk: Arc<NasDisk>,
    outbox: Mutex<VecDeque<Vec<u8>>>,
    ready: Notify,
    /// When false the server answers nothing, which is how it looks when it
    /// goes away.
    answering: AtomicBool,
    /// The handle the CREATE in the current compound produced, so the
    /// QUERY_INFO chained behind it can resolve `FileId::SENTINEL` the way a
    /// real server substitutes the previous sub-request's handle.
    compound_handle: Mutex<Option<(FileId, u64)>>,
}

impl NasLink {
    fn new(disk: &Arc<NasDisk>) -> Arc<Self> {
        Arc::new(Self {
            disk: Arc::clone(disk),
            outbox: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            answering: AtomicBool::new(true),
            compound_handle: Mutex::new(None),
        })
    }

    fn push(&self, frame: Vec<u8>) {
        self.outbox.lock().unwrap().push_back(frame);
        self.ready.notify_one();
    }

    fn respond_to_create(&self, header: &Header, req: &CreateRequest) -> Vec<u8> {
        let path = req.name.clone();
        let asked = create_context::parse_contexts(&req.create_contexts).unwrap_or_default();
        let mut answers = Vec::new();
        let mut file_id = FileId {
            persistent: 0,
            volatile: 0,
        };
        let mut status = NtStatus::SUCCESS;
        let mut inode_answered = 0u64;

        // A reclaim's reconnect context must travel alone (MS-SMB2
        // § 3.3.5.9.12); Samba rejects one that doesn't, so this NAS does too.
        if create_context::find(&asked, create_context::NAME_DH2C).is_some() && asked.len() > 1 {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.message_id = header.message_id;
            h.credits = 8;
            h.status = NtStatus::OBJECT_NAME_NOT_FOUND;
            return pack_message(
                &h,
                &crate::msg::header::ErrorResponse {
                    error_context_count: 0,
                    error_data: vec![],
                },
            );
        }
        if let Some(dh2c) = create_context::find(&asked, create_context::NAME_DH2C) {
            // A reclaim. Look the open up and check the guid the way MS-SMB2
            // § 3.3.5.9.12 requires.
            let persistent = u64::from_le_bytes(dh2c.data[0..8].try_into().unwrap());
            let guid = dh2c.data[16..32].to_vec();
            let held = self.disk.opens.lock().unwrap().get(&persistent).cloned();
            match held {
                Some(held) if held.create_guid == guid => {
                    // A reclaimed open gets a NEW id, the way a real server
                    // hands back a fresh volatile (and often persistent) id.
                    // A client that kept writing through the old one would be
                    // writing into nothing, which the tests below rely on to
                    // stay detectable.
                    let fresh = self.disk.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
                    file_id = FileId {
                        persistent: fresh,
                        volatile: fresh,
                    };
                    let inode = if self.disk.hand_back_the_wrong_file.load(Ordering::Relaxed) {
                        held.inode + 1
                    } else {
                        held.inode
                    };
                    let mut opens = self.disk.opens.lock().unwrap();
                    opens.remove(&persistent);
                    opens.insert(fresh, held);
                    inode_answered = inode;
                }
                _ => status = NtStatus::OBJECT_NAME_NOT_FOUND,
            }
        } else {
            let persistent = self.disk.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
            file_id = FileId {
                persistent,
                volatile: persistent,
            };
            self.disk
                .files
                .lock()
                .unwrap()
                .entry(path.clone())
                .or_default();
            let inode = self.disk.inode_of(&path);
            if let Some(dh2q) = create_context::find(&asked, create_context::NAME_DH2Q) {
                self.disk.opens.lock().unwrap().insert(
                    persistent,
                    HeldOpen {
                        path: path.clone(),
                        create_guid: dh2q.data[16..32].to_vec(),
                        inode,
                    },
                );
                let mut grant = Vec::new();
                grant.extend_from_slice(&180_000u32.to_le_bytes());
                grant.extend_from_slice(&0u32.to_le_bytes());
                answers.push(create_context::CreateContext::new(
                    create_context::NAME_DH2Q,
                    grant,
                ));
            }
            inode_answered = inode;
        }

        let mut h = Header::new_request(Command::Create);
        h.flags.set_response();
        h.message_id = header.message_id;
        h.credits = 8;
        h.status = status;
        if status != NtStatus::SUCCESS {
            *self.compound_handle.lock().unwrap() = None;
            return pack_message(
                &h,
                &crate::msg::header::ErrorResponse {
                    error_context_count: 0,
                    error_data: vec![],
                },
            );
        }
        *self.compound_handle.lock().unwrap() = Some((file_id, inode_answered));
        pack_message(
            &h,
            &CreateResponse {
                oplock_level: OplockLevel::Batch,
                flags: 0,
                create_action: CreateAction::FileOpened,
                creation_time: FileTime::ZERO,
                last_access_time: FileTime::ZERO,
                last_write_time: FileTime::ZERO,
                change_time: FileTime::ZERO,
                allocation_size: 0,
                end_of_file: 0,
                file_attributes: 0,
                file_id,
                create_contexts: create_context::pack_contexts(&answers),
            },
        )
    }

    fn respond_to_write(&self, header: &Header, req: &WriteRequest) -> Vec<u8> {
        let path = self
            .disk
            .opens
            .lock()
            .unwrap()
            .get(&req.file_id.persistent)
            .map(|o| o.path.clone());
        let mut h = Header::new_request(Command::Write);
        h.flags.set_response();
        h.message_id = header.message_id;
        h.credits = 8;

        let Some(path) = path else {
            // A write through a handle this server does not know. Real servers
            // answer STATUS_FILE_CLOSED; answering anything friendlier would
            // let a client that failed to adopt a reclaimed handle look like
            // it was working.
            h.status = NtStatus::FILE_CLOSED;
            return pack_message(
                &h,
                &crate::msg::header::ErrorResponse {
                    error_context_count: 0,
                    error_data: vec![],
                },
            );
        };

        {
            let mut files = self.disk.files.lock().unwrap();
            let file = files.entry(path).or_default();
            let end = req.offset as usize + req.data.len();
            if file.len() < end {
                file.resize(end, 0);
            }
            file[req.offset as usize..end].copy_from_slice(&req.data);
        }
        self.disk
            .bytes_accepted
            .fetch_add(req.data.len(), Ordering::Relaxed);

        pack_message(
            &h,
            &WriteResponse {
                count: req.data.len() as u32,
                remaining: 0,
                write_channel_info_offset: 0,
                write_channel_info_length: 0,
            },
        )
    }
}

/// `FileInternalInformation` (MS-FSCC § 2.4.22): the file's index number.
fn file_internal_information(inode: u64) -> Vec<u8> {
    inode.to_le_bytes().to_vec()
}

/// `FileFsVolumeInformation` (MS-FSCC § 2.5.9) for this NAS's one volume.
fn fs_volume_information() -> Vec<u8> {
    let mut body = vec![0u8; 8]; // VolumeCreationTime
    body.extend_from_slice(&42u32.to_le_bytes()); // VolumeSerialNumber
    body.extend_from_slice(&0u32.to_le_bytes()); // VolumeLabelLength
    body.extend_from_slice(&[0, 0]); // SupportsObjects + Reserved
    body
}

#[async_trait]
impl TransportSend for NasLink {
    async fn send(&self, data: &[u8]) -> Result<()> {
        if !self.answering.load(Ordering::Relaxed) {
            return Ok(()); // the socket takes it; nobody is home to answer
        }
        // A durable open is a compound (CREATE + QUERY_INFO), so every
        // sub-request has to be answered. Answering them as separate frames is
        // legal (MS-SMB2 § 3.3.4.1.3) and is what Samba and QNAP often do.
        let subs = crate::client::connection::split_compound(data).unwrap_or_default();
        for sub in subs {
            self.answer_one(&sub);
        }
        Ok(())
    }
}

impl NasLink {
    fn answer_one(&self, data: &[u8]) {
        let mut cursor = ReadCursor::new(data);
        let Ok(header) = Header::unpack(&mut cursor) else {
            return;
        };
        let frame = match header.command {
            Command::Echo => {
                let mut h = Header::new_request(Command::Echo);
                h.flags.set_response();
                h.message_id = header.message_id;
                h.credits = 8;
                Some(pack_message(&h, &EchoResponse))
            }
            Command::Create => CreateRequest::unpack(&mut cursor)
                .ok()
                .map(|req| self.respond_to_create(&header, &req)),
            Command::Write => WriteRequest::unpack(&mut cursor)
                .ok()
                .map(|req| self.respond_to_write(&header, &req)),
            Command::Close => {
                let mut h = Header::new_request(Command::Close);
                h.flags.set_response();
                h.message_id = header.message_id;
                h.credits = 8;
                Some(pack_message(
                    &h,
                    &CloseResponse {
                        flags: 0,
                        creation_time: FileTime::ZERO,
                        last_access_time: FileTime::ZERO,
                        last_write_time: FileTime::ZERO,
                        change_time: FileTime::ZERO,
                        allocation_size: 0,
                        end_of_file: 0,
                        file_attributes: 0,
                    },
                ))
            }
            Command::QueryInfo => QueryInfoRequest::unpack(&mut cursor)
                .ok()
                .map(|req| self.respond_to_query_info(&header, &req)),
            _ => None,
        };
        if let Some(frame) = frame {
            self.push(frame);
        }
    }

    /// Answer a `QUERY_INFO` chained behind a CREATE: the file's index number
    /// or the volume it lives on, depending on the class asked for.
    fn respond_to_query_info(&self, header: &Header, req: &QueryInfoRequest) -> Vec<u8> {
        let mut h = Header::new_request(Command::QueryInfo);
        h.flags.set_response();
        h.message_id = header.message_id;
        h.credits = 8;
        match *self.compound_handle.lock().unwrap() {
            Some((_, inode)) => pack_message(
                &h,
                &crate::msg::query_info::QueryInfoResponse {
                    output_buffer: match req.info_type {
                        crate::msg::query_info::InfoType::Filesystem => fs_volume_information(),
                        _ => file_internal_information(inode),
                    },
                },
            ),
            None => {
                h.status = NtStatus::FILE_CLOSED;
                pack_message(
                    &h,
                    &crate::msg::header::ErrorResponse {
                        error_context_count: 0,
                        error_data: vec![],
                    },
                )
            }
        }
    }
}

#[async_trait]
impl TransportReceive for NasLink {
    async fn receive(&self) -> Result<Vec<u8>> {
        loop {
            let queued = self.outbox.lock().unwrap().pop_front();
            match queued {
                Some(frame) => return Ok(frame),
                None => self.ready.notified().await,
            }
        }
    }
}

/// The NAS as a whole: one disk, a series of connections to it.
struct Nas {
    disk: Arc<NasDisk>,
    links: Mutex<Vec<Arc<NasLink>>>,
}

impl Nas {
    fn power_on() -> Arc<Self> {
        let nas = Arc::new(Self {
            disk: Arc::new(NasDisk::default()),
            links: Mutex::new(Vec::new()),
        });
        nas.plug_in();
        nas
    }

    fn plug_in(&self) -> Arc<NasLink> {
        let link = NasLink::new(&self.disk);
        self.links.lock().unwrap().push(Arc::clone(&link));
        link
    }

    fn current(&self) -> Arc<NasLink> {
        self.links.lock().unwrap().last().unwrap().clone()
    }

    /// The NAS stops answering, without the socket noticing.
    fn goes_away(&self) {
        self.current().answering.store(false, Ordering::Relaxed);
        if self.disk.forget_opens_on_disconnect.load(Ordering::Relaxed) {
            self.disk.opens.lock().unwrap().clear();
        }
    }
}

#[async_trait]
impl SessionReviver for Nas {
    async fn dial(&self) -> Result<(Box<dyn TransportSend>, Box<dyn TransportReceive>)> {
        let link = self.plug_in();
        Ok((Box::new(Arc::clone(&link)), Box::new(link)))
    }

    async fn reauthenticate(&self, conn: &mut Connection) -> Result<()> {
        conn.set_test_params(crate::client::connection::NegotiatedParams {
            dialect: Dialect::Smb3_1_1,
            max_read_size: 65536,
            max_write_size: 65536,
            max_transact_size: 65536,
            server_guid: Guid::ZERO,
            signing_required: false,
            capabilities: Capabilities::default(),
            gmac_negotiated: false,
            cipher: None,
            compression_supported: false,
        });
        conn.set_credits(512);
        conn.set_session_id(SessionId(0xBEEF));
        Ok(())
    }
}

fn a_share() -> Tree {
    Tree {
        pin: None,
        tree_id: TreeId(1),
        share_name: "share".to_string(),
        server: "nas".to_string(),
        is_dfs: false,
        encrypt_data: false,
        dfs_origin: None,
    }
}

/// Connect to `nas`, armed to reconnect, negotiated as SMB 3.1.1.
async fn attach(nas: &Arc<Nas>) -> Connection {
    let mut conn =
        Connection::from_transport(Box::new(nas.current()), Box::new(nas.current()), "nas");
    conn.set_response_timeout(Some(BASE_DEADLINE));
    conn.set_keepalive(Some(KEEPALIVE));
    conn.set_reviver(Some(Arc::clone(nas) as Arc<dyn SessionReviver>));
    conn.set_reconnect_policy(test_policy());
    nas.reauthenticate(&mut conn).await.unwrap();
    conn
}

/// Write `data` at `offset` through `handle`.
async fn write_chunk(
    conn: &Connection,
    handle: &DurableHandle,
    offset: u64,
    data: &[u8],
) -> Result<()> {
    let req = WriteRequest {
        data_offset: 0x70,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
        offset,
        file_id: handle.file_id,
        channel: 0,
        remaining_bytes: 0,
        flags: 0,
        data: data.to_vec(),
    };
    let frame = conn.execute(Command::Write, &req, Some(TreeId(1))).await?;
    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Write,
        });
    }
    Ok(())
}

const CHUNK: usize = 4096;

/// M3's headline, end to end: a NAS goes away in the middle of a file and the
/// transfer finishes on the other side of the blip without rewriting a byte it
/// had already delivered.
///
/// Before M3 this ended the copy. Before M2 it hung forever.
#[tokio::test]
async fn a_transfer_interrupted_mid_file_resumes_instead_of_starting_over() {
    let nas = Nas::power_on();
    let mut conn = attach(&nas).await;
    let share = a_share();

    let open = share
        .open_file_durable(&mut conn, "big.iso")
        .await
        .expect("the open must succeed");
    let mut handle = open.durable.expect("this NAS grants durable handles");

    // Ten chunks in, the NAS goes away.
    let payload = vec![0xAB_u8; CHUNK];
    for i in 0..10u64 {
        write_chunk(&conn, &handle, i * CHUNK as u64, &payload)
            .await
            .expect("the first ten chunks land");
    }
    let delivered_before = nas.disk.bytes_accepted.load(Ordering::Relaxed);
    assert_eq!(delivered_before, 10 * CHUNK);

    nas.goes_away();
    let stranded = write_chunk(&conn, &handle, 10 * CHUNK as u64, &payload).await;
    assert!(
        matches!(stranded, Err(Error::ServerUnresponsive { .. })),
        "the dead session should surface first, got {stranded:?}"
    );

    // Recovery: bring the connection back, then claim the file back.
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("the revival hung")
        .expect("the NAS is answering again");
    assert!(
        !handle.is_current(&conn),
        "the handle must know it has outlived its session"
    );
    handle = share
        .reclaim_durable_handle(&mut conn, &handle, "big.iso")
        .await
        .expect("same NAS, same file, both proofs hold");
    assert!(handle.is_current(&conn));

    // Carry on from chunk 10, not from zero.
    for i in 10..20u64 {
        write_chunk(&conn, &handle, i * CHUNK as u64, &payload)
            .await
            .expect("the rest of the file lands on the new session");
    }

    assert_eq!(
        nas.disk.bytes_of("big.iso"),
        20 * CHUNK,
        "the whole file should be on disk"
    );
    assert_eq!(
        nas.disk.bytes_accepted.load(Ordering::Relaxed),
        20 * CHUNK,
        "the server accepted each byte exactly once: the ten chunks written \
         before the blip were never re-sent, which is the difference between \
         resuming and starting over"
    );
    assert_eq!(conn.metrics().reconnects_succeeded, 1);
}

/// The refusal, end to end: a NAS that matches the guid and hands back a
/// different file gets nothing written to it.
///
/// No correct server does this. The client still has to behave as though one
/// might, because the cost of being wrong is a user's bytes in a stranger's
/// file.
#[tokio::test]
async fn a_nas_that_hands_back_the_wrong_file_gets_nothing_written_to_it() {
    let nas = Nas::power_on();
    let mut conn = attach(&nas).await;
    let share = a_share();

    let handle = share
        .open_file_durable(&mut conn, "big.iso")
        .await
        .unwrap()
        .durable
        .unwrap();
    write_chunk(&conn, &handle, 0, &vec![0xAB; CHUNK])
        .await
        .unwrap();

    nas.goes_away();
    let _ = write_chunk(&conn, &handle, CHUNK as u64, &vec![0xAB; CHUNK]).await;
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("hung")
        .unwrap();

    nas.disk
        .hand_back_the_wrong_file
        .store(true, Ordering::Relaxed);
    let outcome = share
        .reclaim_durable_handle(&mut conn, &handle, "big.iso")
        .await;

    assert!(
        matches!(
            outcome,
            Err(Error::DurableHandleLost {
                reason: crate::error::DurableLoss::IdentityMismatch,
                ..
            })
        ),
        "expected a refusal, got {outcome:?}"
    );
    assert_eq!(
        nas.disk.bytes_accepted.load(Ordering::Relaxed),
        CHUNK,
        "not one byte may reach a handle we could not prove"
    );
}

/// A NAS that really rebooted has forgotten the open. A durable handle
/// survives a dead connection, not dead storage — only a persistent handle on
/// a continuously-available share does that, and this crate does not ask for
/// one.
///
/// The transfer restarts, which is the correct outcome and a far better one
/// than the permanent wedge this whole effort began with.
#[tokio::test]
async fn a_nas_that_actually_rebooted_reports_the_open_as_gone_rather_than_guessing() {
    let nas = Nas::power_on();
    nas.disk
        .forget_opens_on_disconnect
        .store(true, Ordering::Relaxed);
    let mut conn = attach(&nas).await;
    let share = a_share();

    let handle = share
        .open_file_durable(&mut conn, "big.iso")
        .await
        .unwrap()
        .durable
        .unwrap();
    write_chunk(&conn, &handle, 0, &vec![0xAB; CHUNK])
        .await
        .unwrap();

    nas.goes_away();
    let _ = write_chunk(&conn, &handle, CHUNK as u64, &vec![0xAB; CHUNK]).await;
    tokio::time::timeout(TEST_BUDGET, conn.reconnect_if_needed())
        .await
        .expect("hung")
        .expect("the connection comes back even though the handle will not");

    let outcome = share
        .reclaim_durable_handle(&mut conn, &handle, "big.iso")
        .await;
    assert!(
        matches!(
            outcome,
            Err(Error::DurableHandleLost {
                reason: crate::error::DurableLoss::Expired,
                ..
            })
        ),
        "got {outcome:?}"
    );

    // And the connection is perfectly usable for starting the file again.
    let fresh = share
        .open_file_durable(&mut conn, "big.iso")
        .await
        .expect("a reopen must work");
    assert!(fresh.durable.is_some());
}

// ── A link that is answering, slowly ───────────────────────────────────────
//
// Everything above scripts whole frames, so none of it can express the one
// shape a slow link produces all day: a response that is ARRIVING. An 8 MB READ
// over a 200 KB/s link spends 40 s on the wire, and TCP delivers it in order,
// so every other answer (ECHO replies included) waits behind it. These tests
// put a real socket under the connection, so the frame really does arrive a
// piece at a time through `TcpTransport`.

/// How a [`SlowLink`] dribbles out a large response.
#[derive(Clone, Copy)]
struct Dribble {
    /// Bytes per write.
    piece: usize,
    /// Pause between writes.
    gap: Duration,
    /// Stop after this many pieces and hold the socket open, saying nothing
    /// more. `None` sends the whole frame.
    stop_after: Option<usize>,
}

/// The far end of a loopback socket: a server that answers every request, in
/// the order it received them, one writer for the whole stream, the way TCP
/// forces a real one to.
struct SlowLink {
    addr: std::net::SocketAddr,
    echoes: Arc<AtomicUsize>,
}

impl SlowLink {
    /// Answer READs with `read_len` bytes of data, dribbled per `dribble`.
    /// Everything else small is written at once, but only after whatever is
    /// ahead of it in the stream.
    async fn start(read_len: usize, dribble: Dribble) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let echoes = Arc::new(AtomicUsize::new(0));
        let seen_echoes = Arc::clone(&echoes);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

            tokio::spawn(async move {
                while let Some(body) = rx.recv().await {
                    let mut framed = Vec::with_capacity(body.len() + 4);
                    framed.push(0);
                    framed.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
                    framed.extend_from_slice(&body);
                    if framed.len() <= dribble.piece {
                        if writer.write_all(&framed).await.is_err() {
                            return;
                        }
                        continue;
                    }
                    for (i, piece) in framed.chunks(dribble.piece).enumerate() {
                        if dribble.stop_after == Some(i) {
                            // Hold the socket open and never finish the frame.
                            std::future::pending::<()>().await;
                        }
                        if writer.write_all(piece).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(dribble.gap).await;
                    }
                }
            });

            loop {
                let mut len = [0u8; 4];
                if reader.read_exact(&mut len).await.is_err() {
                    return;
                }
                let len = u32::from_be_bytes(len) as usize;
                let mut request = vec![0u8; len];
                if reader.read_exact(&mut request).await.is_err() {
                    return;
                }
                let Ok(header) = Header::unpack(&mut ReadCursor::new(&request)) else {
                    continue;
                };
                let mut h = Header::new_request(header.command);
                h.flags.set_response();
                h.message_id = header.message_id;
                h.credits = 8;
                let response = match header.command {
                    Command::Echo => {
                        seen_echoes.fetch_add(1, Ordering::Relaxed);
                        pack_message(&h, &EchoResponse)
                    }
                    Command::Read => pack_message(
                        &h,
                        &crate::msg::read::ReadResponse {
                            data_offset: 0x50,
                            data_remaining: 0,
                            flags: 0,
                            data: vec![0x5A; read_len],
                        },
                    ),
                    _ => continue,
                };
                if tx.send(response).is_err() {
                    return;
                }
            }
        });
        Self { addr, echoes }
    }

    /// A connection to this link, tuned to the scaled-down timings.
    async fn connect(&self) -> Connection {
        let mut conn = Connection::connect(&self.addr.to_string(), TEST_BUDGET)
            .await
            .expect("loopback connect");
        conn.set_credits(512);
        conn.set_session_id(SessionId(0x5E55));
        conn.set_response_timeout(Some(BASE_DEADLINE));
        conn.set_keepalive(Some(KEEPALIVE));
        conn
    }
}

/// Issue a READ of `length` bytes on its own task.
fn spawn_read(
    conn: &Connection,
    length: u32,
) -> tokio::task::JoinHandle<Result<crate::client::Frame>> {
    let c = conn.clone();
    tokio::spawn(async move {
        let req = crate::msg::read::ReadRequest {
            padding: 0x50,
            flags: 0,
            length,
            offset: 0,
            file_id: crate::types::FileId {
                persistent: 1,
                volatile: 2,
            },
            minimum_count: 0,
            channel: 0,
            remaining_bytes: 0,
            read_channel_info: vec![],
        };
        c.execute(Command::Read, &req, Some(TreeId(1))).await
    })
}

/// The bug a slow link used to hit on every large read: bytes arriving the
/// whole time, and the client calling the server dead anyway.
///
/// The response takes well over the silence budget to arrive but is moving
/// every few milliseconds. Only whole frames used to count as the server
/// speaking, so the wire read as silent from the moment the READ went out; the
/// ECHO probes that should have proved otherwise were stuck behind the very
/// frame they were asking about, and the read ended in `ServerUnresponsive`
/// with the connection torn down underneath every other caller.
#[tokio::test]
async fn a_response_still_arriving_is_not_silence() {
    const READ_LEN: usize = 320 * 1024;
    // 40 pieces 25 ms apart: about a second on the wire, well past the
    // 600 ms silence budget, and never more than 25 ms without a byte.
    let link = SlowLink::start(
        READ_LEN,
        Dribble {
            piece: 8 * 1024,
            gap: Duration::from_millis(25),
            stop_after: None,
        },
    )
    .await;
    let conn = link.connect().await;

    let started = Instant::now();
    let outcome = finish(spawn_read(&conn, READ_LEN as u32), "the read").await;
    let took = started.elapsed();

    assert!(
        outcome.is_ok(),
        "the response was arriving the whole time, so the read must succeed; got {outcome:?} \
         after {took:?}"
    );
    assert!(
        took > BASE_DEADLINE,
        "the frame should outlast the silence budget, or this test proves nothing ({took:?})"
    );
    assert!(!conn.diagnostics().disconnected);
    assert_eq!(conn.metrics().response_timeouts, 0);
}

/// The guard on the other side: bytes that STOP are silence again. A server
/// that dies halfway through a frame must still be declared dead, or counting
/// partial frames would have traded a false death for a hang.
#[tokio::test]
async fn a_response_that_stops_halfway_is_still_a_death() {
    const READ_LEN: usize = 320 * 1024;
    let link = SlowLink::start(
        READ_LEN,
        Dribble {
            piece: 8 * 1024,
            gap: Duration::from_millis(10),
            stop_after: Some(8),
        },
    )
    .await;
    let conn = link.connect().await;

    let outcome = finish(spawn_read(&conn, READ_LEN as u32), "the read").await;
    assert!(
        matches!(outcome, Err(Error::ServerUnresponsive { .. })),
        "a frame that stopped arriving is a dead link, got {outcome:?}"
    );
    assert!(
        link.echoes.load(Ordering::Relaxed) >= 1,
        "the verdict has to rest on probes"
    );
}

/// What a consumer polling the connection sees while that happens: bytes
/// moving inside a frame that hasn't finished, and a server that reads as
/// alive rather than as quiet.
#[tokio::test]
async fn a_frame_still_arriving_shows_up_in_the_readings() {
    use crate::client::diagnostics::Liveness;

    const READ_LEN: usize = 640 * 1024;
    let link = SlowLink::start(
        READ_LEN,
        Dribble {
            piece: 8 * 1024,
            gap: Duration::from_millis(25),
            stop_after: None,
        },
    )
    .await;
    let conn = link.connect().await;
    assert_eq!(conn.liveness(), Liveness::Idle, "nothing asked yet");

    let read = spawn_read(&conn, READ_LEN as u32);
    // The READ response is the only frame big enough to be seen partway in.
    let reader = conn.clone();
    wait_until("the response to be partway in", || {
        reader
            .inbound()
            .frame
            .is_some_and(|f| f.received > 0 && f.received < f.len)
    })
    .await;
    let first = conn.inbound().bytes_received;
    let reader = conn.clone();
    wait_until("more of it to land", || {
        let now = reader.inbound();
        now.bytes_received > first && now.frame.is_some()
    })
    .await;
    let reader = conn.clone();
    wait_until("the connection to read as alive mid-frame", || {
        reader.liveness() == Liveness::Alive
    })
    .await;

    finish(read, "the read").await.expect("the read completes");
    let done = conn.inbound();
    assert_eq!(done.frame, None, "between frames");
    assert_eq!(
        done.bytes_received,
        conn.metrics().wire_bytes_received,
        "once nothing is partway in, the live count agrees with the whole-frame one"
    );
    assert!(done.since_last_byte.is_some());
}

/// The reading a transfer watchdog needs: a dead link called dead while the
/// request is still waiting, rather than only after it has burned its deadline
/// and taken the connection down with it.
#[tokio::test]
async fn a_dead_link_reads_unresponsive_before_any_request_pays_for_it() {
    use crate::client::diagnostics::Liveness;

    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    // Far more budget than the liveness window, so the reading has to arrive
    // while the write is still waiting.
    conn.set_response_timeout(Some(BASE_DEADLINE * 10));

    let write = spawn_write(&conn);
    let reader = conn.clone();
    wait_until("the connection to read as unresponsive", || {
        matches!(reader.liveness(), Liveness::Unresponsive { .. })
    })
    .await;
    assert!(
        !write.is_finished(),
        "reading the verdict must not act on it"
    );
    assert!(
        !conn.diagnostics().disconnected,
        "reading the verdict must not tear anything down"
    );
    assert_eq!(conn.metrics().response_timeouts, 0);
    write.abort();
}

/// With the keepalive off, silence is only silence: nothing is asking the
/// server a question it could answer without a disk, so no amount of it is
/// evidence of death.
#[tokio::test]
async fn with_the_keepalive_off_a_quiet_link_never_reads_unresponsive() {
    use crate::client::diagnostics::Liveness;

    let server = ScriptedServer::new(Answer::Nothing);
    let conn = connect(&server);
    conn.set_keepalive(None);
    conn.set_response_timeout(Some(BASE_DEADLINE * 10));

    let write = spawn_write(&conn);
    // Well past the liveness window the keepalive would have used.
    tokio::time::sleep(KEEPALIVE * 6).await;
    let reading = conn.liveness();
    assert!(
        matches!(reading, Liveness::Quiet { .. }),
        "expected a quiet link with no verdict, got {reading:?}"
    );
    write.abort();

    conn.mark_dead();
    assert_eq!(conn.liveness(), Liveness::Disconnected);
}
