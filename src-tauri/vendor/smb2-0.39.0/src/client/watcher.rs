//! Directory change notification via SMB2 CHANGE_NOTIFY.
//!
//! The [`Watcher`] type registers for change notifications on a directory
//! and returns [`FileNotifyEvent`] entries describing changes as they happen.
//! The server holds the request until a change occurs, making this a long-poll
//! operation.
//!
//! Because hours of silence is the healthy case here, nothing about the
//! request itself can say the server died -- so the wait is bounded by the
//! connection going silent instead, which only the ECHO keepalive makes
//! meaningful. That bound lives in `Connection::await_response`, and this
//! module reaches it by handing the pre-issued request's `WaiterGuard` there
//! rather than awaiting the guard directly. ❌ A bare `WaiterGuard::recv()` is
//! an unbounded `oneshot` await; using one here is what left a watcher on a
//! silent-but-open session waiting forever.

use log::{debug, trace};

use crate::client::connection::{Connection, LongPollOutcome, WaiterGuard};
use crate::client::tree::Tree;
use crate::client::Frame;
use crate::error::Result;
use crate::msg::change_notify::{
    ChangeNotifyRequest, ChangeNotifyResponse, FILE_NOTIFY_CHANGE_ATTRIBUTES,
    FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_EA,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_NOTIFY_CHANGE_STREAM_NAME, FILE_NOTIFY_CHANGE_STREAM_SIZE,
    FILE_NOTIFY_CHANGE_STREAM_WRITE, SMB2_WATCH_TREE,
};
use crate::pack::{ReadCursor, Unpack};
use crate::types::status::NtStatus;
use crate::types::{Command, FileId, MessageId};
use crate::Error;

/// Default completion filter: watch for most common changes.
const DEFAULT_COMPLETION_FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME
    | FILE_NOTIFY_CHANGE_DIR_NAME
    | FILE_NOTIFY_CHANGE_ATTRIBUTES
    | FILE_NOTIFY_CHANGE_SIZE
    | FILE_NOTIFY_CHANGE_LAST_WRITE
    | FILE_NOTIFY_CHANGE_CREATION;

/// What [`WatchOptions::metadata`] adds to the default filter.
const METADATA_COMPLETION_FILTER: u32 = FILE_NOTIFY_CHANGE_EA
    | FILE_NOTIFY_CHANGE_STREAM_NAME
    | FILE_NOTIFY_CHANGE_STREAM_SIZE
    | FILE_NOTIFY_CHANGE_STREAM_WRITE;

/// How [`Tree::watch_with`](crate::Tree::watch_with) watches a directory.
///
/// ```
/// let options = smb2::WatchOptions::new().recursive(true).metadata(true);
/// # let _ = options;
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WatchOptions {
    pub(crate) recursive: bool,
    pub(crate) metadata: bool,
}

impl WatchOptions {
    /// The defaults: this directory only, and the changes [`Tree::watch`](crate::Tree::watch)
    /// reports.
    pub fn new() -> Self {
        Self::default()
    }

    /// Watch the entire subtree.
    pub fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// Also hear about xattr and named-stream changes (`FILE_NOTIFY_CHANGE_EA`
    /// and the three `FILE_NOTIFY_CHANGE_STREAM_*` bits), so another client's
    /// Finder tag shows up. Off by default, since a server may then wake a watch
    /// for every change to any file's metadata.
    pub fn metadata(mut self, metadata: bool) -> Self {
        self.metadata = metadata;
        self
    }

    fn completion_filter(self) -> u32 {
        if self.metadata {
            DEFAULT_COMPLETION_FILTER | METADATA_COMPLETION_FILTER
        } else {
            DEFAULT_COMPLETION_FILTER
        }
    }
}

/// Default output buffer length for CHANGE_NOTIFY responses (64 KB).
const OUTPUT_BUFFER_LENGTH: u32 = 65536;

/// The type of change that occurred on a file or directory.
///
/// These correspond to the `Action` field in `FILE_NOTIFY_INFORMATION`
/// (MS-FSCC section 2.4.42).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileNotifyAction {
    /// A file was added to the directory.
    Added,
    /// A file was removed from the directory.
    Removed,
    /// A file was modified.
    Modified,
    /// A file was renamed (this is the old name).
    RenamedOldName,
    /// A file was renamed (this is the new name).
    RenamedNewName,
    /// A named stream was added to a file.
    StreamAdded,
    /// A named stream was removed from a file.
    StreamRemoved,
    /// A named stream's content changed.
    StreamModified,
}

impl FileNotifyAction {
    /// Parse an action value from the wire format.
    fn from_u32(value: u32) -> Result<Self> {
        match value {
            0x0000_0001 => Ok(FileNotifyAction::Added),
            0x0000_0002 => Ok(FileNotifyAction::Removed),
            0x0000_0003 => Ok(FileNotifyAction::Modified),
            0x0000_0004 => Ok(FileNotifyAction::RenamedOldName),
            0x0000_0005 => Ok(FileNotifyAction::RenamedNewName),
            0x0000_0006 => Ok(FileNotifyAction::StreamAdded),
            0x0000_0007 => Ok(FileNotifyAction::StreamRemoved),
            0x0000_0008 => Ok(FileNotifyAction::StreamModified),
            other => Err(Error::invalid_data(format!(
                "unknown FILE_NOTIFY_INFORMATION action: {other:#010X}"
            ))),
        }
    }
}

impl std::fmt::Display for FileNotifyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileNotifyAction::Added => write!(f, "added"),
            FileNotifyAction::Removed => write!(f, "removed"),
            FileNotifyAction::Modified => write!(f, "modified"),
            FileNotifyAction::StreamAdded => write!(f, "stream added"),
            FileNotifyAction::StreamRemoved => write!(f, "stream removed"),
            FileNotifyAction::StreamModified => write!(f, "stream modified"),
            FileNotifyAction::RenamedOldName => write!(f, "renamed (old name)"),
            FileNotifyAction::RenamedNewName => write!(f, "renamed (new name)"),
        }
    }
}

/// A single file change notification.
///
/// Represents one `FILE_NOTIFY_INFORMATION` entry from the server.
#[derive(Debug, Clone)]
pub struct FileNotifyEvent {
    /// What kind of change occurred.
    pub action: FileNotifyAction,
    /// The changed path, relative to the watched directory.
    ///
    /// A recursive watch reports something below the directory it was armed
    /// on, so this can carry separators. They are `/`, and each component has
    /// been mapped back out of the private-use area (see [`crate::name`]), so
    /// the value goes straight into any [`Tree`] method after being joined
    /// onto the watched directory's path.
    ///
    /// [`Tree`]: crate::Tree
    pub filename: String,
}

/// Watches a directory for changes via SMB2 CHANGE_NOTIFY.
///
/// The server holds the request until something changes, then responds
/// with one or more [`FileNotifyEvent`] entries. Each call to
/// [`next_events()`](Watcher::next_events) blocks until the server
/// reports a change.
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// let mut watcher = client.watch(&share, "_test/", true).await?;
/// loop {
///     let events = watcher.next_events().await?;
///     for event in &events {
///         println!("{}: {}", event.filename, event.action);
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
/// **Pipelining**: `Watcher` keeps one CHANGE_NOTIFY request pre-issued on
/// the wire at all times after the first call to
/// [`next_events`](Self::next_events). The wire never sits idle between
/// consecutive responses, so server-side events that arrive while the
/// consumer is processing the previous batch are still delivered to an
/// outstanding request — they don't fall in a response→re-arm gap where
/// strict servers (older Samba, NAS firmware) drop them silently.
///
/// The watcher owns a cloned [`Connection`] (cheap `Arc::clone`, all
/// clones multiplex over the same SMB session), so the caller doesn't
/// need a second `SmbClient` to perform other operations while watching.
pub struct Watcher {
    tree: Tree,
    conn: Connection,
    file_id: FileId,
    options: WatchOptions,
    /// The in-flight CHANGE_NOTIFY. Populated lazily on the first
    /// `next_events()` call and re-populated before awaiting each response,
    /// so there is always exactly one outstanding request on the wire from
    /// that point on.
    ///
    /// It is a `WaiterGuard`, not a bare receiver, so a watcher that stops
    /// being polled (a pane closes, the consumer drops it) deregisters its
    /// pre-issued request instead of leaving it in the connection's in-flight
    /// table forever. Before that, a long-lived watcher's abandoned
    /// CHANGE_NOTIFYs showed up as requests "unanswered" for hours.
    pending: Option<WaiterGuard>,
    /// Set by [`close`](Self::close) before its CLOSE, so the drop after it
    /// never sends a second.
    closed: bool,
}

impl Watcher {
    /// Create a new watcher (called by `Tree::watch`).
    pub(crate) fn new(
        tree: Tree,
        conn: Connection,
        file_id: FileId,
        options: WatchOptions,
    ) -> Self {
        let conn = tree.pinned_conn(&conn).pinned_to_session();
        Watcher {
            tree,
            conn,
            file_id,
            options,
            pending: None,
            closed: false,
        }
    }

    /// Wait for the next batch of change events.
    ///
    /// Dispatches a CHANGE_NOTIFY request (if one isn't already pre-issued
    /// from the previous call), then — before awaiting the response —
    /// dispatches the *next* CHANGE_NOTIFY. This keeps the wire
    /// continuously armed: from the moment the first call returns until
    /// the watcher is dropped, the server always has an outstanding
    /// request to deliver events into. Closes the response→re-arm loss
    /// window that strict servers (older Samba, NAS firmware) drop events
    /// through.
    ///
    /// The server holds each request until changes occur, so this call
    /// may block for a long time.
    ///
    /// Returns `Ok(events)` with one or more events when changes are detected.
    ///
    /// # Errors
    ///
    /// Returns `Error::Protocol` with `STATUS_NOTIFY_ENUM_DIR` if too many
    /// changes occurred and the server could not fit them in the response
    /// buffer. In this case, the caller should re-scan the directory and
    /// keep watching — by the time control returns, the pipelined-next
    /// request is already on the wire so no events arriving during the
    /// re-scan get lost.
    pub async fn next_events(&mut self) -> Result<Vec<FileNotifyEvent>> {
        let frame = loop {
            // Cold start: no request has been issued yet. Dispatch the first.
            if self.pending.is_none() {
                let rx = self.dispatch_next().await?;
                self.pending = Some(rx);
            }
            // Take the currently in-flight receiver, then immediately
            // pre-issue the next request before awaiting this one. The
            // `dispatch` call below `.await`s only the transport.send(), so
            // when it returns, the next CHANGE_NOTIFY is on the wire and the
            // server has somewhere to put new events even while we process
            // the response for the previous one.
            let in_flight = self.pending.take().expect("pending populated above");
            let next_rx = self.dispatch_next().await?;
            self.pending = Some(next_rx);

            // ❌ Not `in_flight.recv()`. A bare await on the guard walks past
            // every bound the connection has: CHANGE_NOTIFY is exempt from the
            // request deadline by design, so the connection is the only thing
            // that applies the two bounds it does have — connection-wide
            // silence, and the refresh cycle. Awaiting the guard directly left
            // a watcher on a silent-but-open session waiting forever, which is
            // the exact failure the long-poll bound exists to end.
            match self
                .conn
                .await_long_poll_refreshable(in_flight, Command::ChangeNotify)
                .await?
            {
                LongPollOutcome::Answered(frame) => break frame,
                LongPollOutcome::RefreshDue {
                    msg_id,
                    async_id,
                    generation,
                } => {
                    match self.refresh(msg_id, async_id, generation).await? {
                        // The sibling had an answer waiting after all.
                        Some(frame) => break frame,
                        None => continue,
                    }
                }
            }
        };

        if frame.header.status == NtStatus::NOTIFY_ENUM_DIR {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::ChangeNotify,
            });
        }

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::ChangeNotify,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = ChangeNotifyResponse::unpack(&mut cursor)?;

        let events = parse_notify_information(&resp.output_data)?;
        // An empty response is plumbing: a subscription handed over during a
        // refresh answers with nothing to report, and on an idle directory
        // that's most of them. Events are the signal, so only those get a
        // line a consumer sees at `debug`.
        if events.is_empty() {
            trace!("watcher: a subscription answered with no change events");
        } else {
            debug!("watcher: received {} change event(s)", events.len());
        }
        Ok(events)
    }

    /// Build a CHANGE_NOTIFY request and dispatch it on the cloned
    /// connection, returning the response receiver. `Connection::dispatch`
    /// awaits only up to and including `transport.send()`, so when this
    /// returns the request is on the wire — the caller can rely on the
    /// "outstanding on the wire" invariant for whatever comes next.
    async fn dispatch_next(&self) -> Result<WaiterGuard> {
        let flags = if self.options.recursive {
            SMB2_WATCH_TREE
        } else {
            0
        };
        let req = ChangeNotifyRequest {
            flags,
            output_buffer_length: OUTPUT_BUFFER_LENGTH,
            file_id: self.file_id,
            completion_filter: self.options.completion_filter(),
        };
        self.conn
            .dispatch(Command::ChangeNotify, &req, Some(self.tree.tree_id))
            .await
    }

    /// Replace both of the watcher's subscriptions with fresh ones.
    ///
    /// Reached when a request has been registered with the server longer than
    /// `Connection::set_long_poll_refresh` allows. Nothing is known to be
    /// wrong, and nothing can be: a subscription the server has forgotten and a
    /// directory nobody has touched are the same silence. This declines to bet
    /// on which, at the cost of three frames per cycle.
    ///
    /// **Both** get replaced, not just the one that aged out. The watcher
    /// pre-issues its successor in the same breath as the request it awaits, so
    /// the two are the same age and a server that dropped one has every reason
    /// to have dropped the other.
    ///
    /// **Order is the whole correctness argument.** The replacement goes out
    /// before either retirement, so the server is never without an outstanding
    /// request — leaving that gap open is exactly what the pipelined watcher
    /// exists to prevent, and a strict server drops the events that land in it.
    ///
    /// Returns any events the retiring sibling turned out to be holding: an
    /// answer that arrived in the handover is a real answer, and it also
    /// settles the only question worth asking here, which is that this
    /// subscription was alive.
    async fn refresh(
        &mut self,
        retired: MessageId,
        async_id: Option<u64>,
        generation: u64,
    ) -> Result<Option<Frame>> {
        debug!(
            "watcher: refreshing its subscription (msg_id={} retired)",
            retired.0
        );
        // 1. Arm the replacement first. Everything after this can fail without
        //    leaving the directory unwatched.
        let fresh = self.dispatch_next().await?;

        // 2. Retire the request that aged out. Best-effort: a server that has
        //    forgotten the subscription will ignore the cancel too, which is
        //    the case being healed rather than a new failure. ❌ Skipping it is
        //    not an option though — on a server that DID remember, every cycle
        //    would leave one more abandoned CHANGE_NOTIFY registered for the
        //    life of the watch.
        self.retire(retired, async_id, generation).await;

        // 3. And its equally-old sibling, salvaging an answer if it has one.
        let mut answered = None;
        if let Some(mut sibling) = self.pending.take() {
            let (msg_id, async_id) = (sibling.msg_id(), sibling.async_id());
            let generation = sibling.generation();
            answered = sibling.try_recv().transpose()?;
            drop(sibling);
            if answered.is_none() {
                self.retire(msg_id, async_id, generation).await;
            }
        }

        self.pending = Some(fresh);
        Ok(answered)
    }

    /// Tell the server to let go of a request we have stopped waiting for.
    ///
    /// Failure is logged and swallowed: the cancel is housekeeping, and a
    /// connection too broken to carry it will surface itself through the next
    /// real request with a far better error than this one could offer.
    async fn retire(&self, msg_id: MessageId, async_id: Option<u64>, generation: u64) {
        if let Err(e) = self.conn.send_cancel(msg_id, async_id, generation).await {
            debug!(
                "watcher: could not cancel the retired CHANGE_NOTIFY (msg_id={}): {e}",
                msg_id.0
            );
        }
    }

    /// Close the directory handle.
    ///
    /// Drops the pre-issued CHANGE_NOTIFY, which deregisters its waiter (the
    /// `Connection` receiver task discards the late response silently when it
    /// arrives — same contract `Connection::execute` already documents), then
    /// issues a CLOSE on the file handle and waits for its answer. Dropping
    /// the watcher closes the handle too, without the answer.
    ///
    /// A watcher whose session has ended (a reconnect) sends nothing and
    /// returns `Ok`: the server let go of the handle with the session.
    pub async fn close(mut self) -> Result<()> {
        self.pending.take();
        self.closed = true;
        match self.tree.close_handle(&mut self.conn, self.file_id).await {
            Err(Error::HandleInvalidated { .. }) => Ok(()),
            result => result,
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The pre-issued CHANGE_NOTIFY (if any) drops with the watcher, after
        // this: the server completes it when the CLOSE lands, and the
        // receiver task discards that late answer.
        if !self.closed {
            self.tree
                .close_dropped_handle(&self.conn, self.file_id, "Watcher");
        }
    }
}

/// Parse a chain of FILE_NOTIFY_INFORMATION entries from the response buffer.
///
/// Each entry has:
/// - `NextEntryOffset` (u32): offset to next entry, 0 for last
/// - `Action` (u32): the change type
/// - `FileNameLength` (u32): length of filename in bytes (UTF-16LE)
/// - `FileName` (variable): UTF-16LE, NOT null-terminated
///
/// Entries are 4-byte aligned.
fn parse_notify_information(data: &[u8]) -> Result<Vec<FileNotifyEvent>> {
    let mut events = Vec::new();
    let mut offset = 0usize;

    if data.is_empty() {
        return Ok(events);
    }

    loop {
        // Need at least 12 bytes for the fixed fields.
        if offset + 12 > data.len() {
            return Err(Error::invalid_data(
                "FILE_NOTIFY_INFORMATION truncated: not enough bytes for fixed fields",
            ));
        }

        let next_entry_offset =
            u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let action_raw = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
        let filename_length =
            u32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap()) as usize;

        // Filename starts right after the 12-byte fixed header.
        let filename_start = offset + 12;
        let filename_end = filename_start + filename_length;

        if filename_end > data.len() {
            return Err(Error::invalid_data(format!(
                "FILE_NOTIFY_INFORMATION filename extends beyond buffer: \
                 need {} bytes at offset {}, buffer is {} bytes",
                filename_length,
                filename_start,
                data.len()
            )));
        }

        let filename_bytes = &data[filename_start..filename_end];

        // A recursive watch reports a path, not a bare name, so this decodes
        // per component and hands back `/` separators like every other path
        // the caller deals with.
        let filename = crate::name::decode_path(&decode_utf16le(filename_bytes)?);
        let action = FileNotifyAction::from_u32(action_raw)?;

        events.push(FileNotifyEvent { action, filename });

        if next_entry_offset == 0 {
            break;
        }

        offset += next_entry_offset;
    }

    Ok(events)
}

/// Decode a UTF-16LE byte slice into a Rust String.
pub(super) fn decode_utf16le(bytes: &[u8]) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::invalid_data("UTF-16LE filename has odd byte count"));
    }

    let u16s: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();

    String::from_utf16(&u16s)
        .map_err(|e| Error::invalid_data(format!("invalid UTF-16LE filename: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_notify_entry() {
        // Build a single FILE_NOTIFY_INFORMATION entry.
        let filename = "test.txt";
        let utf16: Vec<u16> = filename.encode_utf16().collect();
        let filename_bytes: Vec<u8> = utf16.iter().flat_map(|c| c.to_le_bytes()).collect();
        let filename_len = filename_bytes.len() as u32;

        let mut data = Vec::new();
        // NextEntryOffset = 0 (last entry)
        data.extend_from_slice(&0u32.to_le_bytes());
        // Action = FILE_ACTION_ADDED (0x00000001)
        data.extend_from_slice(&1u32.to_le_bytes());
        // FileNameLength
        data.extend_from_slice(&filename_len.to_le_bytes());
        // FileName (UTF-16LE)
        data.extend_from_slice(&filename_bytes);

        let events = parse_notify_information(&data).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, FileNotifyAction::Added);
        assert_eq!(events[0].filename, "test.txt");
    }

    #[test]
    fn parse_multiple_notify_entries() {
        // Build two FILE_NOTIFY_INFORMATION entries.
        let build_entry = |name: &str, action: u32, is_last: bool| -> Vec<u8> {
            let utf16: Vec<u16> = name.encode_utf16().collect();
            let filename_bytes: Vec<u8> = utf16.iter().flat_map(|c| c.to_le_bytes()).collect();
            let filename_len = filename_bytes.len() as u32;

            let mut entry = Vec::new();
            // Fixed header is 12 bytes + filename. Align to 4 bytes.
            let entry_size = 12 + filename_bytes.len();
            let aligned_size = (entry_size + 3) & !3;

            let next_offset = if is_last { 0u32 } else { aligned_size as u32 };
            entry.extend_from_slice(&next_offset.to_le_bytes());
            entry.extend_from_slice(&action.to_le_bytes());
            entry.extend_from_slice(&filename_len.to_le_bytes());
            entry.extend_from_slice(&filename_bytes);

            // Pad to 4-byte alignment.
            while entry.len() < aligned_size {
                entry.push(0);
            }

            entry
        };

        let mut data = Vec::new();
        data.extend_from_slice(&build_entry("added.txt", 1, false));
        data.extend_from_slice(&build_entry("removed.txt", 2, true));

        let events = parse_notify_information(&data).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, FileNotifyAction::Added);
        assert_eq!(events[0].filename, "added.txt");
        assert_eq!(events[1].action, FileNotifyAction::Removed);
        assert_eq!(events[1].filename, "removed.txt");
    }

    #[test]
    fn parse_empty_buffer_returns_no_events() {
        let events = parse_notify_information(&[]).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_truncated_buffer_returns_error() {
        // Only 8 bytes, need at least 12 for fixed fields.
        let data = vec![0u8; 8];
        let result = parse_notify_information(&data);
        assert!(result.is_err());
    }

    #[test]
    fn decode_utf16le_basic() {
        let input = "hello";
        let utf16: Vec<u16> = input.encode_utf16().collect();
        let bytes: Vec<u8> = utf16.iter().flat_map(|c| c.to_le_bytes()).collect();
        let result = decode_utf16le(&bytes).unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn decode_utf16le_non_ascii() {
        let input = "photos/\u{00E9}t\u{00E9}";
        let utf16: Vec<u16> = input.encode_utf16().collect();
        let bytes: Vec<u8> = utf16.iter().flat_map(|c| c.to_le_bytes()).collect();
        let result = decode_utf16le(&bytes).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn decode_utf16le_odd_bytes_is_error() {
        let result = decode_utf16le(&[0x41, 0x00, 0x42]);
        assert!(result.is_err());
    }

    #[test]
    fn file_notify_action_display() {
        assert_eq!(format!("{}", FileNotifyAction::Added), "added");
        assert_eq!(format!("{}", FileNotifyAction::Removed), "removed");
        assert_eq!(format!("{}", FileNotifyAction::Modified), "modified");
        assert_eq!(
            format!("{}", FileNotifyAction::RenamedOldName),
            "renamed (old name)"
        );
        assert_eq!(
            format!("{}", FileNotifyAction::RenamedNewName),
            "renamed (new name)"
        );
    }

    #[test]
    fn stream_actions_are_typed() {
        for (raw, action) in [
            (6, FileNotifyAction::StreamAdded),
            (7, FileNotifyAction::StreamRemoved),
            (8, FileNotifyAction::StreamModified),
        ] {
            assert_eq!(FileNotifyAction::from_u32(raw).unwrap(), action);
        }
    }

    #[test]
    fn the_default_filter_stays_as_it_was() {
        assert_eq!(
            WatchOptions::new().completion_filter(),
            DEFAULT_COMPLETION_FILTER
        );
        let metadata = WatchOptions::new().metadata(true).completion_filter();
        for bit in [
            FILE_NOTIFY_CHANGE_EA,
            FILE_NOTIFY_CHANGE_STREAM_NAME,
            FILE_NOTIFY_CHANGE_STREAM_SIZE,
            FILE_NOTIFY_CHANGE_STREAM_WRITE,
        ] {
            assert_ne!(metadata & bit, 0);
            assert_eq!(DEFAULT_COMPLETION_FILTER & bit, 0);
        }
    }

    #[test]
    fn file_notify_action_from_u32_unknown_is_error() {
        let result = FileNotifyAction::from_u32(0x9999);
        assert!(result.is_err());
    }
}

/// Loss-window tests using a strict-server simulator.
///
/// These probe the architectural property the watcher contract should
/// guarantee: every event the server observes is eventually delivered
/// to the consumer, even when the server drops events that arrive
/// while no `CHANGE_NOTIFY` request is outstanding (the naspi / older
/// Samba behavior that triggered cmdr's field reproduction).
///
/// **TDD-red on `main`**: `LossySim` drops events when no request is
/// outstanding; current `next_events()` issues one CHANGE_NOTIFY per
/// call, so there's always a gap between response delivery and the
/// next request. Events pushed during that gap are dropped, and the
/// test fails. The pipelined-watcher fix (always keep one CHANGE_NOTIFY
/// pre-issued on the wire) closes the gap, the simulator never drops,
/// and the test passes.
#[cfg(test)]
mod loss_window_tests {
    use super::*;
    use crate::client::connection::{pack_message, Connection, NegotiatedParams};
    use crate::client::tree::Tree;
    use crate::msg::change_notify::ChangeNotifyResponse;
    use crate::msg::header::Header;
    use crate::pack::Guid;
    use crate::transport::{TransportReceive, TransportSend};
    use crate::types::flags::Capabilities;
    use crate::types::{Command, Dialect, MessageId, SessionId, TreeId};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::Notify;

    /// Simulates a CHANGE_NOTIFY server that DROPS events that arrive
    /// while no request is outstanding. Models naspi / older Samba
    /// firmware (the server side of cmdr's 9-files → 4-events field
    /// reproduction). Forgiving servers like Docker Samba buffer
    /// generously and won't trigger this; the simulator's job is to
    /// surface the architectural bug regardless of how forgiving any
    /// real server happens to be.
    struct LossySim {
        /// Outstanding CHANGE_NOTIFY request msg_ids (FIFO).
        outstanding: Mutex<VecDeque<u64>>,
        /// Events the server has observed but not yet delivered.
        pending_events: Mutex<Vec<(String, u32)>>,
        /// Response queue read by `receive()`.
        responses: Mutex<VecDeque<Vec<u8>>>,
        /// Count of events the server saw with no request outstanding.
        dropped: Mutex<usize>,
        send_notify: Notify,
        recv_notify: Notify,
        closed: AtomicBool,
    }

    impl LossySim {
        fn new() -> Self {
            Self {
                outstanding: Mutex::new(VecDeque::new()),
                pending_events: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::new()),
                dropped: Mutex::new(0),
                send_notify: Notify::new(),
                recv_notify: Notify::new(),
                closed: AtomicBool::new(false),
            }
        }

        /// Block until at least `n` CHANGE_NOTIFY requests are outstanding.
        async fn wait_outstanding_at_least(&self, n: usize) {
            loop {
                if self.outstanding.lock().unwrap().len() >= n {
                    return;
                }
                if self.closed.load(Ordering::Acquire) {
                    return;
                }
                self.send_notify.notified().await;
            }
        }

        fn outstanding_count(&self) -> usize {
            self.outstanding.lock().unwrap().len()
        }

        /// Push an event. If a CHANGE_NOTIFY request is outstanding, buffer
        /// the event for the next `deliver_pending()`. Else, drop silently
        /// and bump the dropped counter.
        fn push_event(&self, name: &str) {
            let outstanding = !self.outstanding.lock().unwrap().is_empty();
            if outstanding {
                self.pending_events
                    .lock()
                    .unwrap()
                    .push((name.to_string(), 1 /* FILE_ACTION_ADDED */));
            } else {
                *self.dropped.lock().unwrap() += 1;
            }
        }

        /// Wrap all buffered events into a single CHANGE_NOTIFY response,
        /// consuming one outstanding msg_id.
        fn deliver_pending(&self) {
            let msg_id = self.outstanding.lock().unwrap().pop_front();
            let events = std::mem::take(&mut *self.pending_events.lock().unwrap());
            if let Some(id) = msg_id {
                let resp = build_response(id, &events);
                self.responses.lock().unwrap().push_back(resp);
                self.recv_notify.notify_one();
            }
        }

        fn dropped_count(&self) -> usize {
            *self.dropped.lock().unwrap()
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Release);
            self.recv_notify.notify_waiters();
            self.send_notify.notify_waiters();
        }
    }

    #[async_trait]
    impl TransportSend for LossySim {
        async fn send(&self, data: &[u8]) -> crate::error::Result<()> {
            if let Some(msg_id) = extract_change_notify_msg_id(data) {
                self.outstanding.lock().unwrap().push_back(msg_id);
                self.send_notify.notify_waiters();
            }
            Ok(())
        }
    }

    #[async_trait]
    impl TransportReceive for LossySim {
        async fn receive(&self) -> crate::error::Result<Vec<u8>> {
            loop {
                if let Some(data) = self.responses.lock().unwrap().pop_front() {
                    return Ok(data);
                }
                if self.closed.load(Ordering::Acquire) {
                    return Err(crate::Error::Disconnected);
                }
                self.recv_notify.notified().await;
            }
        }
    }

    /// Pull `MessageId` out of a request frame, but only for CHANGE_NOTIFY.
    /// Non-CHANGE_NOTIFY sends are ignored by the simulator (the test
    /// pre-configures the connection so no other requests should hit this
    /// transport — but if any do, we won't track them).
    fn extract_change_notify_msg_id(data: &[u8]) -> Option<u64> {
        const HEADER_MIN: usize = 64;
        if data.len() < HEADER_MIN || &data[0..4] != b"\xFESMB" {
            return None;
        }
        let cmd = u16::from_le_bytes([data[12], data[13]]);
        if cmd != Command::ChangeNotify as u16 {
            return None;
        }
        Some(u64::from_le_bytes(data[24..32].try_into().unwrap()))
    }

    /// Pack a CHANGE_NOTIFY response carrying the given (name, action) pairs.
    fn build_response(msg_id: u64, events: &[(String, u32)]) -> Vec<u8> {
        let mut output_data = Vec::new();
        for (i, (name, action)) in events.iter().enumerate() {
            let is_last = i == events.len() - 1;
            let utf16: Vec<u16> = name.encode_utf16().collect();
            let filename_bytes: Vec<u8> = utf16.iter().flat_map(|c| c.to_le_bytes()).collect();
            let filename_len = filename_bytes.len() as u32;
            let entry_size = 12 + filename_bytes.len();
            let aligned_size = (entry_size + 3) & !3;
            let next_offset = if is_last { 0u32 } else { aligned_size as u32 };
            let start = output_data.len();
            output_data.extend_from_slice(&next_offset.to_le_bytes());
            output_data.extend_from_slice(&action.to_le_bytes());
            output_data.extend_from_slice(&filename_len.to_le_bytes());
            output_data.extend_from_slice(&filename_bytes);
            while output_data.len() - start < aligned_size {
                output_data.push(0);
            }
        }
        let mut h = Header::new_request(Command::ChangeNotify);
        h.flags.set_response();
        h.message_id = MessageId(msg_id);
        h.credits = 32;
        let body = ChangeNotifyResponse { output_data };
        pack_message(&h, &body)
    }

    fn setup_connection(sim: &Arc<LossySim>) -> Connection {
        let mut conn =
            Connection::from_transport(Box::new(sim.clone()), Box::new(sim.clone()), "test-server");
        // The credit window a real connection holds by the time it watches a
        // directory; a fresh pool is empty until the server grants.
        conn.set_credits(512);
        conn.set_test_params(NegotiatedParams {
            dialect: Dialect::Smb2_0_2,
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
        conn.set_session_id(SessionId(0x1234));
        conn
    }

    fn test_tree() -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    /// Cycle, repeated N times:
    ///   1. wait for outstanding (watcher armed)
    ///   2. push event A → buffered
    ///   3. deliver_pending → response queued, msg_id consumed
    ///   4. push GAP event → on `main`, no outstanding → DROPPED;
    ///      on the pipelined-watcher fix, the next request is already
    ///      issued → buffered.
    ///
    /// Final flush: one more wait_outstanding + push + deliver to make
    /// sure any buffered gap events on the fix path get out.
    ///
    /// On `main`: `dropped_count() > 0`, `delivered.len() < expected`.
    /// On the fix: `dropped_count() == 0`, all events delivered.
    /// `dispatch` must not return until the bytes are on the wire.
    ///
    /// This is the guarantee the pipelined watcher rests on, and the reason
    /// `dispatch` exists at all rather than `tokio::spawn(conn.execute(..))`:
    /// a spawned send may not run until the spawning task yields, leaving the
    /// window the watcher is supposed to close wide open. Asserted with no
    /// await between the dispatch resolving and the check, so no scheduler
    /// order can make a deferred send look eager.
    #[tokio::test]
    async fn dispatch_returns_only_once_the_request_is_on_the_wire() {
        let sim = Arc::new(LossySim::new());
        let conn = setup_connection(&sim);
        let tree = test_tree();

        let req = ChangeNotifyRequest {
            flags: SMB2_WATCH_TREE,
            output_buffer_length: OUTPUT_BUFFER_LENGTH,
            file_id: crate::types::FileId {
                persistent: 0x1111,
                volatile: 0x2222,
            },
            completion_filter: DEFAULT_COMPLETION_FILTER,
        };
        let _guard = conn
            .dispatch(Command::ChangeNotify, &req, Some(tree.tree_id))
            .await
            .expect("dispatch failed");

        assert_eq!(
            sim.outstanding_count(),
            1,
            "the server must already hold the request when dispatch resolves"
        );
        sim.close();
    }

    #[tokio::test]
    async fn watcher_does_not_lose_events_between_consecutive_requests() {
        let _ = env_logger::try_init();

        const N_CYCLES: usize = 5;

        let sim = Arc::new(LossySim::new());
        let conn = setup_connection(&sim);
        let tree = test_tree();

        let scenario_sim = sim.clone();
        let scenario = tokio::spawn(async move {
            let sim = scenario_sim;
            for round in 0..N_CYCLES {
                // Wait for the watcher to be pipelined (previous request
                // plus its pre-issued successor) before consuming one. That
                // is the invariant under test: delivering a response must
                // never leave the wire unarmed. Waiting on a bare
                // "at least one outstanding" instead would race the
                // watcher's re-arm and measure scheduler order, not the
                // guarantee — `dispatch` yields once now that a writer task
                // owns the socket, and a real `transport.send` yields too.
                sim.wait_outstanding_at_least(2).await;
                sim.push_event(&format!("a_{round:02}"));
                sim.deliver_pending();
                // Inline push (no .await) — outstanding queue was just
                // emptied by deliver_pending. On `main`, no request has
                // been re-issued yet, so this lands in the "drop" branch.
                // On the fix, a pre-issued request is still outstanding,
                // so it lands in the "buffer" branch.
                sim.push_event(&format!("gap_{round:02}"));
                // Models "time passes between server-side events". Real
                // workloads have at least a syscall worth of latency
                // between events, which is enough for the watcher task
                // to wake up, process the previous response, and
                // re-dispatch. The pipelining fix only guarantees one
                // outstanding through the response-processing window,
                // not through arbitrary back-to-back synchronous
                // delivers within a single scheduler quantum.
                tokio::task::yield_now().await;
            }
            // Flush: drive one more cycle to push any buffered gap events
            // out the door for the fix path.
            sim.wait_outstanding_at_least(2).await;
            sim.push_event("flush_marker");
            sim.deliver_pending();
            // Brief grace period for the watcher to drain the response,
            // then close so its next next_events() returns Disconnected
            // and the consumer loop exits.
            tokio::time::sleep(Duration::from_millis(50)).await;
            sim.close();
        });

        let mut watcher = Watcher::new(
            tree,
            conn,
            crate::types::FileId {
                persistent: 0x1111,
                volatile: 0x2222,
            },
            WatchOptions::new().recursive(true),
        );
        let mut delivered: Vec<String> = Vec::new();
        while let Ok(events) = watcher.next_events().await {
            for e in &events {
                delivered.push(e.filename.clone());
            }
        }
        scenario.await.unwrap();

        let dropped = sim.dropped_count();
        // `a_*` events always land in the outstanding window. `flush_marker`
        // ditto. `gap_*` events expose the bug: dropped today, delivered
        // after the fix.
        let expected_min = N_CYCLES /* a_* */ + 1 /* flush_marker */;
        let expected_max = expected_min + N_CYCLES /* gap_* */;

        assert!(
            delivered.len() >= expected_min,
            "watcher dropped 'a_*' or 'flush_marker' events: got {:?}",
            delivered
        );
        assert_eq!(
            dropped, 0,
            "{} server-side event(s) arrived with no outstanding CHANGE_NOTIFY \
             request and were dropped. The pipelined-watcher fix should keep \
             one CHANGE_NOTIFY request continuously outstanding so no event \
             ever lands in the drop branch. Delivered to consumer: {:?}",
            dropped, delivered
        );
        assert_eq!(
            delivered.len(),
            expected_max,
            "expected every 'a_*', 'gap_*', and 'flush_marker' event delivered; \
             got {:?}",
            delivered
        );
    }
}

#[cfg(test)]
mod close_on_drop_tests {
    use super::*;
    use crate::client::test_helpers::{
        build_close_response, build_flush_response, setup_connection,
    };
    use crate::msg::header::Header;
    use crate::pack::{ReadCursor, Unpack};
    use crate::transport::MockTransport;
    use crate::types::TreeId;
    use std::sync::Arc;

    const DIR: FileId = FileId {
        persistent: 0x31,
        volatile: 0x32,
    };

    fn tree() -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    fn sent_commands(mock: &MockTransport) -> Vec<Command> {
        mock.sent_messages()
            .iter()
            .map(|bytes| Header::unpack(&mut ReadCursor::new(bytes)).unwrap().command)
            .collect()
    }

    /// A watcher dropped without `close` (a pane closing) releases the
    /// directory handle, ahead of anything sent after it.
    #[tokio::test]
    async fn a_dropped_watcher_closes_its_handle() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());
        let mut conn = setup_connection(&mock);

        drop(Watcher::new(tree(), conn.clone(), DIR, WatchOptions::new()));
        tree()
            .flush_handle(&mut conn, FileId::default())
            .await
            .unwrap();

        assert_eq!(sent_commands(&mock), [Command::Close, Command::Flush]);
    }

    #[tokio::test]
    async fn a_closed_watcher_sends_one_close() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_close_response());
        mock.queue_response(build_flush_response());
        let mut conn = setup_connection(&mock);

        Watcher::new(tree(), conn.clone(), DIR, WatchOptions::new())
            .close()
            .await
            .unwrap();
        tree()
            .flush_handle(&mut conn, FileId::default())
            .await
            .unwrap();

        assert_eq!(sent_commands(&mock), [Command::Close, Command::Flush]);
    }
}
