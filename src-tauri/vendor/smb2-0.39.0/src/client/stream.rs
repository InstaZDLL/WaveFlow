//! Streaming file I/O with progress reporting.
//!
//! Provides [`FileDownload`] for memory-efficient large file downloads,
//! [`FileReader`] for random-access positioned reads over one open handle,
//! [`FileUpload`] for streaming uploads with progress,
//! [`FileWriter`] for push-based pipelined writes (use
//! [`FileWriter::finish`] for normal completion, [`FileWriter::abort`] for
//! fast cancellation), and [`Progress`] for tracking transfer progress.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::sync::Arc;

use crate::rt::{self, Instant};
use futures_util::future::{select, Either};
use log::{debug, trace};

use crate::client::connection::{Connection, Frame, WaiterGuard};
use crate::client::credits;
use crate::client::durable::FileIdentity;
use crate::client::read_ahead::{Dispatch, Outstanding, Window};
pub use crate::client::read_ahead::{ReadAhead, DOWNLOAD_CHUNK_SIZE};
use crate::client::resolve::OpenFor;
use crate::client::times::FileTimes;
use crate::client::tree::{close_outcome, file_info_from_create, FileInfo, Tree};
pub use crate::client::write_behind::{WriteBehind, UPLOAD_CHUNK_SIZE};
use crate::client::write_pipe::WritePipe;
use crate::error::{Result, ResumeRefusal};
use crate::msg::create::CreateDisposition;
use crate::msg::read::{ReadRequest, ReadResponse, SMB2_CHANNEL_NONE};
use crate::pack::{ReadCursor, Unpack};
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, FileId};
use crate::Error;

/// Progress information for a file transfer.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total file size (if known).
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// Progress as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn percent(&self) -> f64 {
        self.fraction() * 100.0
    }

    /// Progress as a fraction (0.0 to 1.0).
    #[must_use]
    pub fn fraction(&self) -> f64 {
        match self.total_bytes {
            Some(total) if total > 0 => self.bytes_transferred as f64 / total as f64,
            Some(_) => 1.0, // Empty file is "complete"
            None => 0.0,
        }
    }
}

/// An in-progress file download that yields chunks without buffering
/// the entire file in memory.
///
/// Chunks come back in file order, one per
/// [`next_chunk`](FileDownload::next_chunk). Underneath, several READs can be
/// on the wire at once: by default the download sizes that window to the link
/// ([`ReadAhead::Adaptive`]), so a fast link stays full while a slow one
/// queues no more than about one chunk ahead of anything else on the
/// connection. [`with_read_ahead`](Self::with_read_ahead) pins it instead. A
/// file that fits one chunk costs exactly one READ.
///
/// At most [`ADAPTIVE_MAX_IN_FLIGHT`](crate::client::read_ahead::ADAPTIVE_MAX_IN_FLIGHT)
/// bytes (4 MiB) are requested but undelivered at a time, so that is also what
/// one download buffers at most.
///
/// `next_chunk` is cancel-safe: dropping its future (a `select!` arm losing)
/// loses no data, and the next call picks up where it left off. The CLOSE goes
/// out as the last chunk arrives; a download dropped before that closes its
/// handle as it drops (see [`FileWriter`]'s "Dropping it" section).
///
/// # Example
///
/// ```ignore
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// use tokio::io::AsyncWriteExt;
///
/// let mut download = client.download(&share, "big_video.mp4").await?;
/// println!("Downloading {} bytes...", download.size());
///
/// let mut file = tokio::fs::File::create("big_video.mp4").await?;
/// while let Some(chunk) = download.next_chunk().await {
///     let bytes = chunk?;
///     file.write_all(&bytes).await?;
///     println!("{:.1}%", download.progress().percent());
/// }
/// # Ok(())
/// # }
/// ```
pub struct FileDownload<'a> {
    /// Borrowed from the caller, or owned when a DFS link led elsewhere.
    tree: Cow<'a, Tree>,
    /// Pinned to the session the file was opened on, so nothing this download
    /// sends can reach the next one (see `Connection::pinned_to_session`).
    conn: Connection,
    /// The caller's connection stays borrowed for the download's life, as the
    /// constructors' signatures have always said.
    _caller: PhantomData<&'a mut Connection>,
    file_id: FileId,
    file_size: u64,
    /// What the CREATE said about the file; `None` for a handle the caller
    /// opened and passed to [`FileDownload::new`].
    info: Option<FileInfo>,
    bytes_received: u64,
    chunk_size: u32,
    done: bool,
    read_ahead: ReadAhead,
    /// Built on the first READ, so the builder methods can still change what
    /// it's built from.
    window: Option<Window>,
    /// Offset of the next READ to request.
    next_offset: u64,
    /// Requested READs in offset order. Chunks are delivered from the front,
    /// so a response that lands early waits in its guard.
    in_flight: VecDeque<InFlightRead>,
    /// Bytes requested but not yet delivered.
    in_flight_bytes: u64,
    peak_in_flight_bytes: u64,
    /// A chunk already taken off the wire but not yet handed out. It waits
    /// here across the awaits that follow a delivery (the next READs, the
    /// CLOSE), so a caller that drops `next_chunk` there gets it on the next
    /// call rather than losing it.
    ready: Option<Vec<u8>>,
    /// Whether the CLOSE was started. Set before it's sent, so the handle
    /// never gets a second one (see `Tree::close_dropped_handle`); until
    /// then, dropping the download closes the handle.
    close_started: bool,
    /// The CLOSE, once it's on the wire and until its answer is collected.
    /// Sent as the last chunk lands, so handing that chunk out never waits a
    /// round trip for it; the `None` call after the last chunk collects it.
    closing: Option<WaiterGuard>,
    /// Why the CLOSE couldn't be sent, held for the `None` call so the last
    /// chunk, whose data is good, is still handed out.
    close_failed: Option<Error>,
}

/// One requested READ of a [`FileDownload`].
///
/// Dropping it drops the [`WaiterGuard`], which deregisters the waiter; the
/// late response is then discarded by the receiver task (its credits still
/// bank).
struct InFlightRead {
    offset: u64,
    len: u32,
    /// `None` for the remainder of a short read, until it's sent.
    guard: Option<WaiterGuard>,
    dispatched_at: Instant,
}

impl<'a> FileDownload<'a> {
    /// Create a new streaming download from an already-opened file handle.
    ///
    /// Most callers want [`SmbClient::download`](crate::SmbClient::download) or
    /// [`Tree::download`](crate::Tree::download), which issue the CREATE
    /// themselves and wrap the resulting handle. Use this constructor when
    /// you've already opened the file via [`Tree::open_file`] (for example,
    /// to reuse a handle across multiple readers, or to build a custom
    /// chunk loop with non-default `chunk_size`).
    ///
    /// `chunk_size` must not exceed the server's `MaxReadSize`. The download
    /// uses [`ReadAhead::Adaptive`] unless told otherwise.
    ///
    /// The caller is responsible for making sure `file_id` belongs to `tree`
    /// and was opened with read access, on the session `conn` has now. The
    /// `FileDownload` will CLOSE the handle when the last chunk arrives. Once
    /// that session ends (a reconnect), every read fails with
    /// [`Error::HandleInvalidated`].
    pub fn new(
        tree: &'a Tree,
        conn: &'a mut Connection,
        file_id: FileId,
        file_size: u64,
        chunk_size: u32,
    ) -> Self {
        Self::with_tree(
            Cow::Borrowed(tree),
            tree.pinned_conn(conn).pinned_to_session(),
            file_id,
            file_size,
            chunk_size,
        )
    }

    /// A download of a file [`Tree::open_file`] just opened, reading
    /// [`DOWNLOAD_CHUNK_SIZE`](crate::DOWNLOAD_CHUNK_SIZE) at a time (or
    /// `MaxReadSize`, if that's smaller).
    ///
    /// `conn` is the clone the file was opened on, pinned before the CREATE.
    pub(crate) fn of_open_file(
        tree: Cow<'a, Tree>,
        conn: Connection,
        file_id: FileId,
        info: FileInfo,
    ) -> Self {
        let max_read = conn.params().map_or(65536, |p| p.max_read_size);
        let chunk_size = crate::DOWNLOAD_CHUNK_SIZE.min(max_read);
        let mut download = Self::with_tree(tree, conn, file_id, info.size, chunk_size);
        download.info = Some(info);
        download
    }

    fn with_tree(
        tree: Cow<'a, Tree>,
        conn: Connection,
        file_id: FileId,
        file_size: u64,
        chunk_size: u32,
    ) -> Self {
        let conn = tree.pinned_conn(&conn).pinned_to_session();
        Self {
            tree,
            conn,
            _caller: PhantomData,
            file_id,
            file_size,
            info: None,
            bytes_received: 0,
            chunk_size: chunk_size.max(1),
            done: false,
            read_ahead: ReadAhead::default(),
            window: None,
            next_offset: 0,
            in_flight: VecDeque::new(),
            in_flight_bytes: 0,
            peak_in_flight_bytes: 0,
            ready: None,
            close_started: false,
            closing: None,
            close_failed: None,
        }
    }

    /// Choose how many READs to keep on the wire. See [`ReadAhead`].
    ///
    /// Takes effect from the next READ sent; meant to be called before the
    /// first [`next_chunk`](Self::next_chunk).
    #[must_use]
    pub fn with_read_ahead(mut self, read_ahead: ReadAhead) -> Self {
        self.read_ahead = read_ahead;
        self.window = None;
        self
    }

    /// Change how many bytes each READ asks for. It must not exceed the
    /// server's `MaxReadSize`.
    ///
    /// Takes effect from the next READ sent; meant to be called before the
    /// first [`next_chunk`](Self::next_chunk).
    #[must_use]
    pub fn with_chunk_size(mut self, chunk_size: u32) -> Self {
        self.chunk_size = chunk_size.max(1);
        self.window = None;
        self
    }

    /// The read-ahead policy in use.
    #[must_use]
    pub fn read_ahead(&self) -> ReadAhead {
        self.read_ahead
    }

    /// Bytes each READ asks for.
    #[must_use]
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// The most bytes this download has had requested but not yet delivered
    /// at once. A gauge for tuning; it's also the most it buffered.
    #[must_use]
    pub fn peak_in_flight_bytes(&self) -> u64 {
        self.peak_in_flight_bytes
    }

    /// Total file size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.file_size
    }

    /// The file's metadata as the server reported it when it opened the file:
    /// size, whether it's a directory, and its creation, modification, and
    /// access times. It rides on the CREATE that started the download, so it
    /// costs no extra round trip.
    ///
    /// `Some` for every download [`Tree::download`] or
    /// [`SmbClient::download`](crate::SmbClient::download) started. `None` for
    /// one built with [`FileDownload::new`] around a handle the caller opened,
    /// since that handle's CREATE response never passed through here.
    #[must_use]
    pub fn info(&self) -> Option<&FileInfo> {
        self.info.as_ref()
    }

    /// Bytes received so far.
    #[must_use]
    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Current transfer progress.
    #[must_use]
    pub fn progress(&self) -> Progress {
        Progress {
            bytes_transferred: self.bytes_received,
            total_bytes: Some(self.file_size),
        }
    }

    /// Get the next chunk of data from the server.
    ///
    /// Returns `None` when the download is complete. Chunks come in file
    /// order and are at most the chunk size; a short read from the server
    /// shows up as a shorter chunk. After an error, the download is over:
    /// every later call returns `None`, and the handle is left open (the
    /// connection may be what failed).
    ///
    /// The CLOSE goes out as the last chunk lands, and the last chunk is handed
    /// out without waiting for its answer. The call after the last chunk
    /// collects that answer: it returns `None`, or `Some(Err(_))` if the CLOSE
    /// failed. A caller that stops at the last byte instead (dropping the
    /// download without that call) still gets the handle closed, since the
    /// server acts on the CLOSE either way; only a CLOSE error goes unseen.
    ///
    /// Cancel-safe: if this future is dropped before it completes, no data is
    /// lost and the next call continues the download.
    pub async fn next_chunk(&mut self) -> Option<Result<Vec<u8>>> {
        if self.ready.is_none() {
            if self.closing.is_some() || self.close_failed.is_some() {
                return self.finish_close().await.err().map(Err);
            }
            if self.done {
                return None;
            }
            match self.receive_next().await {
                Ok(Some(data)) => self.ready = Some(data),
                Ok(None) => return self.close().await.err().map(Err),
                Err(e) => return Some(Err(self.fail(e))),
            }
        }
        // Keep the wire busy while the caller works on this chunk, or send
        // the CLOSE if it was the last. The chunk waits in `ready` meanwhile.
        let all_delivered = self.in_flight.is_empty() && self.next_offset >= self.file_size;
        if all_delivered {
            if let Err(e) = self.start_close().await {
                self.close_failed = Some(e);
            }
        } else if let Err(e) = self.send_reads().await {
            self.ready = None;
            return Some(Err(self.fail(e)));
        }
        self.ready.take().map(Ok)
    }

    /// Wait for the chunk at the front, sending READs as the window allows
    /// meanwhile. `Ok(None)` means there's nothing left to receive.
    async fn receive_next(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let send_at = self.send_reads().await?;
            let Some(head) = self.in_flight.front_mut() else {
                return Ok(None);
            };
            let guard = head
                .guard
                .as_mut()
                .expect("send_reads sends the head before anything behind it");
            let waiting = self.conn.await_response_in_place(guard, Command::Read);
            let frame = match send_at {
                None => waiting.await?,
                Some(at) => {
                    let timer = std::pin::pin!(rt::sleep_until(at));
                    match select(std::pin::pin!(waiting), timer).await {
                        Either::Left((frame, _)) => frame?,
                        // The next READ is due before the head has landed.
                        // The head's response keeps waiting in its guard.
                        Either::Right(_) => continue,
                    }
                }
            };
            let arrived_at = guard.arrived_at().unwrap_or_else(Instant::now);
            return self.take_head(frame, arrived_at);
        }
    }

    /// Send every READ the window allows right now. Returns when the next one
    /// is due, if that's a time rather than "after the head is delivered".
    async fn send_reads(&mut self) -> Result<Option<Instant>> {
        // The rest of a short read goes out first: it's the next chunk due.
        if let Some(front) = self.in_flight.front_mut() {
            if front.guard.is_none() {
                let guard = send_read(
                    &self.conn,
                    &self.tree,
                    self.file_id,
                    front.offset,
                    front.len,
                )
                .await?;
                let now = Instant::now();
                front.guard = Some(guard);
                front.dispatched_at = now;
                let len = front.len;
                self.window().on_dispatch(now, len);
            }
        }
        loop {
            if self.next_offset >= self.file_size {
                return Ok(None);
            }
            let chunk = self.conn.fundable_chunk(self.chunk_size);
            let len = (self.file_size - self.next_offset).min(u64::from(chunk)) as u32;
            let (reads, bytes) = (self.in_flight.len(), self.in_flight_bytes);
            match self.window().decide(Instant::now(), reads, bytes, len) {
                Dispatch::Now => {}
                Dispatch::At(at) => return Ok(Some(at)),
                Dispatch::AfterHead => return Ok(None),
            }
            let offset = self.next_offset;
            let guard = send_read(&self.conn, &self.tree, self.file_id, offset, len).await?;
            let now = Instant::now();
            self.in_flight.push_back(InFlightRead {
                offset,
                len,
                guard: Some(guard),
                dispatched_at: now,
            });
            self.next_offset += u64::from(len);
            self.in_flight_bytes += u64::from(len);
            self.peak_in_flight_bytes = self.peak_in_flight_bytes.max(self.in_flight_bytes);
            self.window().on_dispatch(now, len);
        }
    }

    /// Take the head READ's response, which came off the wire at `arrived_at`,
    /// off the queue. Synchronous on purpose: from here to handing the chunk
    /// out, nothing may be interrupted.
    fn take_head(&mut self, frame: Frame, arrived_at: Instant) -> Result<Option<Vec<u8>>> {
        let head = self
            .in_flight
            .pop_front()
            .expect("only called with the head's response");
        self.in_flight_bytes -= u64::from(head.len);

        if frame.header.status == NtStatus::END_OF_FILE {
            // The file shrank since the CREATE. Nothing past here exists.
            self.stop_at_eof();
            return Ok(None);
        }
        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Read,
            });
        }
        let mut data = ReadResponse::unpack(&mut ReadCursor::new(&frame.body))?.data;
        if data.is_empty() {
            self.stop_at_eof();
            return Ok(None);
        }
        data.truncate(head.len as usize);
        let got = data.len() as u32;
        if got < head.len {
            // Short read: the rest is the next chunk due, ahead of anything
            // already requested. `send_reads` sends it.
            let rest = head.len - got;
            self.in_flight.push_front(InFlightRead {
                offset: head.offset + u64::from(got),
                len: rest,
                guard: None,
                dispatched_at: head.dispatched_at,
            });
            self.in_flight_bytes += u64::from(rest);
        }
        self.bytes_received += u64::from(got);
        let outstanding = self.outstanding();
        self.window().on_delivery(
            Instant::now(),
            head.dispatched_at,
            arrived_at,
            got,
            outstanding,
        );
        if let Some(window) = &self.window {
            self.conn.note_read(window);
        }
        Ok(Some(data))
    }

    /// What's still out, READ by READ: which answers have come off the wire
    /// and which haven't. Looks at every guard without claiming its answer,
    /// which waits there until its chunk is taken.
    ///
    /// The head alone can't say: chunks are delivered in file order, so a
    /// READ Samba answered early looks unanswered until the ones ahead of it
    /// are taken, and correcting the window against that held READs back on a
    /// reordering link until it idled.
    pub(super) fn outstanding(&mut self) -> Outstanding {
        let mut outstanding = Outstanding {
            bytes: self.in_flight_bytes,
            unanswered: 0,
            last_arrival: None,
        };
        for read in &mut self.in_flight {
            // The rest of a short read not yet re-requested isn't out at all.
            let Some(guard) = read.guard.as_mut() else {
                continue;
            };
            if !guard.has_landed() {
                outstanding.unanswered += u64::from(read.len);
            } else if let Some(at) = guard.arrived_at() {
                outstanding.last_arrival = outstanding.last_arrival.max(Some(at));
            }
        }
        outstanding
    }

    fn window(&mut self) -> &mut Window {
        let (read_ahead, chunk) = (self.read_ahead, self.chunk_size);
        let conn = &self.conn;
        self.window
            .get_or_insert_with(|| Window::new(read_ahead, chunk, conn.read_link_hint()))
    }

    /// Nothing past this point exists: drop what's still requested (the
    /// responses are discarded as they land) and ask for nothing more.
    fn stop_at_eof(&mut self) {
        self.abandon_in_flight();
        self.next_offset = self.file_size;
    }

    /// Stop on an error: no more chunks, and no CLOSE here (the connection
    /// may be the thing that failed). Dropping the download sends one, if
    /// the connection still can.
    fn fail(&mut self, e: Error) -> Error {
        self.abandon_in_flight();
        self.done = true;
        e
    }

    fn abandon_in_flight(&mut self) {
        self.in_flight.clear();
        self.in_flight_bytes = 0;
    }

    /// Consume the download and collect all data with a progress callback.
    ///
    /// Return `ControlFlow::Break(())` from the callback to cancel the download.
    /// Cancellation returns `Error::Cancelled`.
    pub async fn collect_with_progress<F>(mut self, mut on_progress: F) -> Result<Vec<u8>>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        let mut data = Vec::with_capacity(self.file_size as usize);

        while let Some(result) = self.next_chunk().await {
            let chunk = result?;
            data.extend_from_slice(&chunk);

            if let ControlFlow::Break(()) = on_progress(self.progress()) {
                // Best-effort close before returning.
                let _ = self.close().await;
                return Err(Error::Cancelled);
            }
        }

        Ok(data)
    }

    /// Consume the download and collect all data into a `Vec<u8>`.
    pub async fn collect(mut self) -> Result<Vec<u8>> {
        let mut data = Vec::with_capacity(self.file_size as usize);

        while let Some(result) = self.next_chunk().await {
            let chunk = result?;
            data.extend_from_slice(&chunk);
        }

        Ok(data)
    }

    /// Close the file handle and wait for the answer.
    async fn close(&mut self) -> Result<()> {
        self.start_close().await?;
        self.finish_close().await
    }

    /// Put the CLOSE on the wire, without waiting for its answer. Only ever
    /// sends one: `close_started` is set first, so neither a second call nor
    /// the drop of a download whose call was dropped mid-send sends another
    /// (the handle then stays open until the session ends).
    async fn start_close(&mut self) -> Result<()> {
        if self.done || self.close_started {
            return Ok(());
        }
        self.done = true;
        self.close_started = true;
        // Any READs still out were sent before the CLOSE, so the server
        // answers them first; their responses are discarded once the guards
        // drop here.
        self.abandon_in_flight();
        match self.tree.dispatch_close(&self.conn, self.file_id).await {
            Ok(guard) => self.closing = Some(guard),
            // The server let go of the handle with its session: closed.
            Err(Error::HandleInvalidated { .. }) => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Collect the CLOSE's answer, if one is out. Cancel-safe: the guard stays
    /// in `closing` until the answer is in.
    async fn finish_close(&mut self) -> Result<()> {
        if let Some(e) = self.close_failed.take() {
            return Err(e);
        }
        let Some(guard) = self.closing.as_mut() else {
            return Ok(());
        };
        let answer = self
            .conn
            .await_response_in_place(guard, Command::Close)
            .await;
        self.closing = None;
        close_outcome(&answer?)
    }
}

/// Send one READ and return its guard, once it's on the wire.
async fn send_read(
    conn: &Connection,
    tree: &Tree,
    file_id: FileId,
    offset: u64,
    len: u32,
) -> Result<WaiterGuard> {
    let req = ReadRequest {
        padding: 0x50,
        flags: 0,
        length: len,
        offset,
        file_id,
        minimum_count: 0,
        channel: SMB2_CHANNEL_NONE,
        remaining_bytes: 0,
        read_channel_info: vec![],
    };
    conn.dispatch_with_credits(
        Command::Read,
        &req,
        Some(tree.tree_id),
        CreditCharge(credits::charge_for_payload(u64::from(len))),
    )
    .await
}

impl Drop for FileDownload<'_> {
    fn drop(&mut self) {
        if !self.close_started {
            trace!(
                "stream: FileDownload dropped at {}/{} bytes, closing its handle",
                self.bytes_received,
                self.file_size
            );
            self.tree
                .close_dropped_handle(&self.conn, self.file_id, "FileDownload");
        }
    }
}

/// A random-access reader over one open file handle.
///
/// Where [`FileDownload`] streams a file front-to-back (one chunk in memory,
/// forward only), `FileReader` holds the handle open and serves any number of
/// *positioned* reads at arbitrary offsets — the SMB analog of `pread`. It's the
/// right primitive for a consumer that parses a file's structure by jumping
/// around it (a zip's end-of-central-directory near the tail, then the central
/// directory before it, then member data mid-file), all over a SINGLE
/// open → N reads → close handle lifecycle rather than one open per read.
///
/// Owns a cheap `Arc::clone` of [`Connection`] and an `Arc<Tree>`, so it's
/// `'static` — move it across tasks or hand it to `tokio::spawn`. Because
/// [`read_at`](FileReader::read_at) takes `&self` and holds no shared cursor,
/// concurrent positioned reads over one reader are independent and pipeline over
/// the single SMB session (the receiver task multiplexes responses by
/// `MessageId`).
///
/// Call [`close`](FileReader::close) when done to release the server handle
/// and hear how that went. A reader dropped without it closes the handle as it
/// drops, without the answer (see [`FileWriter`]'s "Dropping it" section).
///
/// # Example
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// let reader = client.open_file_reader(share, "archive.zip").await?;
/// let size = reader.size();
/// // Read the 22-byte end-of-central-directory record at the tail, then jump
/// // to the central directory it points at — two positioned reads, one handle.
/// let eocd = reader.read_at(size.saturating_sub(22), 22).await?;
/// let dir = reader.read_at(0x1000, 512).await?;
/// reader.close().await?;
/// # let _ = (eocd, dir);
/// # Ok(())
/// # }
/// ```
pub struct FileReader {
    tree: Arc<Tree>,
    conn: Connection,
    file_id: FileId,
    info: FileInfo,
    max_read: u32,
    resolved_path: Option<String>,
    closed: bool,
}

/// Open a file for reading and return a random-access [`FileReader`] that owns
/// its `Connection` and `Arc<Tree>`.
///
/// Use this when you hold a cloned `Connection` and want to serve many
/// positioned reads over one open handle without holding any external lock.
/// The returned reader is `'static` — it doesn't borrow from anything.
///
/// [`SmbClient::open_file_reader`](crate::SmbClient::open_file_reader) and
/// [`Tree::open_file_reader`] are thin convenience wrappers; reach for them when
/// you already hold an `&SmbClient` or `&Arc<Tree>` and don't need the explicit
/// connection clone.
pub async fn open_file_reader(tree: Arc<Tree>, conn: Connection, path: &str) -> Result<FileReader> {
    trace!("stream: open_file_reader path={path:?}");

    let mut conn = tree.pinned_conn(&conn).pinned_to_session();
    let opened = tree
        .open_and_name(
            &mut conn,
            path,
            &tree.read_open_request(path),
            OpenFor::Reading,
        )
        .await?;
    let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);

    let mut reader = FileReader::new(
        tree,
        conn,
        opened.created.file_id,
        file_info_from_create(&opened.created),
        max_read,
    );
    reader.resolved_path = opened.name;
    Ok(reader)
}

impl FileReader {
    /// Wrap an already-opened read handle. Most callers want
    /// [`open_file_reader`], [`Tree::open_file_reader`], or
    /// [`SmbClient::open_file_reader`](crate::SmbClient::open_file_reader),
    /// which issue the CREATE for you.
    pub(crate) fn new(
        tree: Arc<Tree>,
        conn: Connection,
        file_id: FileId,
        info: FileInfo,
        max_read: u32,
    ) -> Self {
        let conn = tree.pinned_conn(&conn).pinned_to_session();
        Self {
            tree,
            conn,
            file_id,
            info,
            max_read,
            resolved_path: None,
            closed: false,
        }
    }

    /// Total file size in bytes, as seen when the handle was opened.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.info.size
    }

    /// The file's metadata as the server reported it when it opened this
    /// handle: size, whether it's a directory, and its creation,
    /// modification, and access times. From the open's own CREATE response,
    /// so it costs no round trip, and it doesn't change as the file does.
    #[must_use]
    pub fn info(&self) -> &FileInfo {
        &self.info
    }

    /// The path the server opened, as it stores it: relative to the share,
    /// `/`-separated, in on-disk casing, with 8.3 aliases replaced by their
    /// long names (see [`Tree::resolve`]). Asked in the same round trip as
    /// the open, so it names exactly the file this handle reads.
    ///
    /// `None` when the server can't name it (SMB 2.x or 3.0.2, or Windows
    /// before 10 / Server v1803); the open itself is unaffected.
    #[must_use]
    pub fn resolved_path(&self) -> Option<&str> {
        self.resolved_path.as_deref()
    }

    /// Read up to `len` bytes starting at `offset`.
    ///
    /// Positioned like `pread`: `offset` is absolute in the file and no cursor
    /// advances, so calls are independent and may run concurrently. Returns
    /// fewer than `len` bytes only at end of file — a read wholly at or past the
    /// end yields an empty `Vec`, and a read that straddles the end is clamped
    /// to what exists. A range larger than the server's `MaxReadSize` is split
    /// into that many wire-level READs and reassembled, so the caller passes the
    /// range it wants without minding the negotiated cap.
    pub async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        // A read at or past EOF yields no bytes; a read overrunning EOF is
        // clamped so we never ask the server for bytes that don't exist (which
        // would come back as STATUS_END_OF_FILE and complicate the loop).
        let file_size = self.info.size;
        if len == 0 || offset >= file_size {
            return Ok(Vec::new());
        }
        let to_read = len.min(file_size - offset);
        let end = offset + to_read;
        let mut out = Vec::with_capacity(to_read as usize);
        let mut pos = offset;

        while pos < end {
            let chunk_len =
                (end - pos).min(u64::from(self.conn.fundable_chunk(self.max_read))) as u32;
            let req = ReadRequest {
                padding: 0x50,
                flags: 0,
                length: chunk_len,
                offset: pos,
                file_id: self.file_id,
                minimum_count: 0,
                channel: SMB2_CHANNEL_NONE,
                remaining_bytes: 0,
                read_channel_info: vec![],
            };

            let credit_charge = credits::charge_for_payload(chunk_len as u64);
            let frame = self
                .conn
                .execute_with_credits(
                    Command::Read,
                    &req,
                    Some(self.tree.tree_id),
                    crate::types::CreditCharge(credit_charge),
                )
                .await?;

            // Even after clamping to the size seen at open, a racing truncation
            // (or a zero-length file) can surface EOF; treat it as a short read.
            if frame.header.status == NtStatus::END_OF_FILE {
                break;
            }
            if frame.header.status != NtStatus::SUCCESS {
                return Err(Error::Protocol {
                    status: frame.header.status,
                    command: Command::Read,
                });
            }

            let mut cursor = ReadCursor::new(&frame.body);
            let resp = ReadResponse::unpack(&mut cursor)?;
            if resp.data.is_empty() {
                break;
            }
            pos += resp.data.len() as u64;
            out.extend_from_slice(&resp.data);
        }

        Ok(out)
    }

    /// Close the file handle, releasing it on the server.
    ///
    /// Consumes `self` so read-after-close is a compile error. Dropping the
    /// reader closes the handle too; this one waits for the server's answer
    /// and returns it.
    ///
    /// A reader whose session has ended (a reconnect) sends nothing and
    /// returns `Ok`: the server let go of the handle with the session.
    pub async fn close(mut self) -> Result<()> {
        self.closed = true;
        match self.tree.close_handle(&mut self.conn, self.file_id).await {
            Err(Error::HandleInvalidated { .. }) => Ok(()),
            result => result,
        }
    }
}

impl Drop for FileReader {
    fn drop(&mut self) {
        if !self.closed {
            self.tree
                .close_dropped_handle(&self.conn, self.file_id, "FileReader");
        }
    }
}

/// An in-progress file upload that writes data in chunks with progress.
///
/// Each call to [`write_next_chunk`](FileUpload::write_next_chunk) hands one
/// WRITE to the connection and returns `true` while there is more data to
/// send. WRITEs are pipelined with the adaptive write-behind window (see
/// [`WriteBehind`]), so a call waits while the window is full, and
/// [`bytes_written`](FileUpload::bytes_written) counts what the server has
/// confirmed so far. The call that sends the last chunk waits for every
/// answer, flushes, closes the handle, and returns `false`.
///
/// The connection is borrowed mutably for the lifetime of the upload,
/// preventing accidental interleaving of SMB messages.
///
/// # Cancellation
///
/// To cancel an upload, stop calling `write_next_chunk` and drop the
/// `FileUpload`: that closes the file handle without a flush (see
/// [`FileWriter`]'s "Dropping it" section). The server keeps what it wrote
/// so far, so delete the partial file if you don't want it.
///
/// # Example
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// let data = std::fs::read("large_video.mp4")?;
/// let mut upload = client.upload(&share, "remote_video.mp4", &data).await?;
/// println!("Uploading {} bytes...", upload.total_bytes());
///
/// while upload.write_next_chunk().await? {
///     println!("{:.1}%", upload.progress().percent());
/// }
/// // File is flushed and closed automatically after the last chunk.
/// # Ok(())
/// # }
/// ```
pub struct FileUpload<'a> {
    /// Borrowed from the caller, or owned when a DFS link led elsewhere.
    tree: Cow<'a, Tree>,
    /// Pinned to the session the file was opened on, so nothing this upload
    /// sends can reach the next one (see `Connection::pinned_to_session`).
    conn: Connection,
    /// The caller's connection stays borrowed for the upload's life.
    _caller: PhantomData<&'a mut Connection>,
    file_id: FileId,
    data: &'a [u8],
    total_bytes: u64,
    /// Bytes handed to the pipe so far.
    sent: usize,
    /// `None` for an upload the constructor already finished in one frame.
    pipe: Option<WritePipe>,
    done: bool,
}

impl<'a> FileUpload<'a> {
    /// Create a streaming upload for a large file (data larger than one chunk).
    ///
    /// The caller then drives the upload with
    /// [`write_next_chunk`](FileUpload::write_next_chunk). `conn` is the clone
    /// the file was opened on, pinned before the CREATE.
    pub(crate) fn new(
        tree: Cow<'a, Tree>,
        conn: Connection,
        file_id: FileId,
        data: &'a [u8],
        max_write: u32,
    ) -> Self {
        let conn = tree.pinned_conn(&conn).pinned_to_session();
        let pipe = WritePipe::new(conn.clone(), tree.tree_id, file_id, max_write);
        Self {
            tree,
            conn,
            _caller: PhantomData,
            file_id,
            data,
            total_bytes: data.len() as u64,
            sent: 0,
            pipe: Some(pipe),
            done: false,
        }
    }

    /// Create a "done" upload for small files that were already written
    /// via compound in the constructor.
    pub(crate) fn new_done(tree: Cow<'a, Tree>, conn: Connection, total_bytes: u64) -> Self {
        Self {
            tree,
            conn,
            _caller: PhantomData,
            file_id: FileId::SENTINEL,
            data: &[],
            total_bytes,
            sent: 0,
            pipe: None,
            done: true,
        }
    }

    /// Total data size in bytes.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Bytes the server has confirmed so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.pipe
            .as_ref()
            .map_or(self.total_bytes, WritePipe::confirmed)
    }

    /// Current transfer progress.
    #[must_use]
    pub fn progress(&self) -> Progress {
        Progress {
            bytes_transferred: self.bytes_written(),
            total_bytes: Some(self.total_bytes),
        }
    }

    /// Write the next chunk of data to the server.
    ///
    /// Returns `Ok(true)` while there is more data to write, and `Ok(false)`
    /// when the upload is complete. The call that sends the last chunk also
    /// waits for every WRITE's answer, flushes, and closes the file handle.
    ///
    /// For small files that were written via compound in the constructor,
    /// this immediately returns `Ok(false)`.
    pub async fn write_next_chunk(&mut self) -> Result<bool> {
        if self.done {
            return Ok(false);
        }
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(false);
        };

        if self.sent < self.data.len() {
            let len = (self.data.len() - self.sent).min(pipe.next_len() as usize);
            let chunk = self.data[self.sent..self.sent + len].to_vec();
            if let Err(e) = pipe.send(chunk).await {
                return Err(self.failed(e).await);
            }
            self.sent += len;
            if self.sent < self.data.len() {
                return Ok(true);
            }
        }

        // Everything is on the wire: wait for the answers, then flush and close.
        if let Err(e) = pipe.drain().await {
            return Err(self.failed(e).await);
        }
        self.flush_and_close().await?;
        Ok(false)
    }

    /// The upload can't go on: close the handle without a flush
    /// (best-effort) and hand the error back. Only when the server refused a
    /// WRITE, answered one short, or sent an answer that failed its signature
    /// check (so nobody knows what was written); any other error may be the
    /// connection.
    async fn failed(&mut self, e: Error) -> Error {
        self.done = true;
        if matches!(
            e,
            Error::Protocol {
                command: Command::Write,
                ..
            } | Error::InvalidData { .. }
                | Error::SignatureInvalid { .. }
        ) {
            let _ = self.tree.close_handle(&mut self.conn, self.file_id).await;
        }
        e
    }

    /// Flush and close the file handle. Closes only once: `done` is set
    /// before the CLOSE, so a dropped upload never sends a second. A failed
    /// FLUSH leaves the handle open, and dropping the upload closes it.
    async fn flush_and_close(&mut self) -> Result<()> {
        if self.done {
            return Ok(());
        }
        // Flush to ensure data is persisted.
        self.tree.flush_handle(&mut self.conn, self.file_id).await?;
        self.done = true;
        self.tree.close_handle(&mut self.conn, self.file_id).await
    }
}

impl Drop for FileUpload<'_> {
    fn drop(&mut self) {
        if !self.done {
            trace!(
                "stream: FileUpload dropped at {}/{} bytes, closing its handle unflushed",
                self.bytes_written(),
                self.total_bytes
            );
            self.tree
                .close_dropped_handle(&self.conn, self.file_id, "FileUpload");
        }
    }
}

/// A push-based, pipelined streaming file writer.
///
/// The consumer pushes data at its own pace with
/// [`write_chunk`](FileWriter::write_chunk); the writer splits it into WRITEs
/// of [`chunk_size`](FileWriter::chunk_size) bytes (512 KiB by default, see
/// [`UPLOAD_CHUNK_SIZE`]) and keeps as many on the wire as its
/// [`WriteBehind`] policy allows. The default sizes that to the link: full
/// speed on a fast one, and about one chunk queued ahead of everything else
/// on the connection on a slow one. `write_chunk` waits while the window is
/// full, which is the backpressure.
///
/// Call [`finish`](FileWriter::finish) when done to flush, close the handle,
/// and get the total confirmed byte count, or [`abort`](FileWriter::abort) to
/// give up on the file.
///
/// # Dropping it
///
/// A writer dropped without `finish` or `abort` (or whose `finish` failed
/// before its CLOSE) closes its handle as it drops, without a flush, the way
/// `abort` does. The CLOSE is queued before the drop returns, behind the
/// WRITEs the writer already sent and ahead of anything sent on the
/// connection after, so deleting the partial file right after dropping its
/// writer finds it closed. Two exceptions: with no credit on hand the CLOSE
/// goes out a moment later with no such order, and outside any async runtime
/// in that same case it can't be sent, and the handle stays open until the
/// session ends. A handle whose session already ended sends nothing.
///
/// The same holds for every type that owns an open handle:
/// [`FileReader`], [`FileUpload`], [`FileDownload`],
/// [`DirectoryReader`](crate::DirectoryReader), and [`Watcher`](crate::Watcher).
/// A handle's CLOSE is only ever sent once: once `finish`, `abort`, or
/// `close` has started one, dropping the handle sends nothing more.
///
/// Owns its `Connection` and `Arc<Tree>` (both cheap `Arc::clone`s), so the
/// writer is `'static` and N concurrent writers pipeline over one SMB session
/// without any external locking; the receiver task multiplexes their
/// responses by `MessageId`.
///
/// # Example
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// let mut writer = client.create_file_writer(share, "output.bin").await?;
/// writer.write_chunk(b"first part").await?;
/// writer.write_chunk(b"second part").await?;
/// let total = writer.finish().await?;
/// println!("Wrote {total} bytes");
/// # Ok(())
/// # }
/// ```
pub struct FileWriter {
    tree: Arc<Tree>,
    /// For the CREATE-side lifecycle (FLUSH, CLOSE); the pipe holds a clone.
    conn: Connection,
    file_id: FileId,
    pipe: WritePipe,
    /// Pushed bytes not yet handed to the pipe. They leave this buffer only
    /// once their WRITE is on the wire, so a dropped `write_chunk` loses none.
    pending_data: Vec<u8>,
    /// Read position within `pending_data`.
    pending_offset: usize,
    /// Whether the writer has been finalized (handle closed).
    done: bool,
    /// What the server called the file at open, if it said.
    resolved_path: Option<String>,
    /// Which file the handle writes, if the server said in full.
    identity: Option<FileIdentity>,
    /// How far the file is known to be on disk: set only by a FLUSH that
    /// succeeded after every WRITE before it was confirmed.
    durable_offset: Option<u64>,
    /// For a resumed writer, the file's length at open when it was longer
    /// than the checkpoint: `finish` cuts off what's still past its last byte.
    trim_past: Option<u64>,
}

/// How far an upload is known to be on the server's disk, and which file it
/// went into: what an interrupted upload resumes from.
///
/// [`FileWriter::flush_checkpoint`] makes one. Persist both fields beside
/// whatever tracks the source (the offset says how far into it to restart),
/// and hand them back to
/// [`SmbClient::resume_file_writer`](crate::SmbClient::resume_file_writer)
/// after a crash or a reconnect. A checkpoint is plain data and safe to keep
/// for as long as you like: the resume proves the file is still the one it
/// names before writing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct WriteCheckpoint {
    /// Every byte before this offset reached the file and was flushed (SMB2
    /// FLUSH, the server's fsync). An absolute position in the file, so a
    /// writer opened at an offset counts what was there before it too.
    pub offset: u64,
    /// Which file the bytes went into. `None` when the server wouldn't say
    /// (it refused the identity queries, or answered `0` for either half),
    /// and then the checkpoint can't be resumed: nothing could prove a
    /// reopened file is the same one.
    pub identity: Option<FileIdentity>,
}

impl WriteCheckpoint {
    /// A checkpoint read back from wherever the caller persisted one.
    #[must_use]
    pub fn new(offset: u64, identity: Option<FileIdentity>) -> Self {
        Self { offset, identity }
    }
}

/// Open (or create) a file for writing and return a streaming [`FileWriter`]
/// that owns its `Connection` and `Arc<Tree>`.
///
/// Use this when you hold a cloned `Connection` and want to drive a
/// streaming write without holding any external lock for the upload's
/// duration. The returned writer is `'static` — drop it, move it across
/// tasks, hand it to `tokio::spawn`, it doesn't borrow from anything.
///
/// Multiple `FileWriter`s built from clones of the same `Connection`
/// pipeline their WRITEs over a single SMB session.
///
/// `SmbClient::create_file_writer` and `Tree::create_file_writer` are
/// thin convenience wrappers around this; reach for them when you already
/// hold an `&SmbClient` or `&Arc<Tree>` and don't need the explicit
/// connection clone.
pub async fn open_file_writer(tree: Arc<Tree>, conn: Connection, path: &str) -> Result<FileWriter> {
    trace!("stream: open_file_writer path={path:?}");

    let (writer, _) = open_writer(tree, conn, path, CreateDisposition::FileOverwriteIf).await?;
    Ok(writer)
}

/// Exclusive-create sibling of [`open_file_writer`]. Opens the CREATE with
/// `FileCreate` disposition: if the file already exists the open fails with
/// [`crate::ErrorKind::AlreadyExists`] instead of
/// truncating it.
///
/// `Tree::create_file_writer_exclusive` is the convenience wrapper most
/// callers want.
pub async fn open_file_writer_exclusive(
    tree: Arc<Tree>,
    conn: Connection,
    path: &str,
) -> Result<FileWriter> {
    trace!("stream: open_file_writer_exclusive path={path:?}");

    let (writer, _) = open_writer(tree, conn, path, CreateDisposition::FileCreate).await?;
    Ok(writer)
}

/// Open a file for writing at an arbitrary starting offset, returning a
/// [`FileWriter`] whose first byte lands at `offset`.
///
/// Unlike [`open_file_writer`] (which truncates), this opens with `FileOpenIf`
/// disposition and does *not* truncate: existing content is preserved, and the
/// writer's chunks are written starting at `offset` (overwriting or extending
/// from there). This is the positioned-write analog of
/// [`FileReader`]'s positioned reads — the natural shape for appending after a
/// server-side-copied prefix, or patching a known region of an existing file.
///
/// It trusts that the file is what you think it is. ❌ Don't resume an
/// interrupted upload with it: a file replaced, truncated, or deleted
/// meanwhile becomes a corrupt one (or a fresh one with a hole). Use
/// [`open_file_writer_resume`] with the [`WriteCheckpoint`] the upload left,
/// which proves the file first.
///
/// `bytes_written()` counts bytes this writer confirmed, not the absolute file
/// position. `SmbClient::create_file_writer_at` and `Tree::create_file_writer_at`
/// are thin convenience wrappers.
///
/// # Example
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// // Append after a 4 KiB prefix already present in the file.
/// let mut writer = client.create_file_writer_at(share, "patched.bin", 4096).await?;
/// writer.write_chunk(b"appended tail").await?;
/// writer.finish().await?;
/// # Ok(())
/// # }
/// ```
pub async fn open_file_writer_at(
    tree: Arc<Tree>,
    conn: Connection,
    path: &str,
    offset: u64,
) -> Result<FileWriter> {
    trace!("stream: open_file_writer_at path={path:?} offset={offset}");

    let (mut writer, _) = open_writer(tree, conn, path, CreateDisposition::FileOpenIf).await?;
    writer.pipe.set_offset(offset);
    Ok(writer)
}

/// Carry on an interrupted upload from a [`WriteCheckpoint`], once the server
/// has proven the file is still the one the checkpoint names and still holds
/// everything it vouches for.
///
/// One round trip, before any byte is written: a CREATE that only opens
/// (`FileOpen`, never creating), with the file's identity asked in the same
/// compound and its length read from the CREATE's answer. Every check that
/// fails closes the handle it opened and returns
/// [`Error::ResumeRefused`], whose [`ResumeRefusal`] says which:
///
/// - **`Missing`**: nothing is at `path`. A fresh file would start with a hole
///   where the checkpointed bytes were.
/// - **`Replaced`**: the file at `path` isn't the one the checkpoint names.
/// - **`IdentityUnavailable`**: the checkpoint has no identity, or the server
///   won't identify the file now. Nothing could prove it's the same one, so
///   the resume refuses rather than guess. A checkpoint without an identity is
///   refused before anything is sent.
/// - **`Truncated`**: the file is shorter than the checkpoint, so bytes it
///   vouches for are gone.
///
/// Any other error (the network, access denied, a sharing violation) comes
/// back as itself, so [`Error::is_retryable`](crate::Error::is_retryable)
/// tells "try the resume again later" from "write the file from the start".
///
/// A file LONGER than the checkpoint is expected: WRITEs that landed after it
/// and before the crash. The writer starts at the checkpoint and overwrites
/// them, and [`finish`](FileWriter::finish) cuts off whatever is still past
/// its last byte, so a finished resume holds exactly the checkpointed prefix
/// plus what this writer wrote, the same file a fresh upload of the same
/// source would have produced. [`abort`](FileWriter::abort) leaves the file as
/// it is.
///
/// `bytes_written()` counts what this writer wrote, from the checkpoint on.
/// [`SmbClient::resume_file_writer`](crate::SmbClient::resume_file_writer)
/// and [`Tree::resume_file_writer`] are the convenience wrappers.
///
/// # Example
///
/// ```no_run
/// # async fn example(
/// #     client: &mut smb2::SmbClient,
/// #     share: &smb2::Tree,
/// #     source: &[u8],
/// #     saved: smb2::WriteCheckpoint,
/// # ) -> Result<(), smb2::Error> {
/// let mut writer = match client.resume_file_writer(share, "backup.img", &saved).await {
///     Ok(writer) => writer,
///     // Not the file we were writing any more: start over.
///     Err(e) if e.kind() == smb2::ErrorKind::ResumeRefused => {
///         client.create_file_writer(share, "backup.img").await?
///     }
///     Err(e) => return Err(e),
/// };
/// let from = writer.durable_offset().unwrap_or(0) as usize;
/// writer.write_chunk(&source[from..]).await?;
/// writer.finish().await?;
/// # Ok(())
/// # }
/// ```
pub async fn open_file_writer_resume(
    tree: Arc<Tree>,
    conn: Connection,
    path: &str,
    checkpoint: &WriteCheckpoint,
) -> Result<FileWriter> {
    trace!(
        "stream: open_file_writer_resume path={path:?} offset={}",
        checkpoint.offset
    );
    let refused = |reason| {
        debug!(
            "stream: not resuming path={path:?} offset={}: {reason}",
            checkpoint.offset
        );
        Error::ResumeRefused {
            path: path.to_string(),
            offset: checkpoint.offset,
            reason,
        }
    };
    let Some(expected) = checkpoint.identity else {
        return Err(refused(ResumeRefusal::IdentityUnavailable));
    };

    let (mut writer, length) =
        match open_writer(tree, conn, path, CreateDisposition::FileOpen).await {
            Ok(opened) => opened,
            Err(Error::Protocol {
                status: NtStatus::OBJECT_NAME_NOT_FOUND | NtStatus::OBJECT_PATH_NOT_FOUND,
                command: Command::Create,
            }) => return Err(refused(ResumeRefusal::Missing)),
            Err(e) => return Err(e),
        };

    // Identity before length: a different file's length says nothing.
    let refusal = match writer.identity {
        None => Some(ResumeRefusal::IdentityUnavailable),
        Some(found) if found != expected => Some(ResumeRefusal::Replaced),
        Some(_) if length < checkpoint.offset => Some(ResumeRefusal::Truncated { length }),
        Some(_) => None,
    };
    if let Some(reason) = refusal {
        // Nothing was written, so nothing is in flight: this is just the CLOSE.
        writer.abort().await?;
        return Err(refused(reason));
    }

    writer.pipe.set_offset(checkpoint.offset);
    writer.durable_offset = Some(checkpoint.offset);
    writer.trim_past = (length > checkpoint.offset).then_some(length);
    Ok(writer)
}

/// The CREATE behind every [`FileWriter`], with the server's name for the
/// file and which file it is asked in the same round trip. Also hands back
/// the file's length at open.
async fn open_writer(
    tree: Arc<Tree>,
    conn: Connection,
    path: &str,
    disposition: CreateDisposition,
) -> Result<(FileWriter, u64)> {
    let mut conn = tree.pinned_conn(&conn).pinned_to_session();
    let opened = tree
        .open_and_name(
            &mut conn,
            path,
            &tree.write_open_request(path, disposition),
            OpenFor::Writing,
        )
        .await?;
    let max_write = conn.params().map(|p| p.max_write_size).unwrap_or(65536);

    let mut writer = FileWriter::new(tree, conn, opened.created.file_id, max_write);
    writer.resolved_path = opened.name;
    writer.identity = opened.identity;
    Ok((writer, opened.created.end_of_file))
}

impl FileWriter {
    /// Create a new push-based streaming writer.
    ///
    /// Most callers want [`open_file_writer`], [`Tree::create_file_writer`],
    /// or [`SmbClient::create_file_writer`](crate::SmbClient::create_file_writer)
    /// which issue the CREATE for you. Use this constructor when you've
    /// already opened the file via [`Tree::open_file_for_write`] (for
    /// example, to reuse a handle across multiple writers).
    pub(crate) fn new(
        tree: Arc<Tree>,
        conn: Connection,
        file_id: FileId,
        max_write_size: u32,
    ) -> Self {
        let conn = tree.pinned_conn(&conn).pinned_to_session();
        let pipe = WritePipe::new(conn.clone(), tree.tree_id, file_id, max_write_size);
        Self {
            tree,
            conn,
            file_id,
            pipe,
            pending_data: Vec::new(),
            pending_offset: 0,
            done: false,
            resolved_path: None,
            identity: None,
            durable_offset: None,
            trim_past: None,
        }
    }

    /// Which file this writer writes, asked in the same round trip as the
    /// open. `None` when the server wouldn't say in full, and then a
    /// [`WriteCheckpoint`] from this writer can't be resumed.
    #[must_use]
    pub fn identity(&self) -> Option<FileIdentity> {
        self.identity
    }

    /// How far the file is known to be on disk: the offset of the last
    /// [`flush_checkpoint`](Self::flush_checkpoint) that succeeded, or the
    /// checkpoint a resumed writer started from. `None` before either.
    #[must_use]
    pub fn durable_offset(&self) -> Option<u64> {
        self.durable_offset
    }

    /// The path the server opened, as it stores it: relative to the share,
    /// `/`-separated, in on-disk casing, with 8.3 aliases replaced by their
    /// long names (see [`Tree::resolve`]). Asked in the same round trip as
    /// the open, so it names exactly the file this handle writes.
    ///
    /// `None` when the server can't name it (SMB 2.x or 3.0.2, or Windows
    /// before 10 / Server v1803); the open itself is unaffected.
    #[must_use]
    pub fn resolved_path(&self) -> Option<&str> {
        self.resolved_path.as_deref()
    }

    /// Change how many WRITEs this writer keeps on the wire. Meant to be
    /// called before the first [`write_chunk`](Self::write_chunk).
    #[must_use]
    pub fn with_write_behind(mut self, write_behind: WriteBehind) -> Self {
        self.pipe.set_policy(write_behind);
        self
    }

    /// Change how many bytes each WRITE carries: at least one, at most the
    /// server's `MaxWriteSize` (a bigger value is clamped to it). Meant to be
    /// called before the first [`write_chunk`](Self::write_chunk).
    #[must_use]
    pub fn with_chunk_size(mut self, chunk_size: u32) -> Self {
        self.pipe.set_chunk_size(chunk_size);
        self
    }

    /// The write-behind policy in use.
    #[must_use]
    pub fn write_behind(&self) -> WriteBehind {
        self.pipe.policy()
    }

    /// Bytes each WRITE carries, before the credit window lowers it on a
    /// server with a small one.
    #[must_use]
    pub fn chunk_size(&self) -> u32 {
        self.pipe.chunk_size()
    }

    /// The most bytes this writer has had handed to the connection but not
    /// yet confirmed by the server at once.
    #[must_use]
    pub fn peak_in_flight_bytes(&self) -> u64 {
        self.pipe.peak_in_flight_bytes()
    }

    /// Push a data chunk to the writer.
    ///
    /// The data is split into WRITEs of [`chunk_size`](Self::chunk_size)
    /// bytes, and a shorter tail goes out as it is. Returns once every byte
    /// is on the wire; waiting for the window to have room is the
    /// backpressure.
    ///
    /// Dropping this future loses nothing: bytes that hadn't gone out stay
    /// buffered and go out with the next `write_chunk` or
    /// [`finish`](Self::finish). A WRITE caught mid-send may go out a second
    /// time, with the same bytes at the same offset.
    ///
    /// Empty chunks are no-ops.
    pub async fn write_chunk(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        if self.pending_offset > 0 {
            self.pending_data.drain(..self.pending_offset);
            self.pending_offset = 0;
        }
        self.pending_data.extend_from_slice(data);
        self.send_pending().await
    }

    /// Finish the writer: send what's buffered, wait for every WRITE's
    /// answer, flush, and close.
    ///
    /// Returns the total number of confirmed bytes written. Consumes `self`
    /// to prevent write-after-close at compile time.
    pub async fn finish(mut self) -> Result<u64> {
        self.flush_all(true).await?;
        // Before the CLOSE, so the drop at the end never sends a second one.
        self.done = true;
        self.tree.close_handle(&mut self.conn, self.file_id).await?;
        Ok(self.pipe.confirmed())
    }

    /// Abort the writer: discard unsent data, drain in-flight responses, and
    /// close the handle without flushing.
    ///
    /// Use this when you want to cancel a write partway through — for example
    /// on user-triggered cancellation or an error path where the partial upload
    /// will be deleted anyway. `abort()` skips the server-side fsync that
    /// [`finish`](FileWriter::finish) does, so it returns as soon as the
    /// in-flight window is drained.
    ///
    /// What it does:
    /// - Discards any buffered (unsent) data. Wire WRITEs already in flight
    ///   still have responses on the way; those are drained to keep credits
    ///   and message-IDs in sync with the server. Errors on those responses
    ///   are swallowed — at this point we don't care.
    /// - Skips the FLUSH that [`finish`](FileWriter::finish) sends before
    ///   CLOSE, so the server does not fsync. This is the main reason to
    ///   prefer `abort()` over `finish()` on cancellation.
    /// - Best-effort CLOSE of the file handle. If the CLOSE fails, the error
    ///   is logged at debug and swallowed.
    ///
    /// Contrast with [`finish`](FileWriter::finish): `finish()` sends every
    /// pending byte, flushes, and propagates errors from the flush/close
    /// paths. `abort()` sends nothing more, never flushes, and returns `Ok`
    /// regardless of what the server said on the way out.
    ///
    /// Returns the number of confirmed bytes written at the moment of abort
    /// (from WRITE responses seen so far). Consumes `self` to prevent
    /// write-after-abort at compile time. The `Result` wrapper mirrors
    /// [`finish`](FileWriter::finish)'s signature and leaves room for future
    /// failure modes; today `abort()` never returns `Err`.
    ///
    /// The caller is responsible for deleting the partial remote file if they
    /// don't want it to linger — the server now has a zero-to-N byte file
    /// depending on how many WRITEs completed before the abort.
    ///
    /// # Future extension
    ///
    /// A `close_and_delete()` variant that sends `SET_INFO
    /// FileDispositionInformation(DeletePending=true)` before CLOSE would
    /// combine the two round-trips the caller does today. Out of scope here.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::ops::ControlFlow;
    /// # async fn example(
    /// #     client: &mut smb2::SmbClient,
    /// #     share: &smb2::Tree,
    /// #     cancel: impl Fn() -> bool,
    /// # ) -> Result<(), smb2::Error> {
    /// let mut writer = client.create_file_writer(share, "output.bin").await?;
    /// for chunk in [b"first".as_slice(), b"second", b"third"] {
    ///     if cancel() {
    ///         let written = writer.abort().await?;
    ///         println!("Aborted after {written} bytes confirmed");
    ///         // Caller: delete the partial remote file here if desired.
    ///         return Ok(());
    ///     }
    ///     writer.write_chunk(chunk).await?;
    /// }
    /// writer.finish().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn abort(mut self) -> Result<u64> {
        // 1. Discard anything we have not yet put on the wire. Unsent data
        //    means nothing to the server and carries no credits.
        self.pending_data.clear();
        self.pending_offset = 0;

        // 2. Wait out the WRITEs already on the wire, whatever they say:
        //    their answers are on the way either way.
        self.pipe.abandon().await;

        // 3. Skip flush_handle() — that's the whole point of abort().

        // 4. Best-effort CLOSE. If it fails, log and move on. A writer whose
        //    session ended has nothing to close: the server let go of the
        //    handle with the session. `done` first, so the drop at the end
        //    never sends a second CLOSE.
        self.done = true;
        match self.tree.close_handle(&mut self.conn, self.file_id).await {
            Ok(()) | Err(Error::HandleInvalidated { .. }) => {}
            Err(e) => debug!(
                "stream: FileWriter::abort() best-effort CLOSE failed, handle may leak \
                 server-side until session teardown: {}",
                e
            ),
        }
        Ok(self.pipe.confirmed())
    }

    /// Make everything pushed so far durable, without closing the file, and
    /// say how far that is.
    ///
    /// Sends what's buffered, waits for every WRITE's answer, then sends an
    /// SMB2 FLUSH (the server's fsync) on the handle. The returned
    /// [`WriteCheckpoint`] is what an interrupted upload resumes from: persist
    /// it, and after a crash or a reconnect hand it to
    /// [`SmbClient::resume_file_writer`](crate::SmbClient::resume_file_writer).
    /// The writer stays open, so keep pushing chunks afterwards.
    ///
    /// How often is a trade: each checkpoint costs a round trip plus the
    /// server's flush, and empties the write-behind window, so every few tens
    /// of MiB is plenty.
    ///
    /// The guarantees:
    ///
    /// - The offset moves only once the FLUSH succeeded, after every WRITE
    ///   before it was confirmed in full. A WRITE the server answered short
    ///   is an error ([`ErrorKind::InvalidData`](crate::ErrorKind::InvalidData)),
    ///   never a confirmed byte, and closes the handle.
    /// - Dropping this future loses nothing. Bytes not yet sent stay buffered
    ///   and WRITEs already out stay tracked; the next call (or
    ///   [`finish`](Self::finish)) picks up where this one stopped, and
    ///   [`durable_offset`](Self::durable_offset) keeps the last checkpoint
    ///   that completed.
    /// - On an error the offset stays where it was, and so does the handle
    ///   unless the server refused a WRITE.
    pub async fn flush_checkpoint(&mut self) -> Result<WriteCheckpoint> {
        self.flush_all(false).await?;
        let offset = self.pipe.offset();
        self.durable_offset = Some(offset);
        trace!("stream: FileWriter checkpoint at {offset}");
        Ok(WriteCheckpoint::new(offset, self.identity))
    }

    /// Confirmed bytes written (from server WRITE responses).
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.pipe.confirmed()
    }

    /// Set the file's timestamps on this writer's own handle, leaving the
    /// ones `times` doesn't name as they are. Sends what's buffered and waits
    /// for every WRITE's answer first, then one SET_INFO.
    ///
    /// This is how a copy keeps its source's dates: a server stamps a file
    /// with the time of its last write when the handle that wrote it closes,
    /// unless that handle had its times set explicitly. So set them here,
    /// after the last [`write_chunk`](Self::write_chunk) and before
    /// [`finish`](Self::finish), and the close leaves them alone. Setting them
    /// by path ([`Tree::set_times`]) while this writer is open loses to its
    /// close.
    ///
    /// Writes after this call keep the stamp too, on Windows (MS-FSA's
    /// `UserSetModificationTime`) and Samba (its sticky write time), but
    /// setting the times last is the order every server agrees on.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree, modified: std::time::SystemTime) -> Result<(), smb2::Error> {
    /// use smb2::FileTimes;
    ///
    /// let mut writer = client.create_file_writer(share, "copy.bin").await?;
    /// writer.write_chunk(b"contents").await?;
    /// writer.set_times(FileTimes::new().set_modified(modified)).await?;
    /// writer.finish().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_times(&mut self, times: FileTimes) -> Result<()> {
        self.send_pending().await?;
        if let Err(e) = self.pipe.drain().await {
            return Err(self.failed(e).await);
        }
        self.tree
            .set_handle_times(&mut self.conn, self.file_id, times)
            .await
    }

    /// Current transfer progress.
    ///
    /// `total_bytes` is always `None` because push-based writers don't
    /// know the total size upfront.
    #[must_use]
    pub fn progress(&self) -> Progress {
        Progress {
            bytes_transferred: self.pipe.confirmed(),
            total_bytes: None,
        }
    }

    /// Hand everything buffered to the pipe, one WRITE at a time. A byte
    /// leaves the buffer only once its WRITE is on the wire.
    async fn send_pending(&mut self) -> Result<()> {
        while self.pending_offset < self.pending_data.len() {
            let left = self.pending_data.len() - self.pending_offset;
            let len = left.min(self.pipe.next_len() as usize);
            let chunk = self.pending_data[self.pending_offset..self.pending_offset + len].to_vec();
            if let Err(e) = self.pipe.send(chunk).await {
                return Err(self.failed(e).await);
            }
            self.pending_offset += len;
        }
        self.pending_data.clear();
        self.pending_offset = 0;
        Ok(())
    }

    /// Send what's buffered, wait for every WRITE, then FLUSH: everything
    /// [`finish`](Self::finish) and [`flush_checkpoint`](Self::flush_checkpoint)
    /// share. Each step is cancel-safe, so the whole is too.
    ///
    /// `last` is `finish`: a resumed writer then cuts off what the crash left
    /// past its last byte, before the FLUSH so the flush covers the new
    /// length. ❌ Never at a checkpoint: the caller may still be on its way to
    /// overwriting that tail.
    async fn flush_all(&mut self, last: bool) -> Result<()> {
        self.send_pending().await?;
        if let Err(e) = self.pipe.drain().await {
            return Err(self.failed(e).await);
        }
        let end = self.pipe.offset();
        if last && self.trim_past.is_some_and(|old| old > end) {
            self.tree
                .set_end_of_file(&mut self.conn, self.file_id, end)
                .await?;
            self.trim_past = None;
        }
        // Standalone, so Windows' rule that a compounded op going async must
        // end the chain (AGENTS.md pitfall 33) can't apply.
        self.tree.flush_handle(&mut self.conn, self.file_id).await
    }

    /// The server refused a WRITE, answered one short, or sent an answer that
    /// failed its signature check: the file is done for, so close the handle
    /// (best-effort) and hand the error back. The
    /// pipe has already waited out the WRITEs behind it. Any other error
    /// leaves the handle alone: the connection may be the thing that failed.
    async fn failed(&mut self, e: Error) -> Error {
        if matches!(
            e,
            Error::Protocol {
                command: Command::Write,
                ..
            } | Error::InvalidData { .. }
                | Error::SignatureInvalid { .. }
        ) {
            self.done = true;
            let _ = self.tree.close_handle(&mut self.conn, self.file_id).await;
        }
        e
    }
}

impl Drop for FileWriter {
    fn drop(&mut self) {
        if !self.done {
            trace!(
                "stream: FileWriter dropped after {} confirmed bytes, closing its handle \
                 unflushed",
                self.pipe.confirmed()
            );
            self.tree
                .close_dropped_handle(&self.conn, self.file_id, "FileWriter");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_helpers::{
        build_close_error_response, build_close_response, build_compound_response_frame,
        build_create_error_response, build_create_response, build_create_response_dated,
        build_flush_response, build_query_info_error_response, build_query_info_response,
        build_read_error_response, build_read_response, build_set_info_response,
        build_write_error_response, build_write_response, build_write_response_granting,
        build_writer_open_response, file_all_information, file_name_information, setup_connection,
        CREATE_TIMES,
    };
    use crate::transport::MockTransport;
    use crate::types::status::NtStatus;
    use crate::types::{FileId, TreeId};
    use std::sync::Arc;

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

    /// The compound a writer's open gets back: CREATE, then identity and
    /// class 48 queries the server refused (so `resolved_path` is `None`).
    fn writer_opened(file_id: FileId) -> Vec<u8> {
        build_writer_open_response(file_id, 0, None, None)
    }

    /// The compound a reader's open gets back: CREATE, then class 18 and
    /// class 48, both refused.
    fn reader_opened(file_id: FileId, end_of_file: u64) -> Vec<u8> {
        build_compound_response_frame(&[
            build_create_response(file_id, end_of_file),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
        ])
    }

    // ── FileWriter tests ───────────────────────────────────────────────

    /// The commands of every frame sent, in order.
    fn sent_commands(mock: &MockTransport) -> Vec<Command> {
        use crate::msg::header::Header;
        mock.sent_messages()
            .iter()
            .map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).unwrap().command)
            .collect()
    }

    /// A writer's times go out on its own handle after every WRITE has been
    /// answered and before the FLUSH and CLOSE, so the close can't restamp
    /// the file.
    #[tokio::test]
    async fn file_writer_set_times_stamps_its_own_handle_after_the_writes_land() {
        use crate::msg::header::Header;
        use crate::msg::set_info::SetInfoRequest;
        use crate::pack::FileTime;
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_set_info_response());
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let mut writer = test_tree()
            .create_file_writer(conn, "copy.bin")
            .await
            .unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        let modified = FileTime(133_444_736_000_000_000);
        writer
            .set_times(FileTimes::new().set_modified(modified))
            .await
            .unwrap();
        assert_eq!(writer.bytes_written(), 100, "the WRITE was answered first");
        assert_eq!(writer.finish().await.unwrap(), 100);

        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Write,
                Command::SetInfo,
                Command::Flush,
                Command::Close
            ]
        );
        let set = mock.sent_message(2).unwrap();
        let set = SetInfoRequest::unpack(&mut ReadCursor::new(&set[Header::SIZE..])).unwrap();
        assert_eq!(set.file_id, file_id);
        assert_eq!(&set.buffer[16..24], &modified.0.to_le_bytes());
        assert_eq!(&set.buffer[0..8], &[0; 8], "creation time left alone");
    }

    /// A 1 MiB WRITE charges 16 credits, which a window the server stopped
    /// growing at 8 can never fund. The writer sends what half that window
    /// funds instead (four credits, 256 KiB), so a small window slows the
    /// upload down rather than failing it.
    #[tokio::test]
    async fn file_writer_sizes_its_writes_to_a_small_credit_window() {
        let mock = Arc::new(MockTransport::new());
        let quarter = 256 * 1024;
        // A server capped at 8 grants back what each WRITE spent, no more.
        for _ in 0..4 {
            mock.queue_response(build_write_response_granting(quarter, 4));
        }
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        conn.set_credits(8);
        conn.set_credit_ceiling(8);
        let mut writer = FileWriter::new(test_tree(), conn, test_file_id(), 1024 * 1024);
        writer.write_chunk(&vec![0u8; 1024 * 1024]).await.unwrap();
        assert_eq!(writer.finish().await.unwrap(), 1024 * 1024);

        // Four WRITEs, then FLUSH and CLOSE.
        assert_eq!(mock.sent_count(), 6);
    }

    #[tokio::test]
    async fn file_writer_at_offset_writes_from_given_position() {
        use crate::msg::header::Header;
        use crate::msg::write::WriteRequest;
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // CREATE (FileOpenIf) + WRITE(50) + FLUSH + CLOSE.
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(50));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree
            .create_file_writer_at(conn, "patched.bin", 4096)
            .await
            .unwrap();
        writer.write_chunk(&[7u8; 50]).await.unwrap();
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 50);

        // The WRITE must have gone out at offset 4096, not 0.
        let sent = mock.sent_messages();
        let write_frame = sent
            .iter()
            .find_map(|bytes| {
                let mut c = ReadCursor::new(&bytes[Header::SIZE..]);
                WriteRequest::unpack(&mut c)
                    .ok()
                    .filter(|w| !w.data.is_empty())
            })
            .expect("a WRITE with data was sent");
        assert_eq!(write_frame.offset, 4096);
    }

    #[tokio::test]
    async fn file_writer_single_chunk() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // Queue: CREATE + WRITE(100) + FLUSH + CLOSE
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        assert_eq!(writer.bytes_written(), 0); // Not yet drained
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 100);
    }

    #[tokio::test]
    async fn file_writer_multiple_chunks() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[1u8; 100]).await.unwrap();
        writer.write_chunk(&[2u8; 100]).await.unwrap();
        writer.write_chunk(&[3u8; 100]).await.unwrap();
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 300);
    }

    #[tokio::test]
    async fn file_writer_empty_finish() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // Queue: CREATE + FLUSH + CLOSE (no WRITE)
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let writer = tree.create_file_writer(conn, "empty.bin").await.unwrap();
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 0);

        // Verify: CREATE + FLUSH + CLOSE = 3 sent messages.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn file_writer_empty_chunk_noop() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // Queue: CREATE + WRITE(50) + FLUSH + CLOSE
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(50));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[]).await.unwrap(); // No-op
        writer.write_chunk(&[0u8; 50]).await.unwrap();
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 50);

        // CREATE + WRITE + FLUSH + CLOSE = 4 (no extra WRITE for empty chunk).
        assert_eq!(mock.sent_count(), 4);
    }

    #[tokio::test]
    async fn file_writer_chunk_splitting() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // max_write_size = 65536, send 200KB = 3 x 65536 + 1 x 8192.
        // 200 * 1024 = 204800. 204800 / 65536 = 3.125 -> 4 wire writes.
        let chunk_size = 200 * 1024;
        let wire_1 = 65536u32;
        let wire_2 = 65536u32;
        let wire_3 = 65536u32;
        let wire_4 = (chunk_size - 3 * 65536) as u32; // 8192

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(wire_1));
        mock.queue_response(build_write_response(wire_2));
        mock.queue_response(build_write_response(wire_3));
        mock.queue_response(build_write_response(wire_4));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "big.bin").await.unwrap();
        writer.write_chunk(&vec![0u8; chunk_size]).await.unwrap();
        let total = writer.finish().await.unwrap();
        assert_eq!(total, (wire_1 + wire_2 + wire_3 + wire_4) as u64);

        // CREATE + 4 WRITEs + FLUSH + CLOSE = 7
        assert_eq!(mock.sent_count(), 7);
    }

    #[tokio::test]
    async fn file_writer_progress_none_total() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        let progress = writer.progress();
        assert!(progress.total_bytes.is_none());
        assert_eq!(progress.bytes_transferred, 0);
        writer.finish().await.unwrap();
    }

    #[tokio::test]
    async fn file_writer_bytes_written_tracks_confirmed() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_write_response(200));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        // A fixed window sends without waiting for answers, so nothing is
        // confirmed until something waits for one.
        let mut writer = tree
            .create_file_writer(conn, "out.bin")
            .await
            .unwrap()
            .with_write_behind(WriteBehind::Fixed(32));

        // After pushing but before finish, bytes_written reflects only drained responses.
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        assert_eq!(writer.bytes_written(), 0); // Not yet drained

        writer.write_chunk(&[0u8; 200]).await.unwrap();
        assert_eq!(writer.bytes_written(), 0); // Still not drained

        // finish() drains all.
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 300);
    }

    #[tokio::test]
    async fn file_writer_backpressure() {
        const WINDOW: usize = 32;
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));

        // Queue WINDOW + 1 write responses.
        for _ in 0..WINDOW + 1 {
            mock.queue_response(build_write_response(64));
        }
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree
            .create_file_writer(conn, "out.bin")
            .await
            .unwrap()
            .with_write_behind(WriteBehind::Fixed(WINDOW));

        // Fill the window.
        for _ in 0..WINDOW {
            writer.write_chunk(&[0u8; 64]).await.unwrap();
        }

        // This write must drain one response before sending (backpressure).
        writer.write_chunk(&[0u8; 64]).await.unwrap();

        // At least one response was drained by backpressure.
        assert!(writer.bytes_written() >= 64);

        let total = writer.finish().await.unwrap();
        assert_eq!(total, (WINDOW as u64 + 1) * 64);
    }

    #[tokio::test]
    async fn file_writer_server_error() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        // Return error for the WRITE.
        mock.queue_response(build_write_error_response(NtStatus::DISK_FULL));
        // CLOSE after error cleanup.
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        let result = writer.finish().await;
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(
            format!("{err:?}").contains("DISK_FULL"),
            "expected DISK_FULL, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn file_writer_finish_drains_all() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(50));
        mock.queue_response(build_write_response(75));
        mock.queue_response(build_write_response(25));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree
            .create_file_writer(conn, "out.bin")
            .await
            .unwrap()
            .with_write_behind(WriteBehind::Fixed(32));
        writer.write_chunk(&[0u8; 50]).await.unwrap();
        writer.write_chunk(&[0u8; 75]).await.unwrap();
        writer.write_chunk(&[0u8; 25]).await.unwrap();

        // None drained yet.
        assert_eq!(writer.bytes_written(), 0);

        // finish() must drain all 3.
        let total = writer.finish().await.unwrap();
        assert_eq!(total, 150);
    }

    // ── FileWriter::abort tests ────────────────────────────────────────

    #[tokio::test]
    async fn file_writer_abort_no_in_flight() {
        // abort() with nothing in flight: just CLOSE, no FLUSH, no extra reads.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        // Queue: CREATE + CLOSE (note: no FLUSH — abort skips fsync).
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        let total = writer.abort().await.unwrap();
        assert_eq!(total, 0);

        // Exactly 2 messages on the wire: CREATE, CLOSE.
        assert_eq!(mock.sent_count(), 2);
    }

    #[tokio::test]
    async fn file_writer_abort_drains_in_flight() {
        // abort() must consume in-flight WRITE responses to keep the
        // connection in sync, but skips FLUSH.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        // Three WRITEs on the wire, three responses queued.
        mock.queue_response(build_write_response(50));
        mock.queue_response(build_write_response(75));
        mock.queue_response(build_write_response(25));
        // No FLUSH response — abort must not send FLUSH.
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree
            .create_file_writer(conn, "out.bin")
            .await
            .unwrap()
            .with_write_behind(WriteBehind::Fixed(32));
        writer.write_chunk(&[0u8; 50]).await.unwrap();
        writer.write_chunk(&[0u8; 75]).await.unwrap();
        writer.write_chunk(&[0u8; 25]).await.unwrap();

        // Nothing drained yet — write_chunk doesn't drain unless the window fills.
        assert_eq!(writer.bytes_written(), 0);

        // abort() drains all three and returns the confirmed total.
        let total = writer.abort().await.unwrap();
        assert_eq!(total, 150);

        // Wire traffic: CREATE + 3 WRITEs + CLOSE = 5. No FLUSH.
        assert_eq!(mock.sent_count(), 5);
    }

    #[tokio::test]
    async fn file_writer_abort_swallows_write_errors() {
        // Mid-stream WRITE failure during abort's drain: swallowed, abort
        // still closes and returns Ok.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        // Second WRITE errors — abort must not bubble this up.
        mock.queue_response(build_write_error_response(NtStatus::DISK_FULL));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();

        // abort() should return Ok despite the DISK_FULL on the second WRITE.
        // total_written reflects only the successful WRITE (100).
        let total = writer.abort().await.unwrap();
        assert_eq!(total, 100);

        // Wire: CREATE + 2 WRITEs + CLOSE = 4.
        assert_eq!(mock.sent_count(), 4);
    }

    #[tokio::test]
    async fn file_writer_abort_discards_buffered_data() {
        // Bytes still buffered (a `write_chunk` dropped while it waited for
        // room) must not be sent by abort().
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();

        // Inject a buffer directly, as a dropped `write_chunk` leaves it.
        writer.pending_data = vec![0u8; 1000];
        writer.pending_offset = 0;

        let total = writer.abort().await.unwrap();
        assert_eq!(total, 0);

        // Only CREATE + CLOSE on the wire. No WRITE from the buffer.
        assert_eq!(mock.sent_count(), 2);
    }

    #[tokio::test]
    async fn file_writer_abort_close_error_is_swallowed() {
        // CLOSE failing at the end is logged but not surfaced — abort
        // is a best-effort fast exit.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        // CLOSE returns an error. abort() must still return Ok.
        mock.queue_response(build_close_error_response(NtStatus::FILE_CLOSED));

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();

        let result = writer.abort().await;
        assert!(
            result.is_ok(),
            "abort() should swallow CLOSE errors, got: {result:?}"
        );
        assert_eq!(result.unwrap(), 100);

        // CREATE + WRITE + CLOSE = 3.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn file_writer_abort_sets_done_so_drop_sends_no_second_close() {
        // abort() marks the writer done before its CLOSE, so the drop at the
        // end of abort's frame sends nothing more.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let writer = tree
            .create_file_writer(conn.clone(), "out.bin")
            .await
            .unwrap();
        assert!(writer.abort().await.is_ok());
        tokio::task::yield_now().await;
        assert_eq!(sent_commands(&mock), [Command::Create, Command::Close]);
    }

    // ── FileReader tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn file_reader_reports_the_dates_its_open_returned() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response_dated(test_file_id(), 10),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
        ]));
        let conn = setup_connection(&mock);

        let reader = test_tree().open_file_reader(conn, "a.bin").await.unwrap();
        let info = reader.info();
        let [created, accessed, modified, _] = CREATE_TIMES;
        assert_eq!(
            (
                info.size,
                info.is_directory,
                info.created,
                info.modified,
                info.accessed
            ),
            (10, false, created, modified, accessed)
        );
        assert_eq!(reader.size(), 10);
        assert_eq!(mock.sent_count(), 1, "the dates cost no round trip");
        drop(reader);
    }

    #[tokio::test]
    async fn file_reader_positioned_reads_one_open_one_close() {
        // The core no-leak contract: one CREATE, N positioned READs at
        // arbitrary offsets, one CLOSE — the handle is opened once and released
        // once no matter how many reads run.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(reader_opened(file_id, 1000));
        mock.queue_response(build_read_response(vec![0xAA; 4]));
        mock.queue_response(build_read_response(vec![0xBB; 8]));
        mock.queue_response(build_read_response(vec![0xCC; 2]));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let reader = tree.open_file_reader(conn, "archive.zip").await.unwrap();
        assert_eq!(reader.size(), 1000);

        // Three reads at different offsets, out of order — no shared cursor.
        assert_eq!(reader.read_at(500, 4).await.unwrap(), vec![0xAA; 4]);
        assert_eq!(reader.read_at(0, 8).await.unwrap(), vec![0xBB; 8]);
        assert_eq!(reader.read_at(998, 2).await.unwrap(), vec![0xCC; 2]);

        reader.close().await.unwrap();

        // CREATE + 3 READ + CLOSE = 5. Exactly one open, exactly one close.
        assert_eq!(mock.sent_count(), 5);
    }

    #[tokio::test]
    async fn file_reader_clamps_and_skips_reads_at_or_past_eof() {
        // A read wholly past EOF issues NO wire READ and returns empty; a read
        // straddling EOF is clamped to what exists. file_size = 10.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(reader_opened(file_id, 10));
        // Only ONE READ hits the wire: the clamped [8, 10) read of 2 bytes.
        mock.queue_response(build_read_response(vec![0xEE; 2]));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let reader = tree.open_file_reader(conn, "small.bin").await.unwrap();

        // Wholly past EOF → empty, no wire traffic.
        assert!(reader.read_at(10, 5).await.unwrap().is_empty());
        assert!(reader.read_at(99, 100).await.unwrap().is_empty());
        // Zero-length → empty, no wire traffic.
        assert!(reader.read_at(0, 0).await.unwrap().is_empty());
        // Straddling EOF: asked for 100 at offset 8, clamped to 2 bytes.
        assert_eq!(reader.read_at(8, 100).await.unwrap(), vec![0xEE; 2]);

        reader.close().await.unwrap();

        // CREATE + 1 READ (the clamped one) + CLOSE = 3. The past-EOF and
        // zero-length reads never touched the wire.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn file_reader_splits_range_larger_than_max_read() {
        // A single read_at spanning more than MaxReadSize (65536 in the test
        // params) is split into consecutive wire READs and reassembled.
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        let total = 65536usize * 2 + 100; // 3 wire reads: 65536, 65536, 100
        mock.queue_response(reader_opened(file_id, total as u64));
        mock.queue_response(build_read_response(vec![1u8; 65536]));
        mock.queue_response(build_read_response(vec![2u8; 65536]));
        mock.queue_response(build_read_response(vec![3u8; 100]));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let reader = tree.open_file_reader(conn, "big.bin").await.unwrap();
        let data = reader.read_at(0, total as u64).await.unwrap();
        assert_eq!(data.len(), total);
        assert_eq!(&data[..65536], &vec![1u8; 65536][..]);
        assert_eq!(&data[65536..65536 * 2], &vec![2u8; 65536][..]);
        assert_eq!(&data[65536 * 2..], &vec![3u8; 100][..]);

        reader.close().await.unwrap();

        // CREATE + 3 READ + CLOSE = 5 — still ONE open and ONE close for the
        // whole split range.
        assert_eq!(mock.sent_count(), 5);
    }

    #[tokio::test]
    async fn file_reader_surfaces_read_error() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(reader_opened(file_id, 100));
        mock.queue_response(build_read_error_response(NtStatus::ACCESS_DENIED));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();

        let reader = tree.open_file_reader(conn, "denied.bin").await.unwrap();
        let result = reader.read_at(0, 10).await;
        assert!(matches!(
            result,
            Err(Error::Protocol {
                status: NtStatus::ACCESS_DENIED,
                command: Command::Read,
            })
        ));
        // The caller still owns the handle and can close it cleanly.
        reader.close().await.unwrap();
    }

    #[tokio::test]
    async fn file_reader_dropped_without_close_closes_its_handle() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();

        mock.queue_response(reader_opened(file_id, 100));
        mock.queue_response(build_read_response(vec![0x11; 4]));
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();

        {
            let reader = tree
                .open_file_reader(conn.clone(), "leak.bin")
                .await
                .unwrap();
            assert_eq!(reader.read_at(0, 4).await.unwrap(), vec![0x11; 4]);
            // Dropped here without close().
        }
        tree.flush_handle(&mut conn, OTHER_FILE).await.unwrap();

        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Read,
                Command::Close,
                Command::Flush
            ]
        );
        assert_eq!(closed_file_id(&mock, 2), file_id);
    }

    // ── Close on drop ──────────────────────────────────────────────────

    /// A handle no test opened, for the request sent after a drop.
    const OTHER_FILE: FileId = FileId {
        persistent: 0x77,
        volatile: 0x88,
    };

    /// The `FileId` the CLOSE sent as frame `n` names.
    fn closed_file_id(mock: &MockTransport, n: usize) -> FileId {
        use crate::msg::header::Header;
        let bytes = mock.sent_message(n).unwrap();
        let body = &bytes[Header::SIZE..];
        // StructureSize (2), Flags (2), Reserved (4), then the FileId.
        FileId {
            persistent: u64::from_le_bytes(body[8..16].try_into().unwrap()),
            volatile: u64::from_le_bytes(body[16..24].try_into().unwrap()),
        }
    }

    /// The incident behind this: a copy failed, its driver dropped the
    /// in-flight writers, and its cleanup's deletes of their temp files hit
    /// `STATUS_SHARING_VIOLATION`, because a dropped writer sent no CLOSE and
    /// its handle stayed open for as long as the session lived. Now the drop
    /// queues the CLOSE before it returns, so it reaches the server ahead of
    /// anything the owner sends next, the delete included.
    #[tokio::test]
    async fn file_writer_dropped_mid_upload_closes_before_anything_sent_after() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut writer = tree
            .create_file_writer(conn.clone(), "out.bin")
            .await
            .unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        drop(writer);
        tree.flush_handle(&mut conn, OTHER_FILE).await.unwrap();

        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Write,
                Command::Close,
                Command::Flush
            ]
        );
        assert_eq!(closed_file_id(&mock, 2), file_id);
    }

    /// `finish` that failed at the FLUSH leaves the handle open, and the
    /// drop closes it.
    #[tokio::test]
    async fn file_writer_whose_finish_failed_closes_on_drop() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_write_response(100));
        mock.queue_response(crate::client::test_helpers::build_flush_error_response(
            NtStatus::DISK_FULL,
        ));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = test_tree();
        let mut writer = tree.create_file_writer(conn, "out.bin").await.unwrap();
        writer.write_chunk(&[0u8; 100]).await.unwrap();
        writer.finish().await.unwrap_err();

        wait_until(|| mock.sent_count() == 4).await;
        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Write,
                Command::Flush,
                Command::Close
            ]
        );
    }

    /// A handle closed on purpose gets exactly one CLOSE: a second one could
    /// close another open the server has since given the same id.
    #[tokio::test]
    async fn a_finished_or_closed_handle_sends_no_second_close_on_drop() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());
        mock.queue_response(reader_opened(file_id, 0));
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let writer = tree
            .create_file_writer(conn.clone(), "out.bin")
            .await
            .unwrap();
        writer.finish().await.unwrap();
        let reader = tree.open_file_reader(conn.clone(), "in.bin").await.unwrap();
        reader.close().await.unwrap();
        tree.flush_handle(&mut conn, OTHER_FILE).await.unwrap();

        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Flush,
                Command::Close,
                Command::Create,
                Command::Close,
                Command::Flush
            ]
        );
    }

    /// `Drop` can't count on an async runtime being there: a writer dropped
    /// on a plain thread still closes its handle, because the CLOSE is
    /// queued for the connection's own writer task, not spawned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn file_writer_dropped_outside_any_runtime_still_closes() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(writer_opened(file_id));
        mock.queue_response(build_close_response());

        // Kept, as a consumer keeps its client: a writer holding the last
        // clone takes the whole connection down with it instead, and the
        // server lets go of the handle with the session.
        let conn = setup_connection(&mock);
        let writer = test_tree()
            .create_file_writer(conn.clone(), "out.bin")
            .await
            .unwrap();
        std::thread::spawn(move || drop(writer)).join().unwrap();

        wait_until(|| mock.sent_count() == 2).await;
        assert_eq!(sent_commands(&mock), [Command::Create, Command::Close]);
        assert_eq!(closed_file_id(&mock, 1), file_id);
    }

    /// The same for an upload dropped partway: its doc always promised the
    /// handle would close.
    #[tokio::test]
    async fn file_upload_dropped_partway_closes_its_handle() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(build_write_response(65536));
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let data = vec![0u8; 3 * 65536];
        {
            let mut upload = FileUpload::new(
                std::borrow::Cow::Borrowed(&*tree),
                conn.clone(),
                file_id,
                &data,
                65536,
            );
            assert!(upload.write_next_chunk().await.unwrap());
        }
        tree.flush_handle(&mut conn, OTHER_FILE).await.unwrap();

        assert_eq!(
            sent_commands(&mock),
            [Command::Write, Command::Close, Command::Flush]
        );
        assert_eq!(closed_file_id(&mock, 1), file_id);
    }

    /// And for a download abandoned partway.
    #[tokio::test]
    async fn file_download_dropped_partway_closes_its_handle() {
        let mock = Arc::new(MockTransport::new());
        let file_id = test_file_id();
        mock.queue_response(build_create_response(file_id, 3 * 65536));
        mock.queue_response(build_read_response(vec![0x11; 65536]));
        // The READ after it is already out when the download is dropped.
        mock.queue_response(build_read_response(vec![0x22; 65536]));
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        {
            let mut opener = conn.clone();
            let mut download = tree
                .download(&mut opener, "big.bin")
                .await
                .unwrap()
                .with_read_ahead(crate::ReadAhead::SEQUENTIAL);
            download.next_chunk().await.unwrap().unwrap();
        }
        tree.flush_handle(&mut conn, OTHER_FILE).await.unwrap();

        assert_eq!(
            sent_commands(&mock),
            [
                Command::Create,
                Command::Read,
                Command::Read,
                Command::Close,
                Command::Flush
            ]
        );
        assert_eq!(closed_file_id(&mock, 3), file_id);
    }

    /// Poll `f` until it holds, panicking rather than hanging if it never
    /// does. For the parts of a close-on-drop that happen in the background.
    async fn wait_until(mut f: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !f() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition never became true within 5s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    // ── Progress tests ─────────────────────────────────────────────────

    #[test]
    fn progress_calculations() {
        let cases = [
            (50, Some(100), 50.0, 0.5),
            (100, Some(100), 100.0, 1.0),
            (25, Some(100), 25.0, 0.25),
            (0, Some(0), 100.0, 1.0), // Empty file
            (50, None, 0.0, 0.0),     // Unknown total
        ];
        for (transferred, total, expected_pct, expected_frac) in cases {
            let p = Progress {
                bytes_transferred: transferred,
                total_bytes: total,
            };
            assert_eq!(
                p.percent(),
                expected_pct,
                "percent failed for {transferred}/{total:?}"
            );
            assert_eq!(
                p.fraction(),
                expected_frac,
                "fraction failed for {transferred}/{total:?}"
            );
        }

        // Large numbers.
        let large = Progress {
            bytes_transferred: u64::MAX / 2,
            total_bytes: Some(u64::MAX),
        };
        let frac = large.fraction();
        assert!(frac > 0.49 && frac < 0.51);
    }

    // ── What the server opened ─────────────────────────────────────────

    /// The query classes a compound request carried, in order.
    fn sent_query_classes(mock: &MockTransport, n: usize) -> Vec<u8> {
        use crate::msg::header::Header;
        use crate::msg::query_info::QueryInfoRequest;
        let sent = mock.sent_message(n).unwrap();
        let mut classes = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&sent[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            if header.command == Command::QueryInfo {
                classes.push(
                    QueryInfoRequest::unpack(&mut cursor)
                        .unwrap()
                        .file_info_class,
                );
            }
            if header.next_command == 0 {
                return classes;
            }
            offset += header.next_command as usize;
        }
    }

    #[tokio::test]
    async fn a_reader_records_the_name_the_server_opened_in_the_open_round_trip() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(test_file_id(), 5),
            build_query_info_response(file_all_information("\\Docs\\Report.txt")),
            build_query_info_response(file_name_information("Docs\\Report.txt")),
        ]));
        let reader = test_tree()
            .open_file_reader(setup_connection(&mock), "DOCS/report.TXT")
            .await
            .unwrap();
        assert_eq!(reader.resolved_path(), Some("Docs/Report.txt"));
        assert_eq!(reader.size(), 5);
        assert_eq!(mock.sent_count(), 1, "the name costs no extra round trip");
        // Class 18 before 48, so a server cascading a refused 18 can't take
        // 48 down with it.
        assert_eq!(sent_query_classes(&mock, 0), vec![18, 48]);
    }

    #[tokio::test]
    async fn a_reader_falls_back_to_file_all_information() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(test_file_id(), 5),
            build_query_info_response(file_all_information("\\Docs\\Report.txt")),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
        ]));
        let reader = test_tree()
            .open_file_reader(setup_connection(&mock), "docs/report.txt")
            .await
            .unwrap();
        assert_eq!(reader.resolved_path(), Some("Docs/Report.txt"));
    }

    #[tokio::test]
    async fn a_reader_opens_fine_on_a_server_that_names_nothing() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(reader_opened(test_file_id(), 5));
        let reader = test_tree()
            .open_file_reader(setup_connection(&mock), "docs/report.txt")
            .await
            .unwrap();
        assert_eq!(reader.resolved_path(), None);
        assert_eq!(reader.size(), 5);
    }

    #[tokio::test]
    async fn a_reader_that_cannot_open_reports_the_create_error() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_query_info_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_query_info_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ]));
        let err = test_tree()
            .open_file_reader(setup_connection(&mock), "missing.txt")
            .await
            .err()
            .expect("a missing file must not open");
        assert_eq!(err.kind(), crate::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn a_writer_records_the_name_the_server_opened() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_writer_open_response(
            test_file_id(),
            0,
            None,
            Some("Out\\new\u{F025}.bin"),
        ));
        mock.queue_response(build_close_response());
        let writer = test_tree()
            .create_file_writer(setup_connection(&mock), "out/new?.bin")
            .await
            .unwrap();
        assert_eq!(writer.resolved_path(), Some("Out/new?.bin"));
        // A write handle has no FILE_READ_ATTRIBUTES, so class 18 isn't asked;
        // the identity classes need no access.
        assert_eq!(sent_query_classes(&mock, 0), vec![6, 1, 48]);
        writer.abort().await.unwrap();
    }

    #[tokio::test]
    async fn every_writer_open_names_what_it_opened() {
        for disposition in ["exclusive", "at"] {
            let mock = Arc::new(MockTransport::new());
            mock.queue_response(build_writer_open_response(
                test_file_id(),
                0,
                None,
                Some("Out.bin"),
            ));
            mock.queue_response(build_close_response());
            let conn = setup_connection(&mock);
            let writer = match disposition {
                "exclusive" => {
                    test_tree()
                        .create_file_writer_exclusive(conn, "out.bin")
                        .await
                }
                _ => test_tree().create_file_writer_at(conn, "out.bin", 10).await,
            }
            .unwrap();
            assert_eq!(writer.resolved_path(), Some("Out.bin"), "{disposition}");
            writer.abort().await.unwrap();
        }
    }
}
