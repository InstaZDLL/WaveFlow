//! The engine every pipelined upload runs on.
//!
//! [`WritePipe`] sends the WRITEs of one open handle, paced by the same
//! `Window` a download uses (see `write_behind.rs` for what that means for an
//! upload), and bounded by the connection-wide write budget. `FileWriter`,
//! `FileUpload`, `Tree::write_file_pipelined`, `Tree::write_file_streamed`,
//! and `SmbClient::write_file_with_progress` all drive one, so an upload
//! paces the same way whichever of them started it. The handle's lifecycle
//! (CREATE, FLUSH, CLOSE) stays with the caller.

use std::future::Future;
use std::pin::{pin, Pin};

use crate::rt::{self, Instant};
use futures_util::future::{select, Either};
use futures_util::stream::{FuturesUnordered, StreamExt};
use log::debug;

use crate::client::connection::{
    reserve_write_budget_or_drain, Connection, Frame, WriteBudgetStep,
};
use crate::client::credits;
use crate::client::read_ahead::{Correction, Dispatch, Outstanding, Window};
use crate::client::write_behind::{WriteBehind, UPLOAD_CHUNK_SIZE};
use crate::error::Result;
use crate::msg::write::{WriteRequest, WriteResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, FileId, TreeId};
use crate::Error;

/// One WRITE on its way: handed to the connection, not yet confirmed.
///
/// The write-budget permit rides inside the future, so every way it ends
/// (confirmed, abandoned, dropped with the pipe) returns the budget with no
/// explicit release anywhere. ❌ Don't hold it on the pipe instead: a writer
/// dropped mid-flight (a user cancelling a copy) would strand the budget for
/// the life of the connection.
type InFlightWrite = Pin<Box<dyn Future<Output = Landed> + Send>>;

/// A WRITE's answer, with what the window needs to learn from it.
struct Landed {
    dispatched_at: Instant,
    /// When the answer came off the wire, which can be well before the pipe
    /// looks at it: a push-based upload only polls its answers when the
    /// producer hands it more data or finishes.
    arrived_at: Option<Instant>,
    len: u32,
    frame: Result<Frame>,
}

/// Sends the WRITEs of one open file handle, pacing them to the link.
pub(crate) struct WritePipe {
    conn: Connection,
    tree_id: TreeId,
    file_id: FileId,
    /// The server's `MaxWriteSize`, which no chunk may exceed.
    max_write: u32,
    chunk_size: u32,
    policy: WriteBehind,
    /// Where the next WRITE lands in the file.
    offset: u64,
    in_flight: FuturesUnordered<InFlightWrite>,
    in_flight_bytes: u64,
    peak_in_flight_bytes: u64,
    /// Bytes the server confirmed.
    confirmed: u64,
    /// Built on the first send, from what the connection knows by then.
    window: Option<Window>,
}

impl WritePipe {
    /// A pipe writing from the start of `file_id`, in
    /// [`UPLOAD_CHUNK_SIZE`] chunks (or `max_write`, if that's smaller),
    /// with the adaptive window.
    pub(crate) fn new(conn: Connection, tree_id: TreeId, file_id: FileId, max_write: u32) -> Self {
        let max_write = max_write.max(1);
        Self {
            conn,
            tree_id,
            file_id,
            max_write,
            chunk_size: UPLOAD_CHUNK_SIZE.min(max_write),
            policy: WriteBehind::Adaptive,
            offset: 0,
            in_flight: FuturesUnordered::new(),
            in_flight_bytes: 0,
            peak_in_flight_bytes: 0,
            confirmed: 0,
            window: None,
        }
    }

    pub(crate) fn set_policy(&mut self, policy: WriteBehind) {
        self.policy = policy;
        self.window = None;
    }

    /// Clamped to one byte and `MaxWriteSize`: a bigger WRITE is one the
    /// server refuses.
    pub(crate) fn set_chunk_size(&mut self, chunk_size: u32) {
        self.chunk_size = chunk_size.clamp(1, self.max_write);
        self.window = None;
    }

    pub(crate) fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    pub(crate) fn policy(&self) -> WriteBehind {
        self.policy
    }

    pub(crate) fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// How many bytes the next WRITE should carry: the chunk, lowered to what
    /// the credit window funds right now. Callers slice their data by this.
    pub(crate) fn next_len(&self) -> u32 {
        self.conn.fundable_chunk(self.chunk_size)
    }

    /// Where the next WRITE lands. Once the pipe is drained, every byte
    /// before it has been confirmed.
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn confirmed(&self) -> u64 {
        self.confirmed
    }

    pub(crate) fn peak_in_flight_bytes(&self) -> u64 {
        self.peak_in_flight_bytes
    }

    /// Send `data` as one WRITE at the current offset, once the window and
    /// the connection's write budget have room for it. Waiting for room is
    /// the backpressure: it confirms earlier WRITEs as their answers land.
    ///
    /// On an error nothing more is sent; WRITEs still out are drained first,
    /// so the caller can close the handle straight away.
    pub(crate) async fn send(&mut self, data: Vec<u8>) -> Result<()> {
        let len = u32::try_from(data.len()).expect("a WRITE is at most MaxWriteSize");
        let sent = async {
            self.make_room(len).await?;
            let permit = self.reserve_budget(u64::from(len)).await?;
            self.dispatch(data, permit).await
        }
        .await;
        match sent {
            Ok(()) => Ok(()),
            Err(e) => Err(self.fail(e).await),
        }
    }

    /// Wait for one WRITE's answer and account for it. `Ok(false)` when
    /// nothing is in flight.
    pub(crate) async fn confirm_next(&mut self) -> Result<bool> {
        let Some(landed) = self.in_flight.next().await else {
            return Ok(false);
        };
        match self.land(landed) {
            Ok(()) => Ok(true),
            Err(e) => Err(self.fail(e).await),
        }
    }

    /// Wait for every WRITE still out.
    pub(crate) async fn drain(&mut self) -> Result<()> {
        while self.confirm_next().await? {}
        Ok(())
    }

    /// Wait out the WRITEs still out without caring how they went, for a
    /// caller that is giving up on the file. Stops at the first transport
    /// error: the connection may be the thing that failed.
    pub(crate) async fn abandon(&mut self) {
        while let Some(landed) = self.in_flight.next().await {
            self.in_flight_bytes -= u64::from(landed.len);
            match landed.frame {
                Ok(frame) if frame.header.status == NtStatus::SUCCESS => {
                    if let Ok(resp) = WriteResponse::unpack(&mut ReadCursor::new(&frame.body)) {
                        self.confirmed += u64::from(resp.count);
                    }
                }
                Ok(frame) => debug!(
                    "write_pipe: ignoring WRITE status {:?} while giving up",
                    frame.header.status
                ),
                Err(e) => {
                    debug!(
                        "write_pipe: giving up on {} WRITE answer(s) after a transport error: {e}",
                        self.in_flight.len()
                    );
                    break;
                }
            }
        }
        self.in_flight = FuturesUnordered::new();
        self.in_flight_bytes = 0;
    }

    /// Drain what's still out, then hand the error back.
    async fn fail(&mut self, e: Error) -> Error {
        self.abandon().await;
        e
    }

    /// Wait until the window lets a WRITE of `len` bytes go, confirming
    /// answers that land meanwhile.
    async fn make_room(&mut self, len: u32) -> Result<()> {
        loop {
            let (writes, bytes) = (self.in_flight.len(), self.in_flight_bytes);
            match self.window().decide(Instant::now(), writes, bytes, len) {
                Dispatch::Now => return Ok(()),
                Dispatch::AfterHead => {
                    let landed = self
                        .in_flight
                        .next()
                        .await
                        .expect("the window sends at once when nothing is in flight");
                    self.land(landed)?;
                }
                Dispatch::At(at) => {
                    let timer = pin!(rt::sleep_until(at));
                    match select(self.in_flight.next(), timer).await {
                        Either::Left((Some(landed), _)) => self.land(landed)?,
                        Either::Left((None, _)) => return Ok(()),
                        // Due: the WRITE ahead is about to finish its way up.
                        Either::Right(_) => return Ok(()),
                    }
                }
            }
        }
    }

    /// The connection-wide write budget for one WRITE. The deadlock-safe
    /// ordering lives in `reserve_write_budget_or_drain`; this loop only
    /// accounts for the answers that ordering drains on the way. ❌ Don't
    /// inline a shortcut past it.
    async fn reserve_budget(
        &mut self,
        bytes: u64,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>> {
        loop {
            match reserve_write_budget_or_drain(&self.conn, bytes, &mut self.in_flight).await {
                WriteBudgetStep::Granted(permit) => return Ok(permit),
                WriteBudgetStep::Drained(landed) => self.land(landed)?,
            }
        }
    }

    /// Put one WRITE on the wire and keep its answer coming.
    async fn dispatch(
        &mut self,
        data: Vec<u8>,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<()> {
        let len = data.len() as u32;
        let req = WriteRequest {
            data_offset: 0x70,
            offset: self.offset,
            file_id: self.file_id,
            channel: 0,
            remaining_bytes: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
            flags: 0,
            data,
        };
        let charge = CreditCharge(credits::charge_for_payload(u64::from(len)));
        // Taken before the send: from here the bytes queue ahead of whatever
        // the connection sends next, which is what the window budgets.
        let dispatched_at = Instant::now();
        let mut guard = self
            .conn
            .dispatch_with_credits(Command::Write, &req, Some(self.tree_id), charge)
            .await?;
        self.offset += u64::from(len);
        self.in_flight_bytes += u64::from(len);
        self.peak_in_flight_bytes = self.peak_in_flight_bytes.max(self.in_flight_bytes);
        self.window().on_dispatch(dispatched_at, len);

        let conn = self.conn.clone();
        self.in_flight.push(Box::pin(async move {
            let _budget = permit;
            let frame = conn
                .await_response_in_place(&mut guard, Command::Write)
                .await;
            Landed {
                dispatched_at,
                arrived_at: guard.arrived_at(),
                len,
                frame,
            }
        }));
        Ok(())
    }

    /// Account for one WRITE's answer. Synchronous, so nothing between the
    /// answer and the bookkeeping can be interrupted.
    fn land(&mut self, landed: Landed) -> Result<()> {
        self.in_flight_bytes -= u64::from(landed.len);
        let frame = landed.frame?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Write,
            });
        }
        let resp = WriteResponse::unpack(&mut ReadCursor::new(&frame.body))?;
        // A server that wrote less than it was sent leaves a hole the WRITEs
        // after it don't know about, so nothing past here is the file the
        // caller pushed. ❌ Don't count the part that landed: a checkpoint
        // built on it would vouch for bytes that were never written.
        if resp.count < landed.len {
            return Err(Error::invalid_data(format!(
                "the server wrote {} of the {} bytes a WRITE sent",
                resp.count, landed.len
            )));
        }
        self.confirmed += u64::from(resp.count);

        // Each confirmation is handled as it lands, so what's in flight is
        // exactly what the server hasn't answered.
        let outstanding = Outstanding::all_unanswered(self.in_flight_bytes);
        let now = Instant::now();
        self.window().on_delivery(
            now,
            landed.dispatched_at,
            landed.arrived_at.unwrap_or(now),
            landed.len,
            outstanding,
        );
        if let Some(window) = &self.window {
            self.conn.note_write(window);
        }
        Ok(())
    }

    fn window(&mut self) -> &mut Window {
        let (policy, chunk) = (self.policy, self.chunk_size);
        let conn = &self.conn;
        self.window.get_or_insert_with(|| {
            Window::new(policy, chunk, conn.write_link_hint())
                .with_correction(Correction::Throughout)
        })
    }
}
