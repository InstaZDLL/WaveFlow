//! Tree (share) connection and file operations.
//!
//! The [`Tree`] type represents a connection to a specific share on the server.
//! It provides methods for directory listing, file reading/writing, deletion,
//! renaming, stat, and directory creation.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, info, trace, warn};

use crate::client::connection::{CompoundOp, Connection, Frame, SessionPin, WaiterGuard};
use crate::client::credits;
use crate::client::directory::{DirectoryReader, ListOptions};
use crate::client::durable::FileIdentity;
use crate::client::stream::{FileDownload, Progress};
use crate::client::write_pipe::WritePipe;
use crate::error::Result;
use crate::msg::close::CloseRequest;
use crate::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use crate::msg::directory_info;
use crate::msg::flush::FlushRequest;
use crate::msg::query_directory::{
    FileInformationClass, QueryDirectoryFlags, QueryDirectoryRequest, QueryDirectoryResponse,
};
use crate::msg::query_info::{InfoType, QueryInfoRequest, QueryInfoResponse};
use crate::msg::read::{ReadRequest, ReadResponse, SMB2_CHANNEL_NONE};
use crate::msg::set_info::SetInfoRequest;
use crate::msg::tree_connect::{TreeConnectRequest, TreeConnectRequestFlags, TreeConnectResponse};
use crate::msg::tree_disconnect::TreeDisconnectRequest;
use crate::msg::write::{WriteRequest, WriteResponse};
use crate::pack::{FileTime, ReadCursor, Unpack};
use crate::rt;
use crate::types::flags::{FileAccessMask, FileAttributes};
use crate::types::reparse::ReparseTag;
use crate::types::status::NtStatus;
#[cfg(test)]
use crate::types::MessageId;
use crate::types::{Command, CreditCharge, FileId, OplockLevel, TreeId};
use crate::Error;

/// Maximum number of READs to keep in flight during a pipelined read.
///
/// More than 32 in-flight requests creates diminishing returns and
/// increases memory usage (buffering responses). 32 x 64 KB = 2 MB
/// in flight is plenty for Gigabit LAN. Writes pace through `WritePipe`
/// instead.
const MAX_PIPELINE_WINDOW: usize = 32;

/// Pull `next_chunk` dry into `pipe`, then wait for every WRITE's answer.
/// Returns the bytes the server confirmed.
///
/// The body of [`Tree::write_file_streamed`]: a callback error stops the
/// pulling, waits out what's already on the wire, and comes back as
/// [`Error::Io`]; an empty chunk ends the stream like `None`.
async fn write_streamed<F>(pipe: &mut WritePipe, next_chunk: &mut F) -> Result<u64>
where
    F: FnMut() -> Option<std::result::Result<Vec<u8>, std::io::Error>>,
{
    loop {
        match next_chunk() {
            None => break,
            Some(Err(e)) => {
                pipe.drain().await?;
                return Err(Error::Io(e));
            }
            Some(Ok(data)) if data.is_empty() => break,
            Some(Ok(data)) => {
                let mut sent = 0;
                while sent < data.len() {
                    let len = (data.len() - sent).min(pipe.next_len() as usize);
                    pipe.send(data[sent..sent + len].to_vec()).await?;
                    sent += len;
                }
            }
        }
    }
    pipe.drain().await?;
    Ok(pipe.confirmed())
}

/// Unwrap an `execute_compound` result, propagating the first inner
/// waiter-level error (session expired, signature verify failure,
/// connection disconnected mid-await) as the outer `Err`. Returns a
/// `Vec<Frame>` of exactly `expected` frames, so callers can index per
/// sub-op.
///
/// Any routing-level failure aborts the whole operation rather than
/// silently handing back a partial response list the caller would have to
/// inspect one-by-one. Sub-op status codes (`STATUS_OBJECT_NAME_NOT_FOUND`
/// and friends) are NOT errors here; they ride in `Frame::header.status`
/// and the caller checks them.
///
/// The `expected` count is what keeps every caller's `responses[2]` from
/// being a panic: a server that answers a three-op chain with two frames is
/// a protocol error, not a reason to take the process down.
pub(super) fn all_or_first_err(
    frames: Vec<Result<crate::client::connection::Frame>>,
    expected: usize,
) -> Result<Vec<crate::client::connection::Frame>> {
    let mut out = Vec::with_capacity(frames.len());
    for r in frames {
        out.push(r?);
    }
    if out.len() != expected {
        return Err(Error::invalid_data(format!(
            "compound response has {} frames, expected {}",
            out.len(),
            expected
        )));
    }
    Ok(out)
}

/// Create option: the target must be a directory.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

/// Create option: the target must not be a directory.
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;

/// Create option: open a reparse point itself, not what it points to
/// (MS-SMB2 § 2.2.13). Without it, the server opens a junction's target and
/// refuses a symlink with `STATUS_STOPPED_ON_SYMLINK` (§ 3.3.5.9). On anything
/// that isn't a reparse point it changes nothing.
pub(super) const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Share mode that lets every other open keep reading, writing, and deleting.
pub(super) const SHARE_ALL: ShareAccess = ShareAccess(
    ShareAccess::FILE_SHARE_READ | ShareAccess::FILE_SHARE_WRITE | ShareAccess::FILE_SHARE_DELETE,
);

/// FileBasicInformation class for QUERY_INFO (MS-FSCC 2.4.7).
pub(super) const FILE_BASIC_INFORMATION: u8 = 4;

/// FileStandardInformation class for QUERY_INFO (MS-FSCC 2.4.41).
pub(super) const FILE_STANDARD_INFORMATION: u8 = 5;

/// FileAttributeTagInformation class for QUERY_INFO (MS-FSCC 2.4.6): the
/// attributes and the reparse tag.
const FILE_ATTRIBUTE_TAG_INFORMATION: u8 = 35;

/// FileRenameInformation class for SET_INFO (MS-FSCC 2.4.34.2).
const FILE_RENAME_INFORMATION: u8 = 10;

/// FileDispositionInformation class for SET_INFO (MS-FSCC 2.4.11).
const FILE_DISPOSITION_INFORMATION: u8 = 13;

/// How long a delete whose CLOSE came back `STATUS_UNSUCCESSFUL` waits before
/// its one retry: room for the delete that won QNAP's recycle-folder race to
/// finish (see `delete_compound`).
const DELETE_RETRY_PAUSE: Duration = Duration::from_millis(50);

/// FileEndOfFileInformation class for SET_INFO (MS-FSCC 2.4.14).
const FILE_END_OF_FILE_INFORMATION: u8 = 20;

/// FileFsFullSizeInformation class for QUERY_INFO (MS-FSCC 2.5.4).
const FILE_FS_FULL_SIZE_INFORMATION: u8 = 7;

/// A directory entry returned by [`Tree::list_directory`].
///
/// `#[non_exhaustive]`, so a later release can report more about an entry
/// without breaking anyone. To build one yourself (a test fixture for code
/// that maps entries, say), start from [`Default`] and set the fields:
///
/// ```
/// let mut entry = smb2::DirectoryEntry::default();
/// entry.name = "report.pdf".to_string();
/// entry.size = 1024;
/// assert!(!entry.is_directory);
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DirectoryEntry {
    /// The file or directory name.
    pub name: String,
    /// The file size in bytes (0 for directories).
    pub size: u64,
    /// Whether this entry is a directory.
    pub is_directory: bool,
    /// The creation time.
    pub created: FileTime,
    /// The last modification time.
    pub modified: FileTime,
    /// When the contents or the metadata last changed (`ChangeTime`). Moves on
    /// a metadata-only change too (attributes, timestamps, security), which
    /// `modified` misses, so a sync tool compares it to catch those.
    pub changed: FileTime,
    /// The entry's attributes: Hidden, Read-only, Directory, and the rest.
    pub attributes: FileAttributes,
    /// The file system's 64-bit file ID for this entry, the same number
    /// [`FileIdentity::index_number`](crate::FileIdentity) carries. Only a
    /// listing that asked for it fills it in
    /// ([`ListOptions::index_numbers`](crate::ListOptions::index_numbers)),
    /// and only when the server sends a non-zero one. It identifies a file
    /// within one volume; a listing doesn't say which volume, so it isn't a
    /// whole `FileIdentity` on its own.
    pub index_number: Option<u64>,
    /// What kind of reparse point the entry is, `Some` exactly when
    /// `attributes` has [`REPARSE_POINT`](FileAttributes::REPARSE_POINT)
    /// (both listing classes carry the tag then, MS-FSCC § 2.4.8 and
    /// § 2.4.17).
    ///
    /// A junction or a directory symlink still lists with `is_directory`
    /// set, so check [`is_link`](Self::is_link) before descending into one: a
    /// recursive delete that walks into a junction deletes its target's
    /// files.
    pub reparse_tag: Option<ReparseTag>,
}

impl DirectoryEntry {
    /// The entry is a link to another place (a junction, a symlink, or any
    /// other name surrogate, [`ReparseTag::is_name_surrogate`]). A walk
    /// should list it and not descend into it, and delete it with
    /// [`Tree::delete_link`], which leaves its target alone.
    ///
    /// Only as good as the server's listing: Samba lists a Unix symlink as
    /// whatever it points to (verified on 4.20.6 and 4.23.10, 2026-10-20), so
    /// this is `false` for one there. On Samba 4.23 a [`Tree::stat`] of the
    /// path does say ([`FileInfo::is_link`]).
    pub fn is_link(&self) -> bool {
        self.reparse_tag.is_some_and(ReparseTag::is_name_surrogate)
    }
}

/// One QUERY_DIRECTORY round trip within a directory listing, captured by
/// [`Tree::list_directory_instrumented`].
#[derive(Debug, Clone)]
pub struct QueryStep {
    /// Wall-clock time for this QUERY_DIRECTORY request and its response.
    pub elapsed: Duration,
    /// Directory entries this round trip returned (0 for the terminal
    /// NO_MORE_FILES reply).
    pub entries: usize,
    /// Output-buffer bytes the server returned for this round trip.
    pub bytes: usize,
    /// True for the terminal round trip that returned STATUS_NO_MORE_FILES.
    pub no_more_files: bool,
}

/// Per-phase timing for a single directory listing, returned by
/// [`Tree::list_directory_instrumented`]. Every field is a real wire round
/// trip, so [`total`](Self::total) sums to the listing's wall time minus
/// local parse cost. Useful for diagnosing slow shares: it shows whether the
/// CREATE, the QUERY_DIRECTORY loop, or the CLOSE dominates, and how many
/// QUERY round trips a directory needs (which the query buffer size drives).
#[derive(Debug, Clone)]
pub struct ListingTrace {
    /// Time for the CREATE that opens the directory handle.
    pub create: Duration,
    /// One entry per QUERY_DIRECTORY round trip, in order. The last one is the
    /// terminal NO_MORE_FILES reply.
    pub queries: Vec<QueryStep>,
    /// Time for the CLOSE that releases the handle.
    pub close: Duration,
    /// Output-buffer length (bytes) requested per QUERY_DIRECTORY.
    pub query_buffer_len: u32,
    /// Total directory entries returned (includes `.` and `..`).
    pub entries: usize,
}

impl ListingTrace {
    /// Total wire time: CREATE + every QUERY_DIRECTORY + CLOSE.
    pub fn total(&self) -> Duration {
        self.create + self.query_total() + self.close
    }

    /// Summed time of all QUERY_DIRECTORY round trips (including the terminal
    /// NO_MORE_FILES reply).
    pub fn query_total(&self) -> Duration {
        self.queries.iter().map(|q| q.elapsed).sum()
    }

    /// Total round trips: CREATE + every QUERY_DIRECTORY + CLOSE.
    pub fn round_trips(&self) -> usize {
        1 + self.queries.len() + 1
    }
}

/// Outcome of a single QUERY_DIRECTORY round trip.
pub(crate) enum QueryStepOutcome {
    /// The server returned entries. `bytes` is the output-buffer length.
    Entries {
        entries: Vec<DirectoryEntry>,
        bytes: usize,
    },
    /// The server returned STATUS_NO_MORE_FILES, ending the scan.
    NoMoreFiles { bytes: usize },
}

/// File metadata returned by [`Tree::stat`].
///
/// `#[non_exhaustive]`, so a later release can report more about a file
/// without breaking anyone. To build one yourself (a test fixture, say),
/// start from [`Default`] and set the fields:
///
/// ```
/// let mut info = smb2::FileInfo::default();
/// info.size = 2048;
/// info.modified = smb2::pack::FileTime(133_485_408_000_000_000);
/// assert_eq!(info.changed, smb2::pack::FileTime::ZERO);
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FileInfo {
    /// The file size in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_directory: bool,
    /// The creation time.
    pub created: FileTime,
    /// The last modification time.
    pub modified: FileTime,
    /// The last access time.
    pub accessed: FileTime,
    /// When the contents or the metadata last changed (`ChangeTime`). Moves on
    /// a metadata-only change too (attributes, timestamps, security), which
    /// `modified` misses, so a sync tool compares it to catch those.
    pub changed: FileTime,
    /// The file's attributes: Hidden, Read-only, Directory, and the rest.
    pub attributes: FileAttributes,
    /// What kind of reparse point the file is, when `attributes` has
    /// [`REPARSE_POINT`](FileAttributes::REPARSE_POINT) and the call asked
    /// (`FileAttributeTagInformation`, MS-FSCC § 2.4.6). [`Tree::stat`] and
    /// its siblings ask; the metadata an open hands back
    /// ([`Tree::read_file_compound_with_info`], the streaming handles)
    /// doesn't carry it, so it's `None` there.
    pub reparse_tag: Option<ReparseTag>,
}

impl FileInfo {
    /// The file is a link to another place (a junction, a symlink, or any
    /// other name surrogate, [`ReparseTag::is_name_surrogate`]). Only as good
    /// as [`reparse_tag`](Self::reparse_tag): `false` when the call didn't
    /// ask.
    pub fn is_link(&self) -> bool {
        self.reparse_tag.is_some_and(ReparseTag::is_name_surrogate)
    }
}

/// File system space information for a share.
#[derive(Debug, Clone)]
pub struct FsInfo {
    /// Total capacity in bytes.
    pub total_bytes: u64,
    /// Free space available to the caller in bytes.
    pub free_bytes: u64,
    /// Total free space on the volume in bytes (may differ from
    /// `free_bytes` if quotas are in effect).
    pub total_free_bytes: u64,
    /// Bytes per sector.
    pub bytes_per_sector: u32,
    /// Sectors per allocation unit (cluster).
    pub sectors_per_unit: u32,
}

/// How a rename treats a destination that already exists, for
/// [`Tree::rename_with`] and [`SmbClient::rename_with`](crate::SmbClient::rename_with).
///
/// The default is what [`Tree::rename`] does: refuse an existing destination.
///
/// ```
/// use smb2::RenameOptions;
///
/// let options = RenameOptions::new().replace_if_exists(true);
/// assert!(options.replace_if_exists);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenameOptions {
    /// Replace a file already at the destination, in the same step as the
    /// rename (`ReplaceIfExists`, MS-FSCC § 2.4.42.2).
    pub replace_if_exists: bool,
}

impl RenameOptions {
    /// Refuse an existing destination, like [`Tree::rename`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace a file already at the destination instead of refusing it.
    #[must_use]
    pub fn replace_if_exists(mut self, replace: bool) -> Self {
        self.replace_if_exists = replace;
        self
    }
}

/// Where a tree came from, when a DFS referral put it somewhere else.
///
/// A file manager showing `\\fs01\projects_dfs` for a path the person typed as
/// `\\corp.example.com\projects` is showing them a server they have never heard of.
/// Every other client keeps the requested name (macOS reports
/// `SERVER_NAME corp.example.com` for exactly this mount), so a library that threw
/// it away would make every consumer shadow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfsOrigin {
    /// The UNC path the caller asked for, for example `\\corp.example.com\projects`.
    pub requested: String,
    /// The referral target this tree actually sits on, for example
    /// `\\fs01\projects_dfs`.
    pub target: String,
}

/// Where a path behind a DFS link really lives, from
/// [`SmbClient::resolve_dfs_link`](crate::SmbClient::resolve_dfs_link).
///
/// The target's server and share are [`tree`](Self::tree)'s
/// [`server`](Tree::server) and [`share_name`](Tree::share_name).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DfsLink {
    /// The link's target share, connected and pooled by the client. Every
    /// `SmbClient` call takes it as is; for [`Tree`]'s own methods, its
    /// connection is
    /// [`SmbClient::connection_for`](crate::SmbClient::connection_for).
    pub tree: Tree,
    /// The path on [`tree`](Self::tree) that the caller's path names: past
    /// the link folder, under the target's own folder when the link points
    /// inside its share. Empty for the link folder itself (the target's
    /// root).
    pub path: String,
    /// The link folder on the caller's share, as the caller's path spells it
    /// (`data`, or `projects/data`). Every path under it lives on
    /// [`tree`](Self::tree) too, for as long as the server's referral says
    /// (its TTL).
    pub link_path: String,
}

/// A connection to a specific share (tree connect).
///
/// `#[non_exhaustive]`: only the library produces a `Tree`, so nothing is lost
/// by making it unconstructable from outside, and every later field is free.
/// `Debug` holds nothing secret: an id, two names, and three flags. Printing
/// a tree is what a consumer reaches for first when a DFS redirect put it
/// somewhere unexpected.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Tree {
    /// The tree ID assigned by the server.
    pub tree_id: TreeId,
    /// The share name.
    pub share_name: String,
    /// The server name (hostname or IP) this tree is connected to.
    ///
    /// Used by `SmbClient` to route operations through the correct
    /// connection when DFS referrals point to different servers.
    pub server: String,
    /// Whether the share is a DFS share.
    pub is_dfs: bool,
    /// Whether the share requires encryption.
    pub encrypt_data: bool,
    /// `Some` when DFS resolution moved this tree off the path the caller
    /// named. Show the caller [`DfsOrigin::requested`], not
    /// [`server`](Self::server).
    pub dfs_origin: Option<DfsOrigin>,
    /// The connection and session this tree was connected on. Every request
    /// a `Tree` method sends goes out pinned to it, so once that session is
    /// gone (a reconnect) or the tree is used on another connection, the
    /// request fails with [`Error::HandleInvalidated`] instead of sending a
    /// tree id the receiving session may have given a different share. `None`
    /// for a tree built by hand, which nothing checks.
    pub(crate) pin: Option<SessionPin>,
}

impl Tree {
    /// Connect to a share on the server.
    ///
    /// Sends a TREE_CONNECT request with the UNC path `\\server\share`
    /// encoded in UTF-16LE.
    pub async fn connect(conn: &mut Connection, share_name: &str) -> Result<Tree> {
        // Pinned before the TREE_CONNECT, so a revival that lands before the
        // answer leaves the tree pinned to the dead session it belongs to.
        let conn = &mut conn.pinned_to_session();
        let server = conn.server_name().to_string();
        let unc_path = format!(r"\\{}\{}", server, share_name);

        let req = TreeConnectRequest {
            flags: TreeConnectRequestFlags::default(),
            path: unc_path,
        };

        let frame = conn.execute(Command::TreeConnect, &req, None).await?;

        if frame.header.command != Command::TreeConnect {
            return Err(Error::invalid_data(format!(
                "expected TreeConnect response, got {:?}",
                frame.header.command
            )));
        }

        if frame.header.status != NtStatus::SUCCESS {
            // `STATUS_BAD_NETWORK_NAME` stands for two unrelated things, and
            // only the error context tells them apart: a share that isn't
            // there, and a scale-out cluster moving one to another node
            // (MS-SMB2 § 2.2.2.2.2). They get different errors, so nothing
            // downstream — DFS namespace-root resolution above all — can
            // mistake a redirect for a namespace.
            if frame.header.status == NtStatus::BAD_NETWORK_NAME {
                let mut cursor = ReadCursor::new(&frame.body);
                if let Ok(err) = crate::msg::header::ErrorResponse::unpack(&mut cursor) {
                    if err.is_share_redirect() {
                        debug!("tree: redirected to another cluster node share={share_name:?}");
                        return Err(Error::ShareRedirected {
                            share: share_name.to_string(),
                        });
                    }
                }
            }
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::TreeConnect,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = TreeConnectResponse::unpack(&mut cursor)?;

        let tree_id = frame
            .header
            .tree_id
            .ok_or_else(|| Error::invalid_data("TreeConnect response missing tree ID"))?;

        let is_dfs = resp
            .capabilities
            .contains(crate::types::flags::ShareCapabilities::DFS);
        let encrypt_data = resp
            .share_flags
            .contains(crate::types::flags::ShareFlags::ENCRYPT_DATA);

        info!("tree: connected share={share_name:?}, tree_id={tree_id}");
        debug!("tree: is_dfs={}, encrypt_data={}", is_dfs, encrypt_data);

        if is_dfs {
            conn.register_dfs_tree(tree_id);
        }

        Ok(Tree {
            tree_id,
            share_name: share_name.to_string(),
            server: server.clone(),
            is_dfs,
            encrypt_data,
            dfs_origin: None,
            pin: conn.pin(),
        })
    }

    /// `conn`, pinned to the session this tree was connected on (and to
    /// whatever `conn` is already pinned to). Every method sending on this
    /// tree's behalf goes through it.
    pub(crate) fn pinned_conn(&self, conn: &Connection) -> Connection {
        conn.pinned_to(self.pin)
    }

    /// Normalize and format a path for this tree.
    ///
    /// When `is_dfs` is true, the server expects the path to include the
    /// `server\share\` prefix (MS-SMB2 3.2.4.3: "the client MUST pass a
    /// DFS path containing the server, share, and path to the open").
    /// The server strips the first two path components to get the local path,
    /// and if the resulting path starts with a DFS link name, it returns
    /// `STATUS_PATH_NOT_COVERED` so the client can resolve the referral.
    pub(crate) fn format_path(&self, path: &str) -> String {
        let normalized = normalize_path(path);
        if self.is_dfs {
            // The server strips exactly two components off this, so the host
            // has to be the WHOLE host. ❌ Never `split(':')`: an IPv6 literal
            // would arrive as its first group and the server would strip into
            // the caller's path (`crate::client::host_of`).
            let hostname = crate::client::host_of(&self.server);
            if normalized.is_empty() {
                format!("{}\\{}", hostname, self.share_name)
            } else {
                format!("{}\\{}\\{}", hostname, self.share_name, normalized)
            }
        } else {
            normalized
        }
    }

    /// Open a directory for listing one server batch at a time.
    ///
    /// Sends the CREATE and hands back a [`DirectoryReader`] whose
    /// [`next_batch`](DirectoryReader::next_batch) returns one QUERY_DIRECTORY
    /// answer per call and closes the handle at the end of the listing. The
    /// reader owns a clone of `conn` and of this tree, so it borrows nothing
    /// and can move to another task.
    pub async fn open_directory_reader(
        &self,
        conn: Connection,
        path: &str,
    ) -> Result<DirectoryReader> {
        self.open_directory_reader_with(conn, path, ListOptions::new())
            .await
    }

    /// [`open_directory_reader`](Self::open_directory_reader), asking the
    /// server for what `options` names as well (see [`ListOptions`]).
    pub async fn open_directory_reader_with(
        &self,
        conn: Connection,
        path: &str,
        options: ListOptions,
    ) -> Result<DirectoryReader> {
        let buffer_len = Self::default_query_buffer_len(&conn);
        self.open_directory_reader_sized(conn, path, buffer_len, options)
            .await
    }

    /// [`open_directory_reader_with`](Self::open_directory_reader_with) asking
    /// for `buffer_len` bytes of entries per QUERY_DIRECTORY.
    async fn open_directory_reader_sized(
        &self,
        conn: Connection,
        path: &str,
        buffer_len: u32,
        options: ListOptions,
    ) -> Result<DirectoryReader> {
        trace!("tree: open_directory_reader path={path:?}");
        // A server that refused class 37 once on this connection is listed
        // the default way from the start.
        let class = if options.index_numbers && !conn.listing_ids_refused() {
            FileInformationClass::FileIdBothDirectoryInformation
        } else {
            FileInformationClass::FileBothDirectoryInformation
        };
        // Pinned before the CREATE, so a revival that lands before the answer
        // leaves the handle pinned to the session it belongs to.
        let mut conn = self.pinned_conn(&conn).pinned_to_session();
        let file_id = self.open_directory(&mut conn, path).await?;
        Ok(DirectoryReader::new(
            Arc::new(self.clone()),
            conn,
            file_id,
            buffer_len,
            class,
        ))
    }

    /// List files in a directory.
    ///
    /// Opens the directory with CREATE, queries entries with QUERY_DIRECTORY
    /// (looping until STATUS_NO_MORE_FILES), then closes the handle. To handle
    /// each server batch as it arrives instead, or to stop early, use
    /// [`open_directory_reader`](Self::open_directory_reader): this collects
    /// through one, so both send the same requests.
    pub async fn list_directory(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<Vec<DirectoryEntry>> {
        self.list_directory_with(conn, path, ListOptions::new())
            .await
    }

    /// [`list_directory`](Self::list_directory), asking the server for what
    /// `options` names as well (see [`ListOptions`]).
    pub async fn list_directory_with(
        &self,
        conn: &mut Connection,
        path: &str,
        options: ListOptions,
    ) -> Result<Vec<DirectoryEntry>> {
        // TRACE, not DEBUG: a recursive scan calls list_directory once per directory
        // (millions of times on a large share), so at DEBUG it dominates a consumer's
        // log. Per-operation mutations (rename/delete/write) stay at DEBUG. See AGENTS.md.
        trace!("tree: list_directory path={path:?}");
        let mut reader = self
            .open_directory_reader_with(conn.clone(), path, options)
            .await?;
        let mut entries = Vec::new();
        // An error has already closed the handle; the reader says so.
        while let Some(batch) = reader.next_batch().await? {
            entries.extend(batch);
        }
        trace!("tree: list_directory done, entries={}", entries.len());
        Ok(entries)
    }

    /// List a directory while recording a per-phase timing breakdown.
    ///
    /// Same wire path as [`list_directory`](Self::list_directory) (CREATE →
    /// QUERY_DIRECTORY loop → CLOSE), but returns a [`ListingTrace`] alongside
    /// the entries, timing each round trip. Meant for diagnosing slow shares
    /// and sizing protocol tweaks, not for the hot path.
    ///
    /// `query_buffer_len` overrides the output-buffer length requested per
    /// QUERY_DIRECTORY. `None` uses the production default (min of the server's
    /// max transact size and 65536). A larger buffer packs more entries per
    /// round trip, cutting the number of QUERY_DIRECTORY calls a fat directory
    /// needs; the request charges credits to match (ceil(len / 65536)).
    pub async fn list_directory_instrumented(
        &self,
        conn: &mut Connection,
        path: &str,
        query_buffer_len: Option<u32>,
    ) -> Result<(Vec<DirectoryEntry>, ListingTrace)> {
        let buffer_len = query_buffer_len.unwrap_or_else(|| Self::default_query_buffer_len(conn));

        let create_start = Instant::now();
        let mut reader = self
            .open_directory_reader_sized(conn.clone(), path, buffer_len, ListOptions::new())
            .await?;
        let create = create_start.elapsed();

        reader.record_timings();
        let mut entries = Vec::new();
        while let Some(batch) = reader.next_batch().await? {
            entries.extend(batch);
        }
        let timings = reader.take_timings();

        let trace = ListingTrace {
            create,
            queries: timings.queries,
            close: timings.close,
            query_buffer_len: buffer_len,
            entries: entries.len(),
        };
        Ok((entries, trace))
    }

    /// Read a small file using a compound CREATE+READ+CLOSE request.
    ///
    /// Sends all three operations in a single transport frame, reducing
    /// round-trips from 3 to 1. Best for files that fit in a single
    /// READ (up to MaxReadSize).
    ///
    /// Knowing nothing about the file, this asks for a whole `MaxReadSize` and
    /// pays the credits to match: 130 on a server granting 8 MiB reads, which
    /// on a 512-credit window is three concurrent reads and no more, whatever
    /// the files actually weigh. When the size is already known (a directory
    /// scan, a stat), reach for
    /// [`read_file_compound_sized`](Self::read_file_compound_sized) instead
    /// and let the connection carry many more of them at once.
    ///
    /// A single READ can't return more than the server's `MaxReadSize`, so a
    /// file larger than that fails with
    /// [`Error::FileTooLargeForSingleRead`]
    /// rather than coming back truncated. Use
    /// [`read_file_pipelined`](Self::read_file_pipelined) for files of any size.
    pub async fn read_file_compound(&self, conn: &mut Connection, path: &str) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);
        self.read_file_compound_sized(conn, path, max_read as u64)
            .await
    }

    /// [`read_file_compound`](Self::read_file_compound) for a file whose size
    /// the caller already knows.
    ///
    /// `expected_size` is the caller's best knowledge of the file's size (from
    /// a directory scan, say). It bounds both the READ length and the credit
    /// charge, which is what makes this worth reaching for: the charge follows
    /// the expected response size (MS-SMB2 § 3.2.4.1.2), so a 4 MiB file read
    /// as 4 MiB costs 66 credits where a full-window read of the same file
    /// costs 130. On a 512-credit window that is the difference between seven
    /// concurrent reads and three. Size a batch with
    /// [`Connection::credit_capacity_for`](crate::client::Connection::credit_capacity_for).
    ///
    /// `expected_size` is a bound, not a promise: a file bigger than it fails
    /// with [`Error::FileTooLargeForSingleRead`] rather than coming back
    /// truncated, and that includes a file that grew between the scan and this
    /// read. The error carries the server's authoritative `size`, so a caller
    /// can retry with it when it still fits one READ, or switch to
    /// [`read_file_pipelined`](Self::read_file_pipelined), which handles any
    /// size. Over-estimating only costs credits; under-estimating costs a
    /// round-trip.
    pub async fn read_file_compound_sized(
        &self,
        conn: &mut Connection,
        path: &str,
        expected_size: u64,
    ) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        self.read_file_compound_sized_with_info(conn, path, expected_size)
            .await
            .map(|(data, _)| data)
    }

    /// [`read_file_compound`](Self::read_file_compound), plus the file's
    /// metadata as the server reported it when it opened the file: size,
    /// whether it's a directory, and its creation, modification, and access
    /// times.
    ///
    /// The CREATE response carries all of it, so this costs nothing over the
    /// plain read: same single round trip, same bytes on the wire. Reach for
    /// it rather than a [`stat`](Self::stat) beside the read when a copy needs
    /// the source's dates.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let (data, info) = tree.read_file_compound_with_info(conn, "notes.txt").await?;
    /// println!("{} bytes, modified {:?}", data.len(), info.modified.to_system_time());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn read_file_compound_with_info(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<(Vec<u8>, FileInfo)> {
        let conn = &mut self.pinned_conn(conn);
        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);
        self.read_file_compound_sized_with_info(conn, path, max_read as u64)
            .await
    }

    /// [`read_file_compound_sized`](Self::read_file_compound_sized), plus the
    /// file's metadata from the same CREATE response, as
    /// [`read_file_compound_with_info`](Self::read_file_compound_with_info)
    /// describes.
    pub async fn read_file_compound_sized_with_info(
        &self,
        conn: &mut Connection,
        path: &str,
        expected_size: u64,
    ) -> Result<(Vec<u8>, FileInfo)> {
        let wire_name = self.format_path(path);
        self.read_compound_named(conn, path, wire_name, expected_size)
            .await
    }

    /// CREATE + READ + CLOSE on a name already in wire form: a file's
    /// (`format_path`), or a named stream's (`streams.rs`). `path` is what
    /// the caller asked for, kept for the log lines.
    pub(super) async fn read_compound_named(
        &self,
        conn: &mut Connection,
        path: &str,
        wire_name: String,
        expected_size: u64,
    ) -> Result<(Vec<u8>, FileInfo)> {
        let conn = &mut self.pinned_conn(conn);
        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);
        // Ask for what the caller expects, capped by what one READ can carry
        // and floored at one byte: a zero-length READ charges zero credits,
        // which is not a request the spec allows.
        let requested = expected_size.min(max_read as u64).max(1) as u32;
        trace!(
            "tree: read_file_compound path={path:?}, requested={requested}, max_read={max_read}"
        );

        // Build CREATE request (same params as open_file).
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            create_options: 0,
            name: wire_name,
            create_contexts: vec![],
        };

        // Build READ request with sentinel FileId. The credit charge follows
        // the expected response size (MS-SMB2 § 3.2.4.1.2), so a read that
        // asks for less pays less.
        let read_credit_charge = credits::charge_for_payload(requested as u64);
        let read_req = ReadRequest {
            padding: 0x50,
            flags: 0,
            length: requested,
            offset: 0,
            file_id: FileId::SENTINEL,
            minimum_count: 0,
            channel: SMB2_CHANNEL_NONE,
            remaining_bytes: 0,
            read_channel_info: vec![],
        };

        // Build CLOSE request with sentinel FileId.
        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        // Send as compound.
        let ops = [
            CompoundOp {
                command: Command::Create,
                body: &create_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::Read,
                body: &read_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(read_credit_charge),
            },
            CompoundOp {
                command: Command::Close,
                body: &close_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;

        let create_header = &responses[0].header;
        let create_body = &responses[0].body;
        let read_header = &responses[1].header;
        let read_body = &responses[1].body;
        let close_header = &responses[2].header;

        // Check CREATE response.
        if create_header.status != NtStatus::SUCCESS {
            // CREATE failed -- all three fail (cascaded). No handle to clean up.
            return Err(Error::Protocol {
                status: create_header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(create_body);
        let create_resp = CreateResponse::unpack(&mut cursor)?;
        let file_id = create_resp.file_id;

        // The READ can return at most `requested` bytes, so anything bigger
        // comes back truncated, and a file that is exactly `requested` bytes
        // of a larger whole is indistinguishable from a complete one. Compare
        // against what was asked for, never against `max_read`: a file that
        // grew past the caller's `expected_size` between its scan and this
        // read is the case that would otherwise hand back a short buffer that
        // looks complete. `end_of_file` is the server's authoritative size and
        // rides in this same frame, so the check is exact. The compound's
        // CLOSE already released the handle, so there's nothing to clean up.
        if create_resp.end_of_file > requested as u64 {
            debug!(
                "tree: read_file_compound file is {} bytes > the {}-byte read issued for it path={:?}",
                create_resp.end_of_file, requested, path
            );
            return Err(Error::FileTooLargeForSingleRead {
                size: create_resp.end_of_file,
                requested,
            });
        }

        // Check READ response.
        if read_header.status != NtStatus::SUCCESS && read_header.status != NtStatus::END_OF_FILE {
            // READ failed. CLOSE also failed in the compound (cascaded).
            // Issue a standalone CLOSE to clean up the handle.
            debug!(
                "tree: compound READ failed ({:?}), issuing standalone CLOSE",
                read_header.status
            );
            let _ = self.close_handle(conn, file_id).await;
            return Err(Error::Protocol {
                status: read_header.status,
                command: Command::Read,
            });
        }

        // Parse READ data.
        let data = if read_header.status == NtStatus::END_OF_FILE {
            // Empty file.
            Vec::new()
        } else {
            let mut cursor = ReadCursor::new(read_body);
            let read_resp = ReadResponse::unpack(&mut cursor)?;
            read_resp.data
        };

        // Check CLOSE response. If it failed but CREATE and READ succeeded,
        // the handle might still be open, but there's nothing we can do
        // since we already have the data.
        if close_header.status != NtStatus::SUCCESS {
            debug!(
                "tree: compound CLOSE returned {:?} (non-fatal, data already read)",
                close_header.status,
            );
        }

        trace!("tree: read_file_compound done, read {} bytes", data.len());
        Ok((data, file_info_from_create(&create_resp)))
    }

    /// Read a file's contents using a compound request (1 round-trip).
    ///
    /// Sends CREATE+READ+CLOSE as a single compound message. For files
    /// that fit in MaxReadSize (typically 8 MB), this is the fastest
    /// path -- 1 round-trip instead of 3+.
    ///
    /// A file larger than the server's `MaxReadSize` can't be returned by a
    /// single READ, so this fails with
    /// [`Error::FileTooLargeForSingleRead`]
    /// (classified [`ErrorKind::TooLarge`](crate::ErrorKind::TooLarge)) instead
    /// of silently returning only the first chunk. Reach for
    /// [`read_file_pipelined`](Self::read_file_pipelined), which reads any size
    /// with concurrent chunked READs.
    pub async fn read_file(&self, conn: &mut Connection, path: &str) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        self.read_file_compound(conn, path).await
    }

    /// Disconnect from the share.
    pub async fn disconnect(&self, conn: &mut Connection) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        trace!(
            "tree: disconnecting share={:?}, tree_id={}",
            self.share_name,
            self.tree_id
        );
        let body = TreeDisconnectRequest;
        let frame = conn
            .execute(Command::TreeDisconnect, &body, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::TreeDisconnect,
            });
        }

        conn.deregister_dfs_tree(self.tree_id);

        info!(
            "tree: disconnected share={:?}, tree_id={}",
            self.share_name, self.tree_id
        );
        Ok(())
    }

    /// Start watching a directory for changes.
    ///
    /// Opens the directory and returns a [`Watcher`](crate::client::watcher::Watcher) that yields change
    /// events via [`next_events()`](crate::client::watcher::Watcher::next_events).
    /// The server holds each request until changes occur, making this a
    /// long-poll operation.
    ///
    /// Set `recursive` to `true` to watch the entire subtree.
    ///
    /// The returned `Watcher` owns a cloned `Connection` (cheap
    /// `Arc::clone`, all clones multiplex over the same SMB session), so
    /// the caller is free to perform other operations on `conn` while
    /// watching. No second `SmbClient` is required.
    pub async fn watch(
        &self,
        conn: &mut Connection,
        path: &str,
        recursive: bool,
    ) -> Result<crate::client::watcher::Watcher> {
        let options = crate::client::watcher::WatchOptions::new().recursive(recursive);
        self.watch_with(conn, path, options).await
    }

    /// [`watch`](Self::watch) with [`WatchOptions`](crate::WatchOptions), which
    /// can also ask for xattr and named-stream changes. A server reports a
    /// stream's change under its file's name, or under `file:stream`; the notes
    /// have what each one does (`docs/notes/named-streams-and-xattrs.md`).
    pub async fn watch_with(
        &self,
        conn: &mut Connection,
        path: &str,
        options: crate::client::watcher::WatchOptions,
    ) -> Result<crate::client::watcher::Watcher> {
        let conn = &mut self.pinned_conn(conn);
        debug!(
            "tree: watch path={:?}, options={:?}, tree_id={}",
            path, options, self.tree_id
        );

        // Pinned before the open, so the watcher never subscribes on a
        // session other than the one its handle belongs to.
        let mut conn = conn.pinned_to_session();

        // Open the directory with FILE_LIST_DIRECTORY access (same as
        // FILE_READ_DATA = 0x0001). We need the handle to stay open for
        // the lifetime of the watcher.
        let file_id = self.open_directory(&mut conn, path).await?;

        // Hand the watcher an owned Tree clone and an owned Connection
        // clone so it can pipeline CHANGE_NOTIFY requests independently
        // of the caller's connection use.
        Ok(crate::client::watcher::Watcher::new(
            self.clone(),
            conn,
            file_id,
            options,
        ))
    }

    /// Delete a file using a compound request (1 round-trip).
    ///
    /// Sends CREATE + SET_INFO (delete disposition) + CLOSE as a single
    /// compound message. The server deletes the file when the CLOSE completes.
    ///
    /// The CREATE shares only delete access, so the server asks any client
    /// caching a handle under a lease to give it back first. macOS keeps a
    /// file open that way for about a minute after any app read it through
    /// the mount, and a delete that let that open stand left the file
    /// `STATUS_DELETE_PENDING` the whole time, so writing the same name again
    /// failed. When another client genuinely has the file open, the server
    /// refuses the exclusive open, and the delete goes ahead sharing
    /// everything: the file is gone for good once that client closes it.
    ///
    /// A symlink to a file is deleted itself, never its target (see
    /// [`delete_link`](Self::delete_link)).
    pub async fn delete_file(&self, conn: &mut Connection, path: &str) -> Result<()> {
        self.delete_exclusive_first(conn, path, FILE_NON_DIRECTORY_FILE, "file")
            .await
    }

    /// Delete a junction, symlink, or other link, leaving what it points to
    /// alone. One round trip, like [`delete_file`](Self::delete_file).
    ///
    /// Use it for an entry whose [`is_link`](DirectoryEntry::is_link) is set.
    /// A link isn't the same kind of entry everywhere: a junction or
    /// `mklink /D` symlink is a directory on Windows, while Samba reports a
    /// symlink as a file whichever way it points, so `delete_directory`
    /// refuses one there with `STATUS_NOT_A_DIRECTORY` (verified on Samba
    /// 4.23.10, 2026-10-20). This opens the reparse point itself
    /// (`FILE_OPEN_REPARSE_POINT`, MS-SMB2 § 2.2.13) with no file-or-directory
    /// condition, so it deletes the link whichever the server says it is.
    ///
    /// On a path that isn't a link it deletes a file, or a directory only if
    /// it's empty (`STATUS_DIRECTORY_NOT_EMPTY` otherwise), so a stale
    /// `is_link` can't take a folder's contents with it.
    pub async fn delete_link(&self, conn: &mut Connection, path: &str) -> Result<()> {
        self.delete_exclusive_first(conn, path, 0, "link").await
    }

    /// [`delete_compound`](Self::delete_compound) sharing only delete access,
    /// then sharing everything if another open refuses that (see
    /// [`delete_file`](Self::delete_file) for why).
    async fn delete_exclusive_first(
        &self,
        conn: &mut Connection,
        path: &str,
        type_option: u32,
        kind: &str,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let exclusive = self
            .delete_compound(
                conn,
                path,
                type_option,
                ShareAccess(ShareAccess::FILE_SHARE_DELETE),
                kind,
            )
            .await;
        match exclusive {
            // Windows also refuses an op that would go async mid-compound
            // (waiting on that lease break, here) with STATUS_INTERNAL_ERROR
            // (MS-SMB2 § 3.3.5.2.7). Either way the CREATE failed, so nothing
            // was opened and nothing changed: the shared retry is always safe.
            Err(e)
                if matches!(
                    e.status(),
                    Some(NtStatus::SHARING_VIOLATION | NtStatus::INTERNAL_ERROR)
                ) =>
            {
                debug!(
                    "tree: exclusive delete refused ({:?}), deleting shared path={:?}",
                    e.status(),
                    path
                );
                self.delete_compound(conn, path, type_option, SHARE_ALL, kind)
                    .await
            }
            other => other,
        }
    }

    /// Delete multiple files, one compound request each.
    ///
    /// Returns results in the same order as the input paths. Each file's
    /// result is independent: one failure does not affect the others.
    ///
    /// Each delete costs one round trip and they do not overlap, so this
    /// saves the per-call setup rather than the wire time. To overlap server
    /// work, run `delete_file` on several connections concurrently.
    pub async fn delete_files(&self, conn: &mut Connection, paths: &[&str]) -> Vec<Result<()>> {
        let conn = &mut self.pinned_conn(conn);
        if paths.is_empty() {
            return vec![];
        }

        debug!("tree: delete_files batch, count={}", paths.len());

        let mut results: Vec<Result<()>> = Vec::with_capacity(paths.len());
        for path in paths {
            results.push(self.delete_file(conn, path).await);
        }

        debug!(
            "tree: delete_files batch done, {}/{} succeeded",
            results.iter().filter(|r| r.is_ok()).count(),
            paths.len()
        );
        results
    }

    /// Get file metadata (size, timestamps, is_directory) using a compound request (1 round-trip).
    ///
    /// Sends CREATE + QUERY_INFO (FileBasicInformation) +
    /// QUERY_INFO (FileStandardInformation) + CLOSE as a single compound message.
    /// To also learn which file the path names, use
    /// [`stat_with_identity`](Self::stat_with_identity).
    pub async fn stat(&self, conn: &mut Connection, path: &str) -> Result<FileInfo> {
        let (info, _) = self.stat_compound(conn, path, false).await?;
        Ok(info)
    }

    /// [`stat`](Self::stat), plus which file the path names on the server:
    /// its [`FileIdentity`] (index number and volume serial), or `None` when
    /// the server won't say in full.
    ///
    /// Still one round trip: the two identity queries ride in the same
    /// compound, after the metadata ones, so a server that refuses them
    /// still answers the stat. Use it to compare two paths, or a path now and
    /// later, for "is this the same file?" (a rename keeps the identity, a
    /// replace changes it). It's the same identity a
    /// [`WriteCheckpoint`](crate::WriteCheckpoint) carries for the file it
    /// wrote. Plain `stat` doesn't ask, so it costs nothing extra.
    ///
    /// # Errors
    ///
    /// Whatever [`stat`](Self::stat) fails with. The identity never fails
    /// the call.
    pub async fn stat_with_identity(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<(FileInfo, Option<FileIdentity>)> {
        self.stat_compound(conn, path, true).await
    }

    /// The compound behind [`stat`](Self::stat) and
    /// [`stat_with_identity`](Self::stat_with_identity): CREATE, the two
    /// metadata queries, the identity queries when asked, CLOSE.
    async fn stat_compound(
        &self,
        conn: &mut Connection,
        path: &str,
        with_identity: bool,
    ) -> Result<(FileInfo, Option<FileIdentity>)> {
        let conn = &mut self.pinned_conn(conn);
        let normalized = self.format_path(path);
        trace!("tree: stat (compound) path={path:?}");

        // BUILD CREATE request for reading attributes.
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: SHARE_ALL,
            create_disposition: CreateDisposition::FileOpen,
            // Describe the link itself, the way its listing entry does. A
            // junction opened without it is its target, and a symlink is
            // refused with STATUS_STOPPED_ON_SYMLINK (MS-SMB2 § 3.3.5.9).
            create_options: FILE_OPEN_REPARSE_POINT,
            name: normalized,
            create_contexts: vec![],
        };

        // QUERY_INFO for FileBasicInformation (timestamps + attributes).
        let basic_req = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_BASIC_INFORMATION,
            output_buffer_length: 40,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };

        // QUERY_INFO for FileStandardInformation (size + is_directory).
        let std_req = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_STANDARD_INFORMATION,
            output_buffer_length: 24,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };

        let index_req = FileIdentity::index_query();
        let volume_req = FileIdentity::volume_query();

        // QUERY_INFO for FileAttributeTagInformation (which kind of reparse
        // point, if any).
        let tag_req = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_ATTRIBUTE_TAG_INFORMATION,
            output_buffer_length: 8,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };

        // CLOSE with sentinel FileId.
        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        let op = |command, body| CompoundOp {
            command,
            body,
            tree_id: Some(self.tree_id),
            credit_charge: CreditCharge(1),
        };
        // The optional queries go after the required ones (AGENTS.md pitfall
        // 31): a server that fails every related op after a failed one
        // (MS-SMB2 § 3.3.5.2.7.2) can then only take the CLOSE with them. The
        // tag goes last, so a server refusing it can't cost the identity a
        // caller asked for by name.
        let mut ops = vec![
            op(Command::Create, &create_req),
            op(Command::QueryInfo, &basic_req),
            op(Command::QueryInfo, &std_req),
        ];
        if with_identity {
            ops.push(op(Command::QueryInfo, &index_req));
            ops.push(op(Command::QueryInfo, &volume_req));
        }
        ops.push(op(Command::QueryInfo, &tag_req));
        ops.push(op(Command::Close, &close_req));

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        let (create, basic, std, close) = (
            &responses[0],
            &responses[1],
            &responses[2],
            &responses[responses.len() - 1],
        );

        // If CREATE failed, all ops cascade. No handle to clean up.
        if create.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create.header.status,
                command: Command::Create,
            });
        }

        // A CLOSE that didn't succeed left the handle open, most often because
        // a query ahead of it failed and the server cascaded that failure.
        if close.header.status != NtStatus::SUCCESS {
            let file_id = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?.file_id;
            debug!(
                "tree: stat's compound CLOSE returned {:?}, closing standalone",
                close.header.status
            );
            let _ = self.close_handle(conn, file_id).await;
        }

        for (frame, what) in [(basic, "basic"), (std, "standard")] {
            if !frame.header.status.is_success_or_partial() {
                warn!(
                    "tree: compound QUERY_INFO ({}) failed ({:?})",
                    what, frame.header.status
                );
                return Err(Error::Protocol {
                    status: frame.header.status,
                    command: Command::QueryInfo,
                });
            }
            if frame.header.status == NtStatus::BUFFER_OVERFLOW {
                warn!("recv: STATUS_BUFFER_OVERFLOW on QUERY_INFO ({what}), response data may be truncated");
            }
        }

        let mut info = file_info_from(&basic.body, &std.body)?;
        info.reparse_tag = reparse_tag_from(&responses[responses.len() - 2]);
        let identity = if with_identity {
            FileIdentity::from_frames(Some(&responses[3]), Some(&responses[4]))
                .filter(FileIdentity::is_proof)
        } else {
            None
        };

        trace!(
            "tree: stat done, size={}, is_dir={}, identity={:?}",
            info.size,
            info.is_directory,
            identity
        );
        Ok((info, identity))
    }

    /// Stat multiple files, one compound request each.
    ///
    /// Returns results in the same order as the input paths. Each path's
    /// result is independent: one failure does not affect the others.
    ///
    /// Each stat costs one round trip and they do not overlap, so this saves
    /// the per-call setup rather than the wire time. To overlap server work,
    /// run `stat` on several connections concurrently. To describe a whole
    /// directory, `list_directory` is far cheaper than a stat per entry.
    pub async fn stat_files(&self, conn: &mut Connection, paths: &[&str]) -> Vec<Result<FileInfo>> {
        let conn = &mut self.pinned_conn(conn);
        if paths.is_empty() {
            return vec![];
        }

        debug!("tree: stat_files batch, count={}", paths.len());

        let mut results: Vec<Result<FileInfo>> = Vec::with_capacity(paths.len());
        for path in paths {
            results.push(self.stat(conn, path).await);
        }

        debug!(
            "tree: stat_files batch done, {}/{} succeeded",
            results.iter().filter(|r| r.is_ok()).count(),
            paths.len()
        );
        results
    }

    /// Query file system space information for this share.
    ///
    /// Returns total capacity, free space, and allocation unit sizes.
    /// Uses a compound CREATE+QUERY_INFO+CLOSE for efficiency (one round-trip).
    pub async fn fs_info(&self, conn: &mut Connection) -> Result<FsInfo> {
        let conn = &mut self.pinned_conn(conn);
        trace!("tree: fs_info on share={:?}", self.share_name);

        // Build CREATE request to open the root directory of the share.
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            create_options: FILE_DIRECTORY_FILE,
            name: String::new(), // root of share
            create_contexts: vec![],
        };

        // Build QUERY_INFO request for FileFsFullSizeInformation.
        // Use sentinel FileId; the compound will fill it in.
        let query_req = QueryInfoRequest {
            info_type: InfoType::Filesystem,
            file_info_class: FILE_FS_FULL_SIZE_INFORMATION,
            output_buffer_length: 32, // 3 x i64 + 2 x u32
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };

        // Build CLOSE request with sentinel FileId.
        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        // Send as compound.
        let ops = [
            CompoundOp {
                command: Command::Create,
                body: &create_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::QueryInfo,
                body: &query_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::Close,
                body: &close_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;

        let create_header = &responses[0].header;
        let query_header = &responses[1].header;
        let query_body = &responses[1].body;
        let close_header = &responses[2].header;

        // Check CREATE response.
        if create_header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create_header.status,
                command: Command::Create,
            });
        }

        // Check QUERY_INFO response.
        if !query_header.status.is_success_or_partial() {
            // QUERY_INFO failed. Issue standalone CLOSE to clean up.
            let mut cursor = ReadCursor::new(&responses[0].body);
            let create_resp = CreateResponse::unpack(&mut cursor)?;
            debug!(
                "tree: compound QUERY_INFO failed ({:?}), issuing standalone CLOSE",
                query_header.status
            );
            let _ = self.close_handle(conn, create_resp.file_id).await;
            return Err(Error::Protocol {
                status: query_header.status,
                command: Command::QueryInfo,
            });
        }
        if query_header.status == NtStatus::BUFFER_OVERFLOW {
            warn!("recv: STATUS_BUFFER_OVERFLOW on FileFsFullSizeInformation, response data may be truncated");
        }

        // Parse the FileFsFullSizeInformation response.
        let mut cursor = ReadCursor::new(query_body);
        let query_resp = QueryInfoResponse::unpack(&mut cursor)?;
        let buf = &query_resp.output_buffer;

        if buf.len() < 32 {
            return Err(Error::invalid_data(format!(
                "FileFsFullSizeInformation too short: {} bytes",
                buf.len()
            )));
        }

        let total_allocation_units = i64::from_le_bytes(buf[0..8].try_into().unwrap()) as u64;
        let caller_available_units = i64::from_le_bytes(buf[8..16].try_into().unwrap()) as u64;
        let actual_available_units = i64::from_le_bytes(buf[16..24].try_into().unwrap()) as u64;
        let sectors_per_unit = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        let bytes_per_sector = u32::from_le_bytes(buf[28..32].try_into().unwrap());

        let bytes_per_unit = sectors_per_unit as u64 * bytes_per_sector as u64;
        let total_bytes = total_allocation_units * bytes_per_unit;
        let free_bytes = caller_available_units * bytes_per_unit;
        let total_free_bytes = actual_available_units * bytes_per_unit;

        // Check CLOSE response (non-fatal if it failed).
        if close_header.status != NtStatus::SUCCESS {
            debug!(
                "tree: compound CLOSE returned {:?} (non-fatal, fs_info already read)",
                close_header.status,
            );
        }

        trace!(
            "tree: fs_info done, total={}, free={}, total_free={}",
            total_bytes,
            free_bytes,
            total_free_bytes
        );
        Ok(FsInfo {
            total_bytes,
            free_bytes,
            total_free_bytes,
            bytes_per_sector,
            sectors_per_unit,
        })
    }

    /// Rename or move a file within the same share using a compound request (1 round-trip).
    ///
    /// Sends CREATE + SET_INFO (FileRenameInformation) + CLOSE as a single
    /// compound message. A name that's already taken is refused with
    /// `STATUS_OBJECT_NAME_COLLISION` ([`ErrorKind::AlreadyExists`](crate::ErrorKind::AlreadyExists))
    /// and neither file changes; [`rename_with`](Self::rename_with) can
    /// replace it instead.
    pub async fn rename(&self, conn: &mut Connection, from: &str, to: &str) -> Result<()> {
        self.rename_with(conn, from, to, RenameOptions::default())
            .await
    }

    /// Rename like [`rename`](Self::rename), with `options` deciding what
    /// happens to a file already at `to`.
    ///
    /// With [`RenameOptions::replace_if_exists`], the server swaps the source
    /// in under `to` in the same SET_INFO that renames it, so the name is
    /// never missing: write `file.tmp`, then rename it over `file`, and a
    /// reader sees either the old contents or the new ones. Nothing is
    /// emulated client-side; a delete-then-rename would leave a window where
    /// `file` is gone. (Samba has a much narrower one of its own: a client
    /// opening `file` at the very moment of the swap is occasionally told
    /// it's missing, because Samba's open looks the file up twice.)
    ///
    /// What a replacing rename can still be refused with:
    ///
    /// - **A directory at either end.** Only a file is replaced. Samba refuses
    ///   with `STATUS_OBJECT_NAME_COLLISION` ([`ErrorKind::AlreadyExists`](crate::ErrorKind::AlreadyExists)),
    ///   Windows with `STATUS_ACCESS_DENIED` (MS-FSA § 2.1.5.15.12), and
    ///   nothing moves.
    /// - **A destination someone has open, or one marked Read-only**:
    ///   Windows refuses with `STATUS_ACCESS_DENIED`. Samba 4.20 replaces it
    ///   anyway, open handles and leases included.
    ///
    /// Renaming a file onto its own name succeeds and changes nothing, and a
    /// rename that only changes case succeeds with or without the option.
    pub async fn rename_with(
        &self,
        conn: &mut Connection,
        from: &str,
        to: &str,
        options: RenameOptions,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let from_normalized = self.format_path(from);
        let to_normalized = normalize_path(to);
        trace!(
            "tree: rename (compound) from={from:?} to={to:?} replace_if_exists={}",
            options.replace_if_exists
        );

        // Build CREATE request with DELETE access (required for rename).
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::DELETE | FileAccessMask::FILE_READ_ATTRIBUTES,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            // Rename the link, not its target: a junction opened without it
            // is its target directory, which the rename would move.
            create_options: FILE_OPEN_REPARSE_POINT,
            name: from_normalized.clone(),
            create_contexts: vec![],
        };

        // Build SET_INFO request with FileRenameInformation and sentinel FileId.
        let setinfo_req = SetInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_RENAME_INFORMATION,
            additional_information: 0,
            file_id: FileId::SENTINEL,
            buffer: build_rename_info_buffer(&to_normalized, options.replace_if_exists),
        };

        // Build CLOSE request with sentinel FileId.
        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        let ops = [
            CompoundOp {
                command: Command::Create,
                body: &create_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::SetInfo,
                body: &setinfo_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::Close,
                body: &close_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;

        let create_header = &responses[0].header;
        let create_body = &responses[0].body;
        let setinfo_header = &responses[1].header;
        let close_header = &responses[2].header;

        // If CREATE failed, all ops cascade. No handle to clean up.
        if create_header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create_header.status,
                command: Command::Create,
            });
        }

        // CREATE succeeded. If SET_INFO failed, CLOSE also cascaded.
        // Issue standalone CLOSE to avoid leaking the handle.
        if setinfo_header.status != NtStatus::SUCCESS {
            let mut cursor = ReadCursor::new(create_body);
            let create_resp = CreateResponse::unpack(&mut cursor)?;
            warn!(
                "tree: compound SET_INFO failed ({:?}), issuing standalone CLOSE",
                setinfo_header.status
            );
            let _ = self.close_handle(conn, create_resp.file_id).await;
            return Err(Error::Protocol {
                status: setinfo_header.status,
                command: Command::SetInfo,
            });
        }

        // Check CLOSE response (non-fatal if it failed, rename already done).
        if close_header.status != NtStatus::SUCCESS {
            debug!(
                "tree: compound CLOSE returned {:?} (non-fatal, rename already done)",
                close_header.status,
            );
        }

        debug!("tree: renamed from={from:?} to={to:?}");
        Ok(())
    }

    /// Rename multiple files, one compound request each.
    ///
    /// Returns results in the same order as the input pairs. Each rename's
    /// result is independent: one failure does not affect the others.
    ///
    /// Each rename costs one round trip and they do not overlap, so this
    /// saves the per-call setup rather than the wire time. To overlap server
    /// work, run `rename` on several connections concurrently.
    pub async fn rename_files(
        &self,
        conn: &mut Connection,
        renames: &[(&str, &str)],
    ) -> Vec<Result<()>> {
        self.rename_files_with(conn, renames, RenameOptions::default())
            .await
    }

    /// Rename multiple files like [`rename_files`](Self::rename_files), each
    /// with the same `options` (see [`rename_with`](Self::rename_with)).
    pub async fn rename_files_with(
        &self,
        conn: &mut Connection,
        renames: &[(&str, &str)],
        options: RenameOptions,
    ) -> Vec<Result<()>> {
        let conn = &mut self.pinned_conn(conn);
        if renames.is_empty() {
            return vec![];
        }

        debug!("tree: rename_files batch, count={}", renames.len());

        let mut results: Vec<Result<()>> = Vec::with_capacity(renames.len());
        for (from, to) in renames {
            results.push(self.rename_with(conn, from, to, options).await);
        }

        debug!(
            "tree: rename_files batch done, {}/{} succeeded",
            results.iter().filter(|r| r.is_ok()).count(),
            renames.len()
        );
        results
    }

    /// Write a small file: CREATE + WRITE + FLUSH in one compound frame, then
    /// a standalone CLOSE (2 round trips).
    ///
    /// Best for files that fit in MaxWriteSize. For larger files, use
    /// [`write_file_pipelined`](Self::write_file_pipelined).
    ///
    /// The FLUSH ends its chain. A server may refuse an operation that goes
    /// async anywhere but last in a compound (MS-SMB2 § 3.3.5.2.7), a FLUSH
    /// is the one that does, and on Samba up to 4.15 that refusal also crashes
    /// the server's connection process. The data is flushed when this
    /// returns, and a refused FLUSH is an error (after the handle is closed).
    ///
    /// Dropping the future after the first frame leaves the handle open until
    /// the session ends, as with any multi-round-trip write.
    ///
    /// Creates the file, or REPLACES whatever holds the name (`FileOverwriteIf`).
    /// When the name must be new, use
    /// [`write_file_compound_exclusive`](Self::write_file_compound_exclusive).
    pub async fn write_file_compound(
        &self,
        conn: &mut Connection,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        self.write_file_compound_with_disposition(
            conn,
            path,
            data,
            CreateDisposition::FileOverwriteIf,
        )
        .await
    }

    /// Write a small NEW file: CREATE + WRITE + FLUSH in one compound frame,
    /// then a standalone CLOSE.
    ///
    /// The same write as
    /// [`write_file_compound`](Self::write_file_compound), with the CREATE
    /// asking for a new file (`FileCreate`): if the name already exists, the
    /// server refuses with `STATUS_OBJECT_NAME_COLLISION`, which surfaces as
    /// [`crate::ErrorKind::AlreadyExists`], and leaves the existing file
    /// untouched. The server decides it atomically, so a file another writer
    /// put at the name after the caller last looked is refused too, never
    /// replaced.
    ///
    /// The refusal is an [`Error::Protocol`] naming [`Command::Create`], so a
    /// caller can tell that nothing was opened or written, and there is nothing
    /// of its own to clean up at that name.
    pub async fn write_file_compound_exclusive(
        &self,
        conn: &mut Connection,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        self.write_file_compound_with_disposition(conn, path, data, CreateDisposition::FileCreate)
            .await
    }

    /// Shared body of [`write_file_compound`](Self::write_file_compound)
    /// (`FileOverwriteIf`) and
    /// [`write_file_compound_exclusive`](Self::write_file_compound_exclusive)
    /// (`FileCreate`). Private so the disposition stays a strict allow-list.
    async fn write_file_compound_with_disposition(
        &self,
        conn: &mut Connection,
        path: &str,
        data: &[u8],
        create_disposition: CreateDisposition,
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        // The CLOSE carries the handle the CREATE opens, so both go out pinned
        // to the session it opens in (pinned before the CREATE).
        let conn = &mut conn.pinned_to_session();
        let normalized = self.format_path(path);
        trace!(
            "tree: write_file_compound path={:?}, len={}, disposition={:?}",
            path,
            data.len(),
            create_disposition
        );

        // Build CREATE request (write access, the caller's disposition).
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(0),
            create_disposition,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: normalized.clone(),
            create_contexts: vec![],
        };

        // Build WRITE request with sentinel FileId.
        // DataOffset = Header::SIZE (64) + WriteRequest fixed body (48) = 0x70.
        let write_credit_charge = credits::charge_for_payload(data.len() as u64);
        let write_req = WriteRequest {
            data_offset: 0x70,
            offset: 0,
            file_id: FileId::SENTINEL,
            channel: 0,
            remaining_bytes: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
            flags: 0,
            data: data.to_vec(),
        };

        let flush_req = FlushRequest {
            file_id: FileId::SENTINEL,
        };

        // ❌ Nothing may follow the FLUSH: it's the op that goes async, and
        // only the last op in a chain may (AGENTS.md pitfall 33). The CLOSE
        // goes on its own.
        let ops = [
            CompoundOp {
                command: Command::Create,
                body: &create_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::Write,
                body: &write_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(write_credit_charge),
            },
            CompoundOp {
                command: Command::Flush,
                body: &flush_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;

        let create_header = &responses[0].header;
        let create_body = &responses[0].body;
        let write_header = &responses[1].header;
        let write_body = &responses[1].body;
        let flush_header = &responses[2].header;

        // Check CREATE response.
        if create_header.status != NtStatus::SUCCESS {
            // CREATE failed -- the rest fail too (cascaded). No handle to clean up.
            return Err(Error::Protocol {
                status: create_header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(create_body);
        let create_resp = CreateResponse::unpack(&mut cursor)?;
        let file_id = create_resp.file_id;

        // Check WRITE response.
        if write_header.status != NtStatus::SUCCESS {
            debug!(
                "tree: compound WRITE failed ({:?}), issuing standalone CLOSE",
                write_header.status
            );
            let _ = self.close_handle(conn, file_id).await;
            return Err(Error::Protocol {
                status: write_header.status,
                command: Command::Write,
            });
        }

        let mut cursor = ReadCursor::new(write_body);
        let write_resp = WriteResponse::unpack(&mut cursor)?;
        let bytes_written = write_resp.count as u64;

        if flush_header.status != NtStatus::SUCCESS {
            // Nothing vouches for the data, so the write isn't done. The
            // handle still has to go.
            let _ = self.close_handle(conn, file_id).await;
            return Err(Error::Protocol {
                status: flush_header.status,
                command: Command::Flush,
            });
        }
        if let Err(e) = self.close_handle(conn, file_id).await {
            // Written and flushed; the handle goes with the session at worst.
            debug!("tree: CLOSE after the compound write failed: {e} (data already flushed)");
        }

        debug!(
            "tree: write_file_compound done, wrote {} bytes",
            bytes_written
        );
        Ok(bytes_written)
    }

    /// Write data to a file (create or overwrite).
    ///
    /// For data that fits in MaxWriteSize (typically 64 KB to 8 MB), uses a
    /// compound CREATE+WRITE+FLUSH and a standalone CLOSE (2 round trips; see
    /// [`write_file_compound`](Self::write_file_compound)). For larger data,
    /// falls back to the pipelined write path.
    ///
    /// Returns the total number of bytes written.
    pub async fn write_file(&self, conn: &mut Connection, path: &str, data: &[u8]) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        // `max_write`, lowered to what the credit window funds.
        if data.len() as u64 <= conn.compound_write_limit() {
            self.write_file_compound(conn, path, data).await
        } else {
            self.write_file_pipelined(conn, path, data).await
        }
    }

    /// Read a file using pipelined I/O with a sliding window.
    ///
    /// Opens the file, determines its size, then uses a sliding window to
    /// keep the pipe full: as each response arrives, the next request is sent
    /// immediately. Much faster than sequential [`read_file`](Self::read_file)
    /// for large files.
    ///
    /// Uses 64 KB chunks with CreditCharge=1 to maximize concurrency.
    /// The window is capped at 32 in-flight requests (2 MB).
    pub async fn read_file_pipelined(&self, conn: &mut Connection, path: &str) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        // Every request after the open carries its handle, so all of them go
        // out pinned to the session that opened it.
        let conn = &mut conn.pinned_to_session();

        // Open the file.
        let (file_id, file_size) = self.open_file(conn, path).await?;

        if file_size == 0 {
            trace!(
                "tree: read_file_pipelined path={:?}, size=0 (empty file)",
                path
            );
            self.close_handle(conn, file_id).await?;
            return Ok(Vec::new());
        }

        // Balance chunk size for pipelining: small enough to keep many
        // in flight (sliding window benefit), large enough to minimize
        // per-chunk overhead (headers, signing).
        //
        // For files that fit in one read: use file size (no chunking).
        // For larger files: use 512 KB -- gives ~20 chunks per 10 MB
        // (enough for pipelining) with 8 credits per chunk (manageable).
        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);
        let pipeline_chunk = 512 * 1024_u32; // 512 KB
        let chunk_size = if file_size <= max_read as u64 {
            // File fits in one read -- no pipelining needed.
            (file_size as u32).min(max_read)
        } else {
            // Use pipeline chunk size, capped to MaxReadSize.
            pipeline_chunk.min(max_read)
        };
        let chunk_size = conn.fundable_chunk(chunk_size);
        let credit_charge = credits::charge_for_payload(chunk_size as u64);
        let total_chunks = file_size.div_ceil(chunk_size as u64) as usize;
        trace!(
            "tree: read_file_pipelined path={:?}, size={}, chunk_size={}, credit_charge={}, total_chunks={}, credits={}",
            path, file_size, chunk_size, credit_charge, total_chunks, conn.credits()
        );

        let start = std::time::Instant::now();
        let result = self
            .read_pipelined_loop(
                conn,
                file_id,
                file_size,
                chunk_size,
                credit_charge,
                total_chunks,
            )
            .await;

        // Close the handle regardless of read result.
        let close_result = self.close_handle(conn, file_id).await;

        let data = result?;
        close_result?;

        let elapsed = start.elapsed();
        let mb = data.len() as f64 / (1024.0 * 1024.0);
        let mbps = if elapsed.as_secs_f64() > 0.0 {
            mb / elapsed.as_secs_f64()
        } else {
            0.0
        };
        debug!(
            "tree: read_file_pipelined done, read {} bytes in {:.2?} ({:.1} MB/s)",
            data.len(),
            elapsed,
            mbps
        );

        Ok(data)
    }

    /// Read a file using pipelined I/O with progress reporting and cancellation.
    ///
    /// Same as [`read_file_pipelined`](Self::read_file_pipelined) but calls
    /// `on_progress` after each chunk is received. Return
    /// `ControlFlow::Break(())` from the callback to cancel the read.
    pub async fn read_file_pipelined_with_progress<F>(
        &self,
        conn: &mut Connection,
        path: &str,
        on_progress: F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let (file_id, file_size) = self.open_file(conn, path).await?;
        self.read_open_file_with_progress(conn, path, file_id, file_size, on_progress)
            .await
    }

    /// The rest of [`read_file_pipelined_with_progress`](Self::read_file_pipelined_with_progress),
    /// once the file is open: reads it all, reporting each chunk, then
    /// closes it. `path` is for the log.
    pub(crate) async fn read_open_file_with_progress<F>(
        &self,
        conn: &mut Connection,
        path: &str,
        file_id: FileId,
        file_size: u64,
        mut on_progress: F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        let conn = &mut self.pinned_conn(conn);
        // A no-op when the caller pinned before the open, as it should.
        let conn = &mut conn.pinned_to_session();
        if file_size == 0 {
            trace!(
                "tree: read_file_pipelined_with_progress path={:?}, size=0 (empty file)",
                path
            );
            self.close_handle(conn, file_id).await?;
            let _ = on_progress(Progress {
                bytes_transferred: 0,
                total_bytes: Some(0),
            });
            return Ok(Vec::new());
        }

        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);
        let pipeline_chunk = 512 * 1024_u32;
        let chunk_size = if file_size <= max_read as u64 {
            (file_size as u32).min(max_read)
        } else {
            pipeline_chunk.min(max_read)
        };
        let chunk_size = conn.fundable_chunk(chunk_size);
        let credit_charge = credits::charge_for_payload(chunk_size as u64);
        let total_chunks = file_size.div_ceil(chunk_size as u64) as usize;
        trace!(
            "tree: read_file_pipelined_with_progress path={:?}, size={}, chunk_size={}, total_chunks={}",
            path, file_size, chunk_size, total_chunks
        );

        let result = self
            .read_pipelined_loop_with_progress(
                conn,
                file_id,
                file_size,
                chunk_size,
                credit_charge,
                total_chunks,
                &mut on_progress,
            )
            .await;

        // Close the handle regardless of read result.
        let close_result = self.close_handle(conn, file_id).await;

        let data = result?;
        close_result?;

        debug!(
            "tree: read_file_pipelined_with_progress done, read {} bytes",
            data.len()
        );
        Ok(data)
    }

    /// Start a streaming file download on this tree.
    ///
    /// Issues CREATE and returns a [`FileDownload`] that pulls the body in
    /// chunks via [`next_chunk`](FileDownload::next_chunk). Mirrors
    /// [`SmbClient::download`](crate::SmbClient::download) but accepts a
    /// borrowed [`Connection`] directly, so callers who hold a cloned
    /// `Connection` (see [`Connection::clone`]) can drive concurrent
    /// downloads on one SMB session.
    ///
    /// Reads in [`DOWNLOAD_CHUNK_SIZE`](crate::DOWNLOAD_CHUNK_SIZE) chunks
    /// (512 KiB, or `MaxReadSize` if that's smaller) with an adaptive
    /// read-ahead window ([`ReadAhead::Adaptive`](crate::ReadAhead::Adaptive)):
    /// full speed on a fast link, and about one chunk queued ahead of anything
    /// else on a slow one. Chain
    /// [`with_read_ahead`](FileDownload::with_read_ahead) or
    /// [`with_chunk_size`](FileDownload::with_chunk_size) on the result to
    /// change either.
    ///
    /// For files that fit in one READ (≤ `max_read_size`), prefer
    /// [`read_file_compound`](Self::read_file_compound) — 1 RTT vs. 3 RTTs —
    /// when the link is fast. Its single READ is also the whole file queued
    /// ahead of everything else on the connection, which on a slow link means
    /// no progress and a long wait for any other request.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # async fn example(conn: &mut smb2::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let mut download = tree.download(conn, "big.bin").await?;
    /// while let Some(chunk) = download.next_chunk().await {
    ///     let bytes = chunk?;
    ///     // process bytes
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn download<'a>(
        &'a self,
        conn: &'a mut Connection,
        path: &str,
    ) -> Result<FileDownload<'a>> {
        let mut conn = conn.pinned_to_session();
        let (file_id, info) = self.open_for_read(&mut conn, path).await?;
        Ok(FileDownload::of_open_file(
            std::borrow::Cow::Borrowed(self),
            conn,
            file_id,
            info,
        ))
    }

    /// Write a file using pipelined I/O.
    ///
    /// Opens/creates the file, then streams it in
    /// [`UPLOAD_CHUNK_SIZE`](crate::UPLOAD_CHUNK_SIZE) WRITEs (512 KiB, or
    /// `MaxWriteSize` if that's smaller) with the adaptive write-behind window
    /// ([`WriteBehind::Adaptive`](crate::WriteBehind::Adaptive)): full speed on
    /// a fast link, and about one chunk queued ahead of anything else on the
    /// connection on a slow one. Flushes to ensure data is persisted on the
    /// server. For a file that fits one frame, [`write_file`](Self::write_file)
    /// costs one round trip instead.
    pub async fn write_file_pipelined(
        &self,
        conn: &mut Connection,
        path: &str,
        data: &[u8],
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        let normalized = self.format_path(path);

        if data.is_empty() {
            trace!("tree: write_file_pipelined path={path:?}, len=0 (empty write)");
            // Still create the file (to match write_file behavior).
            return self.write_file_compound(conn, path, data).await;
        }

        // Every request after the open carries its handle, so all of them go
        // out pinned to the session that opened it.
        let conn = &mut conn.pinned_to_session();

        // Open (or create) the file for writing.
        let req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(0),
            create_disposition: CreateDisposition::FileOverwriteIf,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: normalized.clone(),
            create_contexts: vec![],
        };

        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let create_resp = CreateResponse::unpack(&mut cursor)?;
        let file_id = create_resp.file_id;

        let max_write = conn.params().map(|p| p.max_write_size).unwrap_or(65536);
        let mut pipe = WritePipe::new(conn.clone(), self.tree_id, file_id, max_write);
        trace!(
            "tree: write_file_pipelined path={:?}, len={}, chunk_size={}, credits={}",
            path,
            data.len(),
            pipe.next_len(),
            conn.credits()
        );

        let start = std::time::Instant::now();
        let result = async {
            let mut sent = 0;
            while sent < data.len() {
                let len = (data.len() - sent).min(pipe.next_len() as usize);
                pipe.send(data[sent..sent + len].to_vec()).await?;
                sent += len;
            }
            pipe.drain().await?;
            Ok::<_, Error>(pipe.confirmed())
        }
        .await;

        // Flush to ensure data is persisted on the server.
        if result.is_ok() {
            self.flush_handle(conn, file_id).await?;
        }

        // Close the handle.
        let close_result = self.close_handle(conn, file_id).await;

        let bytes_written = result?;
        close_result?;

        let elapsed = start.elapsed();
        let mb = bytes_written as f64 / (1024.0 * 1024.0);
        let mbps = if elapsed.as_secs_f64() > 0.0 {
            mb / elapsed.as_secs_f64()
        } else {
            0.0
        };
        debug!(
            "tree: write_file_pipelined done, wrote {} bytes in {:.2?} ({:.1} MB/s)",
            bytes_written, elapsed, mbps
        );

        Ok(bytes_written)
    }

    /// Write a file from a streaming source using pipelined I/O.
    ///
    /// Pulls data on demand from a callback, so you never need the full
    /// file in memory. Ideal for writing from a network stream, a
    /// channel, or any producer that generates data incrementally.
    ///
    /// # Callback contract
    ///
    /// Each call to `next_chunk` must return one of:
    /// - `Some(Ok(data))` — the next chunk to write (any size; chunks
    ///   larger than `MaxWriteSize` are split automatically)
    /// - `Some(Err(e))` — an I/O error from the source; aborts the
    ///   write, drains in-flight responses, and propagates the error
    /// - `None` — end of stream; all remaining in-flight writes are
    ///   completed before returning
    ///
    /// An empty `Vec<u8>` in `Some(Ok(vec![]))` is treated the same as
    /// `None` (end of stream).
    ///
    /// # Behavior
    ///
    /// - Returns the total number of bytes the server acknowledged.
    /// - The file handle is always closed, even on error.
    /// - If `next_chunk` returns `None` on the first call, an empty file
    ///   is created.
    /// - On early termination (callback error or server error), a partial
    ///   file may remain on the server. The caller is responsible for
    ///   cleanup (for example, calling [`delete_file`](Self::delete_file)).
    ///
    /// # Performance
    ///
    /// Paces its WRITEs like [`write_file_pipelined`](Self::write_file_pipelined):
    /// 512 KiB chunks and the adaptive write-behind window, so throughput stays
    /// high on a high-latency link and a slow one isn't flooded. Memory is
    /// bounded by that window (at most 4 MiB in flight) plus one callback
    /// chunk, not by the file size.
    ///
    /// # When to use which write method
    ///
    /// | Method | Best for |
    /// |--------|----------|
    /// | [`write_file`](Self::write_file) | Small files that fit in a single compound (one round-trip) |
    /// | [`write_file_pipelined`](Self::write_file_pipelined) | Large files already in a `&[u8]` buffer |
    /// | `write_file_streamed` | Data produced incrementally (streams, channels, generators) |
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(tree: &smb2::client::Tree, conn: &mut smb2::client::Connection) -> smb2::Result<()> {
    /// let chunks = vec![b"hello ".to_vec(), b"world".to_vec()];
    /// let mut iter = chunks.into_iter();
    /// let mut next = || iter.next().map(Ok);
    ///
    /// let bytes_written = tree.write_file_streamed(conn, "greeting.txt", &mut next).await?;
    /// assert_eq!(bytes_written, 11);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_file_streamed<F>(
        &self,
        conn: &mut Connection,
        path: &str,
        next_chunk: &mut F,
    ) -> Result<u64>
    where
        F: FnMut() -> Option<std::result::Result<Vec<u8>, std::io::Error>>,
    {
        let conn = &mut self.pinned_conn(conn);
        trace!("tree: write_file_streamed path={path:?}");
        // Every request after the open carries its handle, so all of them go
        // out pinned to the session that opened it.
        let conn = &mut conn.pinned_to_session();

        // Open (or create) the file for writing.
        let file_id = self.open_file_for_write(conn, path).await?;
        self.write_open_file_streamed(conn, file_id, next_chunk)
            .await
    }

    /// The rest of [`write_file_streamed`](Self::write_file_streamed), once
    /// the file is open: pulls and writes every chunk, flushes, and closes.
    pub(crate) async fn write_open_file_streamed<F>(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        next_chunk: &mut F,
    ) -> Result<u64>
    where
        F: FnMut() -> Option<std::result::Result<Vec<u8>, std::io::Error>>,
    {
        let conn = &mut self.pinned_conn(conn);
        // A no-op when the caller pinned before the open, as it should.
        let conn = &mut conn.pinned_to_session();
        let max_write = conn.params().map(|p| p.max_write_size).unwrap_or(65536);
        let mut pipe = WritePipe::new(conn.clone(), self.tree_id, file_id, max_write);

        let start = std::time::Instant::now();
        let result = write_streamed(&mut pipe, next_chunk).await;

        // Flush to ensure data is persisted on the server.
        if result.is_ok() {
            self.flush_handle(conn, file_id).await?;
        }

        // Close the handle (always, even on error).
        let close_result = self.close_handle(conn, file_id).await;

        let bytes_written = result?;
        close_result?;

        let elapsed = start.elapsed();
        let mb = bytes_written as f64 / (1024.0 * 1024.0);
        let mbps = if elapsed.as_secs_f64() > 0.0 {
            mb / elapsed.as_secs_f64()
        } else {
            0.0
        };
        debug!(
            "tree: write_file_streamed done, wrote {} bytes in {:.2?} ({:.1} MB/s)",
            bytes_written, elapsed, mbps
        );

        Ok(bytes_written)
    }

    /// Open a random-access [`FileReader`](super::stream::FileReader) that owns
    /// its `Connection` and `Arc<Tree>`.
    ///
    /// Opens the file for reading and hands back a reader that serves any number
    /// of positioned reads at arbitrary offsets over the one open handle, then
    /// [`close`](super::stream::FileReader::close)s. The returned reader is
    /// `'static` — multiple readers built from clones of the same `Connection`
    /// pipeline their READs over a single SMB session.
    pub async fn open_file_reader(
        self: &Arc<Self>,
        conn: Connection,
        path: &str,
    ) -> Result<super::stream::FileReader> {
        let conn = self.pinned_conn(&conn);
        super::stream::open_file_reader(Arc::clone(self), conn, path).await
    }

    /// Create a push-based pipelined streaming writer that owns its
    /// `Connection` and `Arc<Tree>`.
    ///
    /// Opens (or creates) the file for writing and returns a
    /// [`FileWriter`](super::stream::FileWriter) that accepts pushed
    /// chunks. The caller drives writes at their own pace and calls
    /// [`FileWriter::finish`](super::stream::FileWriter::finish) to
    /// flush and close.
    ///
    /// The returned writer is `'static` — multiple writers built from
    /// clones of the same `Connection` pipeline their WRITEs over a
    /// single SMB session without external locking.
    pub async fn create_file_writer(
        self: &Arc<Self>,
        conn: Connection,
        path: &str,
    ) -> Result<super::stream::FileWriter> {
        let conn = self.pinned_conn(&conn);
        super::stream::open_file_writer(Arc::clone(self), conn, path).await
    }

    /// Open a push-based pipelined file writer with **exclusive-create**
    /// semantics. Same shape as [`Tree::create_file_writer`], but the CREATE
    /// uses `FileCreate` disposition: if the file already exists the open
    /// fails with [`crate::ErrorKind::AlreadyExists`]
    /// instead of truncating it.
    ///
    /// Use this when the consumer needs a race-free "create only if absent"
    /// write — for example, a file manager's "New File" action where
    /// silently clobbering an existing file is unsafe.
    ///
    /// The returned writer is `'static` and behaves identically to
    /// `create_file_writer` from there on; chunks are pipelined over the
    /// shared SMB session.
    pub async fn create_file_writer_exclusive(
        self: &Arc<Self>,
        conn: Connection,
        path: &str,
    ) -> Result<super::stream::FileWriter> {
        let conn = self.pinned_conn(&conn);
        super::stream::open_file_writer_exclusive(Arc::clone(self), conn, path).await
    }

    /// Open a push-based pipelined file writer positioned at `offset`. Same
    /// shape as [`Tree::create_file_writer`], but the file is opened without
    /// truncating (`FileOpenIf`) and the writer's first byte lands at `offset`.
    ///
    /// The natural way to append after a server-side-copied prefix
    /// ([`server_side_copy_range`](Self::server_side_copy_range)) or to patch a
    /// known region of an existing file.
    ///
    /// To carry on an interrupted upload, use
    /// [`resume_file_writer`](Self::resume_file_writer) instead: this one
    /// trusts that the file is still the one you were writing.
    pub async fn create_file_writer_at(
        self: &Arc<Self>,
        conn: Connection,
        path: &str,
        offset: u64,
    ) -> Result<super::stream::FileWriter> {
        let conn = self.pinned_conn(&conn);
        super::stream::open_file_writer_at(Arc::clone(self), conn, path, offset).await
    }

    /// Carry on an interrupted upload from its checkpoint, once the server
    /// has proven the file is still the one the checkpoint names.
    ///
    /// See [`open_file_writer_resume`](super::stream::open_file_writer_resume)
    /// for what is checked and what each refusal means.
    pub async fn resume_file_writer(
        self: &Arc<Self>,
        conn: Connection,
        path: &str,
        checkpoint: &super::stream::WriteCheckpoint,
    ) -> Result<super::stream::FileWriter> {
        let conn = self.pinned_conn(&conn);
        super::stream::open_file_writer_resume(Arc::clone(self), conn, path, checkpoint).await
    }

    /// Set the length of an open file (SET_INFO `FileEndOfFileInformation`,
    /// MS-FSCC § 2.4.14), cutting off or zero-extending its end.
    pub(crate) async fn set_end_of_file(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        length: u64,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        trace!("tree: setting end of file to {length}");
        let req = SetInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_END_OF_FILE_INFORMATION,
            additional_information: 0,
            file_id,
            buffer: length.to_le_bytes().to_vec(),
        };
        let frame = conn
            .execute(Command::SetInfo, &req, Some(self.tree_id))
            .await?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::SetInfo,
            });
        }
        Ok(())
    }

    /// Create a directory.
    ///
    /// Opens the path with `FileCreate` disposition and `FILE_DIRECTORY_FILE`
    /// option, then immediately closes the handle.
    pub async fn create_directory(&self, conn: &mut Connection, path: &str) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let normalized = self.format_path(path);
        trace!("tree: create_directory path={path:?}");

        let req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::DIRECTORY,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileCreate,
            create_options: FILE_DIRECTORY_FILE,
            name: normalized.clone(),
            create_contexts: vec![],
        };

        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let create_resp = CreateResponse::unpack(&mut cursor)?;
        let file_id = create_resp.file_id;

        // Close the handle immediately.
        self.close_handle(conn, file_id).await?;
        debug!("tree: created directory path={path:?}");
        Ok(())
    }

    /// Delete a directory using a compound request (1 round-trip).
    ///
    /// Sends CREATE (with `DELETE_ON_CLOSE`) + CLOSE as a single compound
    /// message. The directory must be empty.
    ///
    /// A junction or directory symlink is deleted itself, never its target
    /// (see [`delete_link`](Self::delete_link), which also covers a server
    /// that reports a link as a file).
    pub async fn delete_directory(&self, conn: &mut Connection, path: &str) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        self.delete_compound(conn, path, FILE_DIRECTORY_FILE, SHARE_ALL, "directory")
            .await
    }

    // ── Private helpers ──────────────────────────────────────────────

    /// Compound CREATE + SET_INFO (FileDispositionInformation) + CLOSE in a
    /// single round-trip.
    ///
    /// `type_option` selects file vs directory (`FILE_NON_DIRECTORY_FILE`
    /// or `FILE_DIRECTORY_FILE`, or 0 for either), and `share_access` is what the CREATE lets
    /// other opens keep (see `delete_file`). `kind` is used only for log
    /// messages.
    ///
    /// Don't switch this back to `FILE_DELETE_ON_CLOSE`. Samba accepts a
    /// delete-on-close CREATE against a non-empty directory, answers both
    /// CREATE and CLOSE with `STATUS_SUCCESS`, and then doesn't delete
    /// anything — so the caller is told a delete happened that never did.
    /// Setting the disposition explicitly makes the server validate while it
    /// still has somewhere to put the error, and it comes back as
    /// `STATUS_DIRECTORY_NOT_EMPTY`.
    ///
    /// A CLOSE answered `STATUS_UNSUCCESSFUL` gets one more try after
    /// [`DELETE_RETRY_PAUSE`]. That's a QNAP with its recycle bin on, where
    /// deletes on several connections race to create the same `@Recycle`
    /// folder and the losers' files stay in place (verified on QuTS hero with
    /// Samba 4.20.0 + QNAP `ksmbd`, 2026-10-22,
    /// `docs/notes/2026-10-22-rm-close-unsuccessful.md`). The failed attempt
    /// sends its own standalone CLOSE for the handle QNAP leaves open, so the
    /// retry is a fresh delete, and dropping the future during the pause
    /// leaves nothing behind. Any other status, or a second failure, is
    /// returned as is.
    async fn delete_compound(
        &self,
        conn: &mut Connection,
        path: &str,
        type_option: u32,
        share_access: ShareAccess,
        kind: &str,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        match self
            .delete_compound_once(conn, path, type_option, share_access, kind)
            .await
        {
            Err(Error::Protocol {
                status: NtStatus::UNSUCCESSFUL,
                command: Command::Close,
            }) => {
                debug!("tree: delete_{kind} CLOSE was unsuccessful, retrying once path={path:?}");
                rt::sleep(DELETE_RETRY_PAUSE).await;
                self.delete_compound_once(conn, path, type_option, share_access, kind)
                    .await
            }
            other => other,
        }
    }

    /// One attempt of [`delete_compound`](Self::delete_compound).
    async fn delete_compound_once(
        &self,
        conn: &mut Connection,
        path: &str,
        type_option: u32,
        share_access: ShareAccess,
        kind: &str,
    ) -> Result<()> {
        let normalized = self.format_path(path);
        trace!("tree: delete_{kind} (compound) path={path:?}");

        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::DELETE | FileAccessMask::FILE_READ_ATTRIBUTES,
            ),
            file_attributes: 0,
            share_access,
            create_disposition: CreateDisposition::FileOpen,
            // The link itself, never its target: a junction opened without
            // it is its target directory, which the delete would remove.
            create_options: type_option | FILE_OPEN_REPARSE_POINT,
            name: normalized.clone(),
            create_contexts: vec![],
        };

        let disposition_req = SetInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_DISPOSITION_INFORMATION,
            additional_information: 0,
            file_id: FileId::SENTINEL,
            buffer: vec![1], // DeletePending = true (MS-FSCC 2.4.11).
        };

        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        let ops = [
            CompoundOp {
                command: Command::Create,
                body: &create_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::SetInfo,
                body: &disposition_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
            CompoundOp {
                command: Command::Close,
                body: &close_req,
                tree_id: Some(self.tree_id),
                credit_charge: CreditCharge(1),
            },
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;

        let create_header = &responses[0].header;
        let create_body = &responses[0].body;
        let setinfo_header = &responses[1].header;
        let close_header = &responses[2].header;

        // If CREATE failed, all ops in the compound fail (cascaded). No handle to clean up.
        if create_header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create_header.status,
                command: Command::Create,
            });
        }

        // CREATE succeeded, so a handle exists. Any later failure has to close
        // it by hand: a cascaded CLOSE fails along with the op before it and
        // leaves the handle open on the server.
        for (header, command) in [
            (setinfo_header, Command::SetInfo),
            (close_header, Command::Close),
        ] {
            if header.status != NtStatus::SUCCESS {
                let mut cursor = ReadCursor::new(create_body);
                let create_resp = CreateResponse::unpack(&mut cursor)?;
                warn!(
                    "tree: compound {:?} failed ({:?}), issuing standalone CLOSE",
                    command, header.status
                );
                let _ = self.close_handle(conn, create_resp.file_id).await;
                return Err(Error::Protocol {
                    status: header.status,
                    command,
                });
            }
        }

        debug!("tree: deleted {kind} path={path:?}");
        Ok(())
    }

    /// Open a directory handle.
    async fn open_directory(&self, conn: &mut Connection, path: &str) -> Result<FileId> {
        let conn = &mut self.pinned_conn(conn);
        let path = self.format_path(path);
        let req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            create_options: FILE_DIRECTORY_FILE,
            name: path,
            create_contexts: vec![],
        };

        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = CreateResponse::unpack(&mut cursor)?;
        Ok(resp.file_id)
    }

    /// Open a file handle for reading and return the file ID and size.
    ///
    /// Sends a single CREATE with read access, `FileOpen` disposition (fail
    /// if absent), and the standard share mask. Returns the server's
    /// [`FileId`] plus the file's size in bytes (end-of-file offset) so
    /// callers can size their read loop.
    ///
    /// Most callers want [`read_file_compound`](Self::read_file_compound),
    /// [`read_file_pipelined`](Self::read_file_pipelined), or
    /// [`download`](Self::download), which all open the file, read it, and
    /// close it in one call. Use `open_file` directly when you want to build
    /// a custom read loop — for example, constructing a [`FileDownload`]
    /// with a non-default `chunk_size` via
    /// [`FileDownload::new`](crate::client::stream::FileDownload::new).
    ///
    /// The caller is responsible for closing the handle when done (either
    /// by handing it to a [`FileDownload`], which closes on completion or
    /// drop, or by calling the internal close path). Leaking the handle
    /// wastes server resources.
    pub async fn open_file(&self, conn: &mut Connection, path: &str) -> Result<(FileId, u64)> {
        let conn = &mut self.pinned_conn(conn);
        let (file_id, info) = self.open_for_read(conn, path).await?;
        Ok((file_id, info.size))
    }

    /// [`open_file`](Self::open_file), keeping everything the CREATE response
    /// said about the file.
    pub(crate) async fn open_for_read(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<(FileId, FileInfo)> {
        let conn = &mut self.pinned_conn(conn);
        let req = self.read_open_request(path);
        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = CreateResponse::unpack(&mut cursor)?;
        Ok((resp.file_id, file_info_from_create(&resp)))
    }

    /// The CREATE [`open_file`](Self::open_file) and a
    /// [`FileReader`](crate::FileReader) send: read access, `FileOpen`
    /// (fail if absent), and the standard share mask.
    pub(super) fn read_open_request(&self, path: &str) -> CreateRequest {
        CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            create_options: 0,
            name: self.format_path(path),
            create_contexts: vec![],
        }
    }

    /// Open (or create) a file for writing, returning the file handle.
    ///
    /// Uses `FileOverwriteIf` disposition (create if absent, overwrite if present)
    /// and requests write access. Used by [`FileUpload`](crate::client::stream::FileUpload).
    pub(crate) async fn open_file_for_write(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<FileId> {
        let conn = &mut self.pinned_conn(conn);
        let req = self.write_open_request(path, CreateDisposition::FileOverwriteIf);
        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = CreateResponse::unpack(&mut cursor)?;
        Ok(resp.file_id)
    }

    /// The CREATE every write open sends, with the disposition chosen by the
    /// caller: `FileOverwriteIf` (create or truncate), `FileCreate` (exclusive,
    /// refused with `STATUS_OBJECT_NAME_COLLISION` if the name exists, which
    /// surfaces as [`crate::ErrorKind::AlreadyExists`]), or `FileOpenIf` (open
    /// without truncating, for a positioned writer). Kept `pub(super)` so the
    /// disposition stays an allow-list inside the crate.
    pub(super) fn write_open_request(
        &self,
        path: &str,
        create_disposition: CreateDisposition,
    ) -> CreateRequest {
        CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(0),
            create_disposition,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: self.format_path(path),
            create_contexts: vec![],
        }
    }

    /// Open (or create) a file with combined read+write access, **without**
    /// truncating it, returning the handle and its current size.
    ///
    /// Requests `FILE_READ_DATA | FILE_WRITE_DATA` with `FileOpenIf`
    /// disposition (open if present, create if absent) and preserves existing
    /// content. Use it to open a destination for the low-level server-side copy
    /// primitives ([`copy_chunks`](Self::copy_chunks),
    /// [`server_side_copy_range`](Self::server_side_copy_range)) — those need
    /// the destination handle to carry read access as well as write for
    /// [`FSCTL_SRV_COPYCHUNK`](crate::msg::ioctl::FSCTL_SRV_COPYCHUNK) (MS-SMB2
    /// 3.3.5.15.6). Close it with [`close_handle`](Self::close_handle) when done.
    pub async fn open_file_readwrite(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<(FileId, u64)> {
        let conn = &mut self.pinned_conn(conn);
        self.open_readwrite_with_disposition(conn, path, CreateDisposition::FileOpenIf)
            .await
    }

    /// Read+write open with `FileOverwriteIf` (truncating). Used by the
    /// whole-file [`server_side_copy_file`](Self::server_side_copy_file), where
    /// the destination becomes an exact copy of the source.
    pub(crate) async fn open_readwrite_overwrite(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<(FileId, u64)> {
        let conn = &mut self.pinned_conn(conn);
        self.open_readwrite_with_disposition(conn, path, CreateDisposition::FileOverwriteIf)
            .await
    }

    /// Shared body for the read+write opens. Requests read+write data access so
    /// the handle works as a server-side copy destination.
    async fn open_readwrite_with_disposition(
        &self,
        conn: &mut Connection,
        path: &str,
        create_disposition: CreateDisposition,
    ) -> Result<(FileId, u64)> {
        let conn = &mut self.pinned_conn(conn);
        let path = self.format_path(path);
        let req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: path,
            create_contexts: vec![],
        };

        let frame = conn
            .execute(Command::Create, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Create,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = CreateResponse::unpack(&mut cursor)?;
        Ok((resp.file_id, resp.end_of_file))
    }

    /// Default per-QUERY_DIRECTORY output-buffer length.
    ///
    /// Capped at 65536 so that CreditCharge=1 is valid: the spec requires
    /// CreditCharge = 1 + (OutputBufferLength - 1) / 65536 for multi-credit
    /// dialects, and 65536 keeps it at 1 while still holding plenty of entries.
    fn default_query_buffer_len(conn: &Connection) -> u32 {
        conn.params()
            .map(|p| p.max_transact_size.min(65536))
            .unwrap_or(65536)
    }

    /// Issue one QUERY_DIRECTORY round trip and parse its reply.
    ///
    /// The one wire implementation of a listing's queries: every
    /// [`DirectoryReader`] drives this, and [`list_directory`](Self::list_directory)
    /// and [`list_directory_instrumented`](Self::list_directory_instrumented)
    /// collect through one.
    ///
    /// `class` is one of the two listing classes `msg::directory_info` speaks.
    ///
    /// `output_buffer_length` above 65536 needs a matching multi-credit charge;
    /// this computes it as ceil(len / 65536).
    pub(crate) async fn query_directory_step(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        restart: bool,
        output_buffer_length: u32,
        class: FileInformationClass,
    ) -> Result<QueryStepOutcome> {
        let conn = &mut self.pinned_conn(conn);
        let req = QueryDirectoryRequest {
            file_information_class: class,
            flags: QueryDirectoryFlags(if restart {
                QueryDirectoryFlags::RESTART_SCANS
            } else {
                0
            }),
            file_index: 0,
            file_id,
            output_buffer_length,
            file_name: "*".to_string(),
        };
        let credit_charge = CreditCharge(credits::charge_for_payload(output_buffer_length as u64));

        let frame = conn
            .execute_with_credits(
                Command::QueryDirectory,
                &req,
                Some(self.tree_id),
                credit_charge,
            )
            .await?;

        if frame.header.status == NtStatus::NO_MORE_FILES {
            return Ok(QueryStepOutcome::NoMoreFiles {
                bytes: frame.body.len(),
            });
        }

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::QueryDirectory,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = QueryDirectoryResponse::unpack(&mut cursor)?;
        let bytes = resp.output_buffer.len();

        let entries = parse_directory_entries(class, &resp.output_buffer)?;
        for e in &entries {
            trace!(
                "tree: dir_entry name={:?}, size={}, is_dir={}",
                e.name,
                e.size,
                e.is_directory
            );
        }
        Ok(QueryStepOutcome::Entries { entries, bytes })
    }

    /// Read file data in chunks.
    #[allow(dead_code)] // Will be used by read_file_pipelined for large-file chunked reads.
    async fn read_loop(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        file_size: u64,
    ) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        let max_read = conn.params().map(|p| p.max_read_size).unwrap_or(65536);

        let mut data = Vec::with_capacity(file_size as usize);
        let mut offset = 0u64;

        loop {
            let remaining = file_size.saturating_sub(offset);
            if remaining == 0 {
                break;
            }

            let chunk_size = remaining.min(max_read as u64) as u32;

            let req = ReadRequest {
                padding: 0x50,
                flags: 0,
                length: chunk_size,
                offset,
                file_id,
                minimum_count: 0,
                channel: SMB2_CHANNEL_NONE,
                remaining_bytes: 0,
                read_channel_info: vec![],
            };

            let frame = conn
                .execute(Command::Read, &req, Some(self.tree_id))
                .await?;

            // STATUS_END_OF_FILE means we read past the end.
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

            offset += resp.data.len() as u64;
            data.extend_from_slice(&resp.data);
        }

        Ok(data)
    }

    /// Pipelined read using a sliding window.
    ///
    /// Instead of batch send/receive phases, each received response
    /// immediately triggers the next send. The pipe stays full at all times,
    /// eliminating idle gaps between batches.
    async fn read_pipelined_loop(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        file_size: u64,
        chunk_size: u32,
        credit_charge: u16,
        total_chunks: usize,
    ) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        use futures_util::stream::{FuturesUnordered, StreamExt};

        let mut data = vec![0u8; file_size as usize];
        let mut chunks_sent = 0usize;
        let mut chunks_received = 0usize;

        // How many requests to keep queued, not how many the credit budget
        // allows: `Connection` reserves credits per send and parks a request
        // that can't afford one, so throttling here as well could only
        // under-send.
        let initial_window = total_chunks.min(MAX_PIPELINE_WINDOW);

        trace!(
            "tree: pipeline read sliding window: initial_window={}, total_chunks={}, credits={}",
            initial_window,
            total_chunks,
            conn.credits()
        );

        // Spawn each chunk read as an independent `execute_with_credits`
        // future. `FuturesUnordered` polls them concurrently — the actor-
        // based receiver task routes responses by `MessageId`, so all
        // chunks compete fairly even when they arrive out of order.
        let mut in_flight = FuturesUnordered::new();
        let build_req = |chunk_index: usize| -> ReadRequest {
            let offset = chunk_index as u64 * chunk_size as u64;
            let this_chunk = if chunk_index == total_chunks - 1 {
                (file_size - offset) as u32
            } else {
                chunk_size
            };
            ReadRequest {
                padding: 0x50,
                flags: 0,
                length: this_chunk,
                offset,
                file_id,
                minimum_count: 0,
                channel: SMB2_CHANNEL_NONE,
                remaining_bytes: 0,
                read_channel_info: vec![],
            }
        };
        let launch_chunk = |conn: &Connection, chunk_index: usize, tree_id: TreeId| -> _ {
            let c = conn.clone();
            let req = build_req(chunk_index);
            async move {
                let frame = c
                    .execute_with_credits(
                        Command::Read,
                        &req,
                        Some(tree_id),
                        CreditCharge(credit_charge),
                    )
                    .await;
                (chunk_index, frame)
            }
        };

        for _ in 0..initial_window {
            in_flight.push(launch_chunk(conn, chunks_sent, self.tree_id));
            chunks_sent += 1;
        }

        while chunks_received < total_chunks {
            let Some((chunk_index, frame_result)) = in_flight.next().await else {
                break;
            };
            chunks_received += 1;
            let frame = frame_result?;

            if frame.header.status == NtStatus::END_OF_FILE {
                // File is shorter than expected. Keep draining but don't
                // launch more.
                continue;
            }

            if frame.header.status != NtStatus::SUCCESS {
                return Err(Error::Protocol {
                    status: frame.header.status,
                    command: Command::Read,
                });
            }

            let mut cursor = ReadCursor::new(&frame.body);
            let resp = ReadResponse::unpack(&mut cursor)?;

            if !resp.data.is_empty() {
                let dest_offset = chunk_index as u64 * chunk_size as u64;
                let dest_end = (dest_offset as usize + resp.data.len()).min(data.len());
                let src_len = dest_end - dest_offset as usize;
                data[dest_offset as usize..dest_end].copy_from_slice(&resp.data[..src_len]);
            }

            if chunks_sent < total_chunks {
                in_flight.push(launch_chunk(conn, chunks_sent, self.tree_id));
                chunks_sent += 1;
            }
        }

        Ok(data)
    }

    /// Pipelined read with progress callback and cancellation.
    ///
    /// Same sliding window as `read_pipelined_loop`, but calls `on_progress`
    /// after each chunk. Returns `Error::Cancelled` if the callback breaks.
    async fn read_pipelined_loop_with_progress<F>(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        file_size: u64,
        chunk_size: u32,
        credit_charge: u16,
        total_chunks: usize,
        on_progress: &mut F,
    ) -> Result<Vec<u8>>
    where
        F: FnMut(Progress) -> ControlFlow<()>,
    {
        let conn = &mut self.pinned_conn(conn);
        use futures_util::stream::{FuturesUnordered, StreamExt};

        let mut data = vec![0u8; file_size as usize];
        let mut chunks_sent = 0usize;
        let mut chunks_received = 0usize;
        let mut bytes_received = 0u64;

        // How many requests to keep queued, not how many the credit budget
        // allows: `Connection` reserves credits per send and parks a request
        // that can't afford one, so throttling here as well could only
        // under-send.
        let initial_window = total_chunks.min(MAX_PIPELINE_WINDOW);

        let mut in_flight = FuturesUnordered::new();
        let build_req = |chunk_index: usize| -> ReadRequest {
            let offset = chunk_index as u64 * chunk_size as u64;
            let this_chunk = if chunk_index == total_chunks - 1 {
                (file_size - offset) as u32
            } else {
                chunk_size
            };
            ReadRequest {
                padding: 0x50,
                flags: 0,
                length: this_chunk,
                offset,
                file_id,
                minimum_count: 0,
                channel: SMB2_CHANNEL_NONE,
                remaining_bytes: 0,
                read_channel_info: vec![],
            }
        };
        let launch_chunk = |conn: &Connection, chunk_index: usize, tree_id: TreeId| {
            let c = conn.clone();
            let req = build_req(chunk_index);
            async move {
                let frame = c
                    .execute_with_credits(
                        Command::Read,
                        &req,
                        Some(tree_id),
                        CreditCharge(credit_charge),
                    )
                    .await;
                (chunk_index, frame)
            }
        };

        for _ in 0..initial_window {
            in_flight.push(launch_chunk(conn, chunks_sent, self.tree_id));
            chunks_sent += 1;
        }

        while chunks_received < total_chunks {
            let Some((chunk_index, frame_result)) = in_flight.next().await else {
                break;
            };
            chunks_received += 1;
            let frame = frame_result?;

            if frame.header.status == NtStatus::END_OF_FILE {
                continue;
            }

            if frame.header.status != NtStatus::SUCCESS {
                return Err(Error::Protocol {
                    status: frame.header.status,
                    command: Command::Read,
                });
            }

            let mut cursor = ReadCursor::new(&frame.body);
            let resp = ReadResponse::unpack(&mut cursor)?;

            if !resp.data.is_empty() {
                let dest_offset = chunk_index as u64 * chunk_size as u64;
                let dest_end = (dest_offset as usize + resp.data.len()).min(data.len());
                let src_len = dest_end - dest_offset as usize;
                data[dest_offset as usize..dest_end].copy_from_slice(&resp.data[..src_len]);
                bytes_received += src_len as u64;
            }

            let progress = Progress {
                bytes_transferred: bytes_received,
                total_bytes: Some(file_size),
            };
            if let ControlFlow::Break(()) = on_progress(progress) {
                return Err(Error::Cancelled);
            }

            if chunks_sent < total_chunks {
                in_flight.push(launch_chunk(conn, chunks_sent, self.tree_id));
                chunks_sent += 1;
            }
        }

        Ok(data)
    }

    /// Flush a file handle to ensure data is persisted on the server.
    ///
    /// Sends an SMB2 FLUSH request and waits for the server to confirm
    /// that all cached data has been written to persistent storage.
    pub(crate) async fn flush_handle(&self, conn: &mut Connection, file_id: FileId) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        trace!("tree: flushing file handle");
        let req = FlushRequest { file_id };

        let frame = conn
            .execute(Command::Flush, &req, Some(self.tree_id))
            .await?;

        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Flush,
            });
        }

        Ok(())
    }

    /// Close an open file handle, releasing it on the server.
    ///
    /// Pairs with the raw-handle opens ([`open_file`](Self::open_file),
    /// [`open_file_readwrite`](Self::open_file_readwrite)): a handle you opened
    /// and did not hand to a streaming type (which close themselves) must be
    /// closed with this, or it leaks server-side until session teardown.
    pub async fn close_handle(&self, conn: &mut Connection, file_id: FileId) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        // A no-op for the handles that never held an oplock, which is nearly
        // all of them. Done before the CLOSE so a break arriving in the gap
        // isn't acknowledged on a handle that is on its way out.
        conn.forget_oplock(file_id);
        let req = CloseRequest { flags: 0, file_id };

        let frame = conn
            .execute(Command::Close, &req, Some(self.tree_id))
            .await?;
        close_outcome(&frame)
    }

    /// Put a CLOSE on the wire and hand back its guard, without waiting for
    /// the answer. The server closes the handle on receipt whether or not
    /// anyone collects the response, so dropping the guard is a valid way to
    /// be done with it. Collect it with [`close_outcome`].
    pub(crate) async fn dispatch_close(
        &self,
        conn: &Connection,
        file_id: FileId,
    ) -> Result<WaiterGuard> {
        let conn = &self.pinned_conn(conn);
        // See `close_handle`.
        conn.forget_oplock(file_id);
        let req = CloseRequest { flags: 0, file_id };
        conn.dispatch(Command::Close, &req, Some(self.tree_id))
            .await
    }

    /// Close `file_id` for a handle type whose owner dropped it still open
    /// (a `FileWriter` dropped without `finish` or `abort`, say). Called from
    /// `Drop`, so it never blocks and never panics.
    ///
    /// The CLOSE is queued before this returns, so it reaches the server
    /// after every request the handle already sent and before anything the
    /// owner sends next: delete the file right after dropping its writer and
    /// the server has closed it first. Only when that can't happen without
    /// waiting (no credit on hand, or the writer's queue full) is the CLOSE
    /// sent from a background task instead, which gives no such order; and
    /// if there's no async runtime to run that task on, the handle stays open
    /// until the session ends, as it would have before this existed.
    ///
    /// ❌ Callers must not call this for a handle whose CLOSE was already
    /// started, even one whose future was dropped before it could have gone
    /// out: the server may have handed the same file id to a new open since,
    /// and a second CLOSE would close that one. A handle from a session that
    /// has ended sends nothing; the server let go of it with the session.
    pub(crate) fn close_dropped_handle(
        &self,
        conn: &Connection,
        file_id: FileId,
        owner: &'static str,
    ) {
        let conn = self.pinned_conn(conn);
        if !conn.pinned_session_is_current() {
            return;
        }
        // See `close_handle`.
        conn.forget_oplock(file_id);
        let req = CloseRequest { flags: 0, file_id };
        let tree_id = self.tree_id;
        match conn.dispatch_without_waiting(Command::Close, &req, Some(tree_id)) {
            Ok(Some(queued)) => {
                trace!("{owner} dropped while open; its CLOSE is queued");
                // Only to hear the answer. With no runtime to wait on, the
                // CLOSE goes out all the same and its answer is discarded.
                if let Some(task) = rt::try_spawn(async move {
                    match queued
                        .response()
                        .await
                        .and_then(|frame| close_outcome(&frame))
                    {
                        Ok(()) | Err(Error::HandleInvalidated { .. }) => {}
                        Err(e) => debug!("{owner} dropped while open; its CLOSE failed: {e}"),
                    }
                }) {
                    task.detach();
                }
            }
            Ok(None) => {
                let task = rt::try_spawn(async move {
                    let result = match conn.dispatch(Command::Close, &req, Some(tree_id)).await {
                        Ok(mut guard) => conn
                            .await_response_in_place(&mut guard, Command::Close)
                            .await
                            .and_then(|frame| close_outcome(&frame)),
                        Err(e) => Err(e),
                    };
                    match result {
                        Ok(()) | Err(Error::HandleInvalidated { .. }) => {}
                        Err(e) => debug!(
                            "{owner} dropped while open; its CLOSE failed, so the handle stays \
                             open on the server until the session ends: {e}"
                        ),
                    }
                });
                match task {
                    Some(task) => task.detach(),
                    None => debug!(
                        "{owner} dropped while open, outside an async runtime and with no credit \
                         on hand to queue a CLOSE; the handle stays open on the server until the \
                         session ends"
                    ),
                }
            }
            Err(Error::HandleInvalidated { .. }) => {}
            Err(e) => debug!(
                "{owner} dropped while open; no CLOSE could be sent, so the handle stays open on \
                 the server until the session ends: {e}"
            ),
        }
    }
}

/// What a CLOSE response says about the close.
pub(crate) fn close_outcome(frame: &Frame) -> Result<()> {
    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Close,
        });
    }
    Ok(())
}

/// Build a FILE_RENAME_INFORMATION_TYPE_2 buffer (MS-FSCC 2.4.42.2).
fn build_rename_info_buffer(new_name: &str, replace_if_exists: bool) -> Vec<u8> {
    let name_u16: Vec<u16> = new_name.encode_utf16().collect();
    let name_byte_len = name_u16.len() * 2;

    let mut buf = Vec::with_capacity(20 + name_byte_len);
    buf.push(u8::from(replace_if_exists)); // ReplaceIfExists
    buf.extend_from_slice(&[0u8; 7]); // Reserved
    buf.extend_from_slice(&0u64.to_le_bytes()); // RootDirectory
    buf.extend_from_slice(&(name_byte_len as u32).to_le_bytes()); // FileNameLength
    for &u in &name_u16 {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    buf
}

/// Turn a caller's path into the wire path SMB2 wants.
///
/// `/` is the separator, `\` is a name character, and any character SMB2
/// refuses is mapped into the private-use area per component. See
/// [`crate::name`] for the table and why the mapping is unconditional.
fn normalize_path(path: &str) -> String {
    crate::name::encode_path(path)
}

/// Build a [`FileInfo`] from the QUERY_INFO response bodies for
/// `FileBasicInformation` and `FileStandardInformation`. Shared by
/// [`Tree::stat`] and [`Tree::resolve`], which ask the same two questions.
pub(super) fn file_info_from(basic_body: &[u8], std_body: &[u8]) -> Result<FileInfo> {
    let basic_resp = QueryInfoResponse::unpack(&mut ReadCursor::new(basic_body))?;
    let basic_buf = &basic_resp.output_buffer;
    if basic_buf.len() < 36 {
        return Err(Error::invalid_data(format!(
            "FileBasicInformation too short: {} bytes",
            basic_buf.len()
        )));
    }
    let created = FileTime(u64::from_le_bytes(basic_buf[0..8].try_into().unwrap()));
    let accessed = FileTime(u64::from_le_bytes(basic_buf[8..16].try_into().unwrap()));
    let modified = FileTime(u64::from_le_bytes(basic_buf[16..24].try_into().unwrap()));
    let changed = FileTime(u64::from_le_bytes(basic_buf[24..32].try_into().unwrap()));
    let attributes = FileAttributes::new(u32::from_le_bytes(basic_buf[32..36].try_into().unwrap()));

    let std_resp = QueryInfoResponse::unpack(&mut ReadCursor::new(std_body))?;
    let std_buf = &std_resp.output_buffer;
    if std_buf.len() < 22 {
        return Err(Error::invalid_data(format!(
            "FileStandardInformation too short: {} bytes",
            std_buf.len()
        )));
    }
    let end_of_file = u64::from_le_bytes(std_buf[8..16].try_into().unwrap());
    let is_directory = std_buf[21] != 0 || attributes.is_directory();

    Ok(FileInfo {
        size: end_of_file,
        is_directory,
        created,
        modified,
        accessed,
        changed,
        attributes,
        reparse_tag: None,
    })
}

/// The tag a `FileAttributeTagInformation` answer carries, `None` when the
/// server refused it or the file isn't a reparse point. The tag counts only
/// beside the answer's own REPARSE_POINT bit (MS-FSCC § 2.4.6).
fn reparse_tag_from(frame: &Frame) -> Option<ReparseTag> {
    if frame.header.status != NtStatus::SUCCESS {
        return None;
    }
    let resp = QueryInfoResponse::unpack(&mut ReadCursor::new(&frame.body)).ok()?;
    let buf = resp.output_buffer.get(..8)?;
    let attributes = FileAttributes::new(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
    let tag = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    attributes.is_reparse_point().then_some(ReparseTag(tag))
}

/// The [`FileInfo`] a CREATE response already answers: every open reports the
/// file's size, attributes, and times, so a read that opened the file needs no
/// `stat` to know them.
pub(crate) fn file_info_from_create(resp: &CreateResponse) -> FileInfo {
    let attributes = FileAttributes::new(resp.file_attributes);
    FileInfo {
        size: resp.end_of_file,
        is_directory: attributes.is_directory(),
        created: resp.creation_time,
        modified: resp.last_write_time,
        accessed: resp.last_access_time,
        changed: resp.change_time,
        attributes,
        reparse_tag: None,
    }
}

/// The entries in a QUERY_DIRECTORY output buffer of `class`, as callers see
/// them. Both listing classes come through here, so a name maps back the same
/// way whichever one carried it.
pub(crate) fn parse_directory_entries(
    class: FileInformationClass,
    data: &[u8],
) -> Result<Vec<DirectoryEntry>> {
    Ok(directory_info::parse_entries(class, data)?
        .into_iter()
        .map(|entry| {
            let attributes = FileAttributes::new(entry.file_attributes);
            DirectoryEntry {
                // A single component, so `decode_name`, not `decode_path`: a
                // `\` that comes back here is a character in the name (see
                // `crate::name`).
                name: crate::name::decode_name(&entry.file_name).into_owned(),
                size: entry.end_of_file,
                is_directory: attributes.is_directory(),
                created: entry.creation_time,
                modified: entry.last_write_time,
                changed: entry.change_time,
                attributes,
                // 0 is "this file system has none" (MS-FSCC § 2.4.22), and
                // what a server MAY send for `..`.
                index_number: entry.file_id.filter(|&id| id != 0),
                // EaSize carries the tag when the reparse bit is set
                // (MS-FSCC § 2.4.8), and the EAs' size otherwise.
                reparse_tag: attributes
                    .is_reparse_point()
                    .then_some(ReparseTag(entry.ea_size)),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::connection::pack_message;
    use crate::client::test_helpers::{
        build_close_response, build_create_error_response, build_create_response,
        build_file_both_dir_info, build_query_directory_response, build_tree_connect_response,
        setup_connection, setup_connection_with_max_read, setup_connection_without_credits,
    };
    use crate::msg::create::{CreateAction, CreateResponse};
    use crate::msg::header::Header;
    use crate::msg::tree_connect::ShareType;
    use crate::transport::MockTransport;
    use crate::types::status::NtStatus;
    use crate::types::{Command, TreeId};
    use std::sync::Arc;

    /// The DFS prefix carries the WHOLE host, IPv6 literals included.
    ///
    /// MS-SMB2 § 3.2.4.3: the server strips exactly two components off this to
    /// get the local path. Reading `[fe80::1]:445` as `fe80` left one host
    /// group where a host belonged, so the server's second strip ate the
    /// caller's first path component and opened the wrong file. Invisible on
    /// IPv4, which is every fixture we have.
    #[test]
    fn a_dfs_prefix_keeps_a_whole_ipv6_host() {
        let tree = |server: &str| Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "projects".to_string(),
            server: server.to_string(),
            is_dfs: true,
            encrypt_data: false,
            dfs_origin: None,
        };

        assert_eq!(
            tree("[fe80::1]:445").format_path("Docs/Report.pdf"),
            r"fe80::1\projects\Docs\Report.pdf"
        );
        assert_eq!(
            tree("fe80::1:445").format_path("Docs/Report.pdf"),
            r"fe80::1\projects\Docs\Report.pdf"
        );
        assert_eq!(
            tree("corp.example.com:445").format_path("Docs/Report.pdf"),
            r"corp.example.com\projects\Docs\Report.pdf"
        );
        // The share root, where there is no path to eat into.
        assert_eq!(tree("[::1]:445").format_path(""), r"::1\projects");
        // A non-DFS tree sends a share-relative path and no prefix at all.
        let mut plain = tree("[::1]:445");
        plain.is_dfs = false;
        assert_eq!(plain.format_path("Docs/Report.pdf"), r"Docs\Report.pdf");
    }

    fn build_flush_response() -> Vec<u8> {
        let mut h = Header::new_request(Command::Flush);
        h.flags.set_response();
        h.credits = 32;

        let body = crate::msg::flush::FlushResponse;
        pack_message(&h, &body)
    }

    fn build_read_response(status: NtStatus, data: Vec<u8>) -> Vec<u8> {
        let mut h = Header::new_request(Command::Read);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;

        if status == NtStatus::END_OF_FILE {
            use crate::msg::header::ErrorResponse;
            let body = ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            };
            return pack_message(&h, &body);
        }

        let body = ReadResponse {
            data_offset: 0x50,
            data_remaining: 0,
            flags: 0,
            data,
        };

        pack_message(&h, &body)
    }

    #[tokio::test]
    async fn tree_connect_stores_tree_id() {
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(42);
        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Disk));

        let mut conn = setup_connection(&mock);
        let tree = Tree::connect(&mut conn, "naspi").await.unwrap();
        assert_eq!(tree.tree_id, tree_id);
        assert_eq!(tree.share_name, "naspi");
    }

    #[tokio::test]
    async fn nothing_but_negotiate_can_send_before_the_server_grants_a_credit() {
        let mock = Arc::new(MockTransport::new());
        // No response queued: the point is that nothing gets asked at all.
        let mut conn = setup_connection_without_credits(&mock);

        let attempt = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Tree::connect(&mut conn, "naspi"),
        )
        .await
        .expect("an unfunded request must fail fast, not park on the wire");

        assert_eq!(
            mock.sent_count(),
            0,
            "a request the server funded no credit for must never reach the \
             wire: the server silently discards it and the client then waits \
             out a response deadline for an answer that is never coming"
        );
        assert!(
            matches!(attempt, Err(Error::CreditStarvation { .. })),
            "the caller gets a bounded error instead, got {:?}",
            attempt.map(|_| "a tree")
        );
    }

    #[tokio::test]
    async fn tree_connect_sends_unc_path() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_tree_connect_response(TreeId(1), ShareType::Disk));

        let mut conn = setup_connection(&mock);
        let _tree = Tree::connect(&mut conn, "myshare").await.unwrap();

        // Verify the sent request contains the UNC path.
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = TreeConnectRequest::unpack(&mut cursor).unwrap();
        assert_eq!(req.path, r"\\test-server\myshare");
    }

    #[tokio::test]
    async fn list_directory_returns_entries() {
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(10);
        let file_id = FileId {
            persistent: 0x1111,
            volatile: 0x2222,
        };

        // Build two directory entries.
        let entry1 = build_file_both_dir_info("file1.txt", 1024, false, 0);
        let total_entry_len = entry1.len();
        let entry1_with_next =
            build_file_both_dir_info("file1.txt", 1024, false, total_entry_len as u32);
        let entry2 = build_file_both_dir_info("subdir", 0, true, 0);

        let mut entries_data = entry1_with_next;
        entries_data.extend_from_slice(&entry2);

        // Queue: CREATE response, QUERY_DIRECTORY response (with data), QUERY_DIRECTORY response (no more), CLOSE response.
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_query_directory_response(
            NtStatus::SUCCESS,
            entries_data,
        ));
        mock.queue_response(build_query_directory_response(
            NtStatus::NO_MORE_FILES,
            vec![],
        ));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let entries = tree.list_directory(&mut conn, "somedir").await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file1.txt");
        assert_eq!(entries[0].size, 1024);
        assert!(!entries[0].is_directory);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_directory);
    }

    #[tokio::test]
    async fn list_directory_instrumented_reports_phase_breakdown() {
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(10);
        let file_id = FileId {
            persistent: 0x1111,
            volatile: 0x2222,
        };

        let entry1 = build_file_both_dir_info("file1.txt", 1024, false, 0);
        let entry1_with_next =
            build_file_both_dir_info("file1.txt", 1024, false, entry1.len() as u32);
        let entry2 = build_file_both_dir_info("subdir", 0, true, 0);
        let mut entries_data = entry1_with_next;
        entries_data.extend_from_slice(&entry2);
        let payload_len = entries_data.len();

        // CREATE, one QUERY with entries, one QUERY NO_MORE_FILES, CLOSE.
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_query_directory_response(
            NtStatus::SUCCESS,
            entries_data,
        ));
        mock.queue_response(build_query_directory_response(
            NtStatus::NO_MORE_FILES,
            vec![],
        ));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let (entries, trace) = tree
            .list_directory_instrumented(&mut conn, "somedir", None)
            .await
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(trace.entries, 2);

        // Two QUERY round trips: the data reply, then the terminal NO_MORE_FILES.
        assert_eq!(trace.queries.len(), 2);
        assert_eq!(trace.queries[0].entries, 2);
        assert_eq!(trace.queries[0].bytes, payload_len);
        assert!(!trace.queries[0].no_more_files);
        assert_eq!(trace.queries[1].entries, 0);
        assert!(trace.queries[1].no_more_files);

        // CREATE + 2 QUERY + CLOSE.
        assert_eq!(trace.round_trips(), 4);
        // total() sums the phases; query_total() only the QUERY round trips.
        assert_eq!(
            trace.total(),
            trace.create + trace.query_total() + trace.close
        );
    }

    #[tokio::test]
    async fn read_file_returns_data() {
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(20);
        let file_id = FileId {
            persistent: 0x3333,
            volatile: 0x4444,
        };
        let file_data = b"Hello, SMB world!";

        // Queue a single compound response frame: CREATE + READ + CLOSE.
        let create_resp = build_create_response(file_id, file_data.len() as u64);
        let read_resp = build_read_response(NtStatus::SUCCESS, file_data.to_vec());
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree.read_file(&mut conn, "test.txt").await.unwrap();
        assert_eq!(data, file_data);
    }

    #[tokio::test]
    async fn read_file_errors_when_larger_than_single_read() {
        // The mock negotiates a 64 KiB MaxReadSize (see setup_connection). A
        // file bigger than that can't come back in one READ, so read_file must
        // fail with a typed error rather than silently returning the first
        // 64 KiB. Pre-fix this returned a truncated buffer.
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(21);
        let file_id = FileId {
            persistent: 0x55,
            volatile: 0x66,
        };
        let big_size = 100_000u64; // > 65536

        // Even though we error on size, the compound still carried READ + CLOSE
        // sub-responses; queue a well-formed frame so routing is realistic.
        let create_resp = build_create_response(file_id, big_size);
        let read_resp = build_read_response(NtStatus::SUCCESS, vec![0u8; 65536]);
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let err = tree.read_file(&mut conn, "big.bin").await.unwrap_err();
        assert!(
            matches!(
                err,
                Error::FileTooLargeForSingleRead {
                    size: 100_000,
                    requested: 65536
                }
            ),
            "expected FileTooLargeForSingleRead, got {err:?}"
        );
        assert_eq!(err.kind(), crate::ErrorKind::TooLarge);
    }

    #[tokio::test]
    async fn normalize_path_converts_slashes() {
        assert_eq!(normalize_path("foo/bar/baz"), "foo\\bar\\baz");
        assert_eq!(normalize_path("/leading/slash"), "leading\\slash");
        assert_eq!(normalize_path("no_change"), "no_change");
        // A `\` is a name character now, not a second separator.
        assert_eq!(
            normalize_path("\\leading\\backslash"),
            "\u{F026}leading\u{F026}backslash"
        );
    }

    #[tokio::test]
    async fn illegal_characters_are_mapped_into_the_private_use_area() {
        // The exact name that a QNAP Samba share rejected with
        // STATUS_OBJECT_NAME_INVALID before the mapping existed.
        assert_eq!(
            normalize_path("dir/\"how_are_you_feeling?\"_emojis.json"),
            "dir\\\u{F020}how_are_you_feeling\u{F025}\u{F020}_emojis.json"
        );
        // A backslash inside a name is a name character, not a separator.
        assert_eq!(normalize_path("a\\b"), "a\u{F026}b");
        // The trailing rule applies per component, to the last character only.
        assert_eq!(normalize_path("dir /file. "), "dir\u{F028}\\file.\u{F028}");
    }

    #[tokio::test]
    async fn directory_listings_decode_private_use_area_names() {
        let data = build_file_both_dir_info("a\u{F025}b", 7, false, 0);
        let entries =
            parse_directory_entries(FileInformationClass::FileBothDirectoryInformation, &data)
                .unwrap();
        assert_eq!(entries[0].name, "a?b");
    }

    /// One listed entry with `attributes` and `ea_size` in the slot MS-FSCC
    /// § 2.4.8 says carries the reparse tag when REPARSE_POINT is set.
    fn listed_with(class: FileInformationClass, attributes: u32, ea_size: u32) -> DirectoryEntry {
        let data = directory_info::pack_entries(
            class,
            &[directory_info::DirectoryInfoEntry {
                file_attributes: attributes,
                ea_size,
                file_name: "entry".to_string(),
                ..Default::default()
            }],
        )
        .unwrap();
        parse_directory_entries(class, &data).unwrap().remove(0)
    }

    #[tokio::test]
    async fn a_listed_junction_carries_the_mount_point_tag_in_both_classes() {
        for class in [
            FileInformationClass::FileBothDirectoryInformation,
            FileInformationClass::FileIdBothDirectoryInformation,
        ] {
            let entry = listed_with(
                class,
                FileAttributes::DIRECTORY | FileAttributes::REPARSE_POINT,
                0xA000_0003,
            );
            assert_eq!(
                entry.reparse_tag,
                Some(ReparseTag::MOUNT_POINT),
                "{class:?}"
            );
            // Still a directory by its attributes, but not one to walk into.
            assert!(entry.is_directory);
            assert!(entry.is_link());
        }
    }

    #[tokio::test]
    async fn a_listed_cloud_placeholder_carries_its_tag_and_is_not_a_link() {
        let entry = listed_with(
            FileInformationClass::FileBothDirectoryInformation,
            FileAttributes::ARCHIVE | FileAttributes::REPARSE_POINT,
            0x9000_101A,
        );
        assert_eq!(entry.reparse_tag, Some(ReparseTag(0x9000_101A)));
        assert!(entry.reparse_tag.unwrap().is_cloud());
        assert!(!entry.is_link());
    }

    #[tokio::test]
    async fn an_entry_without_the_reparse_bit_has_no_tag_whatever_its_ea_size() {
        // EaSize is the extended attributes' size then, not a tag.
        let entry = listed_with(
            FileInformationClass::FileBothDirectoryInformation,
            FileAttributes::DIRECTORY,
            0xA000_0003,
        );
        assert_eq!(entry.reparse_tag, None);
        assert!(!entry.is_link());
    }

    #[tokio::test]
    async fn format_path_prepends_dfs_prefix() {
        let tree = Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "dfs".to_string(),
            server: "server1".to_string(),
            is_dfs: true,
            encrypt_data: false,
            dfs_origin: None,
        };
        assert_eq!(
            tree.format_path("data/hello.txt"),
            "server1\\dfs\\data\\hello.txt"
        );
        assert_eq!(tree.format_path(""), "server1\\dfs");
        assert_eq!(
            tree.format_path("nested/path"),
            "server1\\dfs\\nested\\path"
        );
    }

    #[tokio::test]
    async fn format_path_strips_port_from_dfs_prefix() {
        let tree = Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "dfs".to_string(),
            server: "server1:10456".to_string(),
            is_dfs: true,
            encrypt_data: false,
            dfs_origin: None,
        };
        assert_eq!(
            tree.format_path("data/hello.txt"),
            "server1\\dfs\\data\\hello.txt"
        );
    }

    #[tokio::test]
    async fn format_path_no_prefix_when_not_dfs() {
        let tree = Tree {
            pin: None,
            tree_id: TreeId(1),
            share_name: "public".to_string(),
            server: "server1".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };
        assert_eq!(tree.format_path("data/hello.txt"), "data\\hello.txt");
        assert_eq!(tree.format_path(""), "");
    }

    #[tokio::test]
    async fn parse_file_both_dir_info_single_entry() {
        let data = build_file_both_dir_info("test.txt", 42, false, 0);
        let entries =
            parse_directory_entries(FileInformationClass::FileBothDirectoryInformation, &data)
                .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "test.txt");
        assert_eq!(entries[0].size, 42);
        assert!(!entries[0].is_directory);
        assert_eq!(entries[0].modified, FileTime(133_000_000_000_000_000));
        assert_eq!(entries[0].changed, FileTime(133_100_000_000_000_000));
    }

    #[test]
    fn a_listing_entry_carries_its_attributes() {
        let mut data = build_file_both_dir_info(".secret", 1, false, 0);
        // FileAttributes sits after NextEntryOffset, FileIndex, four times,
        // EndOfFile, and AllocationSize: byte 56.
        let attrs = FileAttributes::HIDDEN | FileAttributes::READONLY | FileAttributes::ARCHIVE;
        data[56..60].copy_from_slice(&attrs.to_le_bytes());

        let entries =
            parse_directory_entries(FileInformationClass::FileBothDirectoryInformation, &data)
                .unwrap();
        assert_eq!(entries[0].attributes, FileAttributes::new(attrs));
        assert!(entries[0].attributes.is_hidden());
        assert!(entries[0].attributes.is_readonly());
        assert!(!entries[0].is_directory);

        let dir = parse_directory_entries(
            FileInformationClass::FileBothDirectoryInformation,
            &build_file_both_dir_info("d", 0, true, 0),
        )
        .unwrap();
        assert!(dir[0].is_directory && dir[0].attributes.is_directory());
    }

    #[test]
    fn a_create_response_hands_out_the_attributes_it_carries() {
        use crate::msg::create::CreateAction;
        let resp = CreateResponse {
            oplock_level: OplockLevel::None,
            flags: 0,
            create_action: CreateAction::FileOpened,
            creation_time: FileTime(1),
            last_access_time: FileTime(2),
            last_write_time: FileTime(3),
            change_time: FileTime(4),
            allocation_size: 0,
            end_of_file: 7,
            file_attributes: FileAttributes::HIDDEN | FileAttributes::READONLY,
            file_id: FileId::default(),
            create_contexts: vec![],
        };
        let info = file_info_from_create(&resp);
        assert!(info.attributes.is_hidden() && info.attributes.is_readonly());
        assert!(!info.is_directory);
    }

    #[tokio::test]
    async fn tree_disconnect_sends_request() {
        let mock = Arc::new(MockTransport::new());

        // Queue a tree disconnect response.
        let mut h = Header::new_request(Command::TreeDisconnect);
        h.flags.set_response();
        h.credits = 32;
        use crate::msg::tree_disconnect::TreeDisconnectResponse;
        mock.queue_response(pack_message(&h, &TreeDisconnectResponse));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(99),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.disconnect(&mut conn).await.unwrap();
        assert_eq!(mock.sent_count(), 1);
    }

    // ── Delete file tests ────────────────────────────────────────────

    fn build_write_response(count: u32) -> Vec<u8> {
        use crate::msg::write::WriteResponse;
        let mut h = Header::new_request(Command::Write);
        h.flags.set_response();
        h.credits = 32;

        let body = WriteResponse {
            count,
            remaining: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
        };

        pack_message(&h, &body)
    }

    use crate::client::test_helpers::{
        build_close_error_response, build_query_info_error_response, build_query_info_response,
        build_query_info_response_with_status, build_set_info_response,
    };

    /// Build a FileBasicInformation buffer (40 bytes).
    fn build_file_basic_info(
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        change_time: u64,
        file_attributes: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&creation_time.to_le_bytes());
        buf.extend_from_slice(&last_access_time.to_le_bytes());
        buf.extend_from_slice(&last_write_time.to_le_bytes());
        buf.extend_from_slice(&change_time.to_le_bytes());
        buf.extend_from_slice(&file_attributes.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // Reserved/padding
        buf
    }

    /// Build a FileStandardInformation buffer (24 bytes).
    fn build_file_standard_info(
        allocation_size: u64,
        end_of_file: u64,
        number_of_links: u32,
        delete_pending: bool,
        directory: bool,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&allocation_size.to_le_bytes());
        buf.extend_from_slice(&end_of_file.to_le_bytes());
        buf.extend_from_slice(&number_of_links.to_le_bytes());
        buf.push(if delete_pending { 1 } else { 0 });
        buf.push(if directory { 1 } else { 0 });
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf
    }

    #[tokio::test]
    async fn delete_file_sends_compound_create_and_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };

        // DELETE = compound CREATE + SET_INFO(disposition) + CLOSE
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.delete_file(&mut conn, "remove.txt").await.unwrap();

        // One compound frame sent.
        assert_eq!(mock.sent_count(), 1);

        // The CREATE asks for DELETE access and leaves the deleting to the
        // SET_INFO. Pre-fix this asserted FILE_DELETE_ON_CLOSE, which Samba
        // silently ignores on a non-empty directory.
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        assert!(req.desired_access.contains(FileAccessMask::DELETE));
        assert_ne!(req.create_options & FILE_NON_DIRECTORY_FILE, 0);
        // Sharing only delete is what makes the server break a lingering
        // handle lease (vdavid/cmdr#307).
        assert_eq!(
            req.share_access,
            ShareAccess(ShareAccess::FILE_SHARE_DELETE)
        );
    }

    /// The CREATE that opens a compound `mock` was sent at `index`.
    fn sent_create(mock: &MockTransport, index: usize) -> CreateRequest {
        let sent = mock.sent_message(index).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        CreateRequest::unpack(&mut cursor).unwrap()
    }

    /// Deleting or renaming a junction or symlink acts on the link, never on
    /// what it points to (MS-SMB2 § 2.2.13 `FILE_OPEN_REPARSE_POINT`): without
    /// the flag the server opens a junction's target, so a delete removed the
    /// target directory and a rename moved it.
    #[tokio::test]
    async fn delete_and_rename_open_the_reparse_point_itself() {
        let tree = stat_tree();
        let ok_compound = || {
            build_compound_response_frame(&[
                build_create_response(STAT_FILE, 0),
                build_set_info_response(),
                build_close_response(),
            ])
        };
        let mock = Arc::new(MockTransport::new());
        for _ in 0..3 {
            mock.queue_response(ok_compound());
        }
        let mut conn = setup_connection(&mock);

        tree.delete_file(&mut conn, "link.txt").await.unwrap();
        tree.delete_directory(&mut conn, "junction").await.unwrap();
        tree.rename(&mut conn, "junction", "renamed").await.unwrap();

        for (index, what) in ["delete_file", "delete_directory", "rename"]
            .into_iter()
            .enumerate()
        {
            let options = sent_create(&mock, index).create_options;
            assert_ne!(options & FILE_OPEN_REPARSE_POINT, 0, "{what}: {options:#x}");
        }
        // The type check stays: a directory delete still refuses a file.
        assert_ne!(
            sent_create(&mock, 0).create_options & FILE_NON_DIRECTORY_FILE,
            0
        );
        assert_ne!(
            sent_create(&mock, 1).create_options & FILE_DIRECTORY_FILE,
            0
        );
    }

    /// A link is a directory on Windows (a junction, `mklink /D`) and a file on
    /// Samba, so `delete_link` names neither: only the reparse point itself.
    #[tokio::test]
    async fn delete_link_opens_the_reparse_point_whatever_its_type() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        stat_tree()
            .delete_link(&mut conn, "junction")
            .await
            .unwrap();

        let create = sent_create(&mock, 0);
        assert_eq!(create.create_options, FILE_OPEN_REPARSE_POINT);
        assert!(create.desired_access.contains(FileAccessMask::DELETE));
        assert_eq!(
            sent_compound(&mock, 0),
            vec![
                (Command::Create, None),
                (Command::SetInfo, None),
                (Command::Close, None)
            ]
        );
    }

    #[tokio::test]
    async fn delete_link_refused_exclusively_retries_shared() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(failed_delete_compound(NtStatus::SHARING_VIOLATION));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        stat_tree().delete_link(&mut conn, "link").await.unwrap();

        assert_eq!(mock.sent_count(), 2);
        assert_eq!(sent_create(&mock, 1).share_access, SHARE_ALL);
        assert_eq!(
            sent_create(&mock, 1).create_options,
            FILE_OPEN_REPARSE_POINT
        );
    }

    /// A compound response whose CREATE failed with `status`, every later op
    /// cascading the same status.
    fn failed_delete_compound(status: NtStatus) -> Vec<u8> {
        let ops = [Command::Create, Command::SetInfo, Command::Close].map(|command| {
            let mut hdr = Header::new_request(command);
            hdr.flags.set_response();
            hdr.credits = 32;
            hdr.status = status;
            pack_message(
                &hdr,
                &crate::msg::header::ErrorResponse {
                    error_context_count: 0,
                    error_data: vec![],
                },
            )
        });
        build_compound_response_frame(&ops)
    }

    /// When another client genuinely has the file open, the exclusive open is
    /// refused, and the delete goes ahead sharing everything, as it always did.
    #[tokio::test]
    async fn delete_file_refused_exclusively_retries_shared() {
        for refusal in [NtStatus::SHARING_VIOLATION, NtStatus::INTERNAL_ERROR] {
            let mock = Arc::new(MockTransport::new());
            mock.queue_response(failed_delete_compound(refusal));
            mock.queue_response(build_compound_response_frame(&[
                build_create_response(
                    FileId {
                        persistent: 1,
                        volatile: 2,
                    },
                    0,
                ),
                build_set_info_response(),
                build_close_response(),
            ]));

            let mut conn = setup_connection(&mock);
            let tree = Tree {
                pin: None,
                tree_id: TreeId(10),
                share_name: "test".to_string(),
                server: "test-server".to_string(),
                is_dfs: false,
                encrypt_data: false,
                dfs_origin: None,
            };

            tree.delete_file(&mut conn, "in-use.txt")
                .await
                .unwrap_or_else(|e| panic!("after {refusal:?}: {e}"));

            assert_eq!(mock.sent_count(), 2, "after {refusal:?}");
            let shares: Vec<ShareAccess> = (0..2)
                .map(|n| {
                    let sent = mock.sent_message(n).unwrap();
                    let mut cursor = ReadCursor::new(&sent);
                    let _header = Header::unpack(&mut cursor).unwrap();
                    CreateRequest::unpack(&mut cursor).unwrap().share_access
                })
                .collect();
            assert_eq!(
                shares,
                [ShareAccess(ShareAccess::FILE_SHARE_DELETE), SHARE_ALL],
                "after {refusal:?}"
            );
        }
    }

    #[tokio::test]
    async fn delete_file_create_failure_returns_error() {
        let mock = Arc::new(MockTransport::new());

        // Build compound response where CREATE fails.
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_resp = pack_message(
            &create_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut setinfo_hdr = Header::new_request(Command::SetInfo);
        setinfo_hdr.flags.set_response();
        setinfo_hdr.credits = 32;
        setinfo_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let setinfo_resp = pack_message(
            &setinfo_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.delete_file(&mut conn, "nonexistent.txt").await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
        // Only the one compound frame, no standalone CLOSE needed.
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn delete_file_close_failure_issues_standalone_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };

        // Compound: CREATE and SET_INFO succeed, CLOSE fails (with a status
        // that isn't retried; UNSUCCESSFUL is, see the retry tests).
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::ACCESS_DENIED;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        // Queue response for the standalone CLOSE retry.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.delete_file(&mut conn, "tricky.txt").await;
        assert!(result.is_err());
        // Compound frame + standalone CLOSE = 2 messages sent.
        assert_eq!(mock.sent_count(), 2);
    }

    /// A delete compound whose CREATE and SET_INFO succeeded and whose CLOSE
    /// came back `close_status`.
    fn close_failed_delete_compound(file_id: FileId, close_status: NtStatus) -> Vec<u8> {
        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = close_status;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        build_compound_response_frame(&[
            build_create_response(file_id, 0),
            build_set_info_response(),
            close_resp,
        ])
    }

    /// QNAP's recycle bin answers the CLOSE `STATUS_UNSUCCESSFUL` when parallel
    /// deletes race to create the same `@Recycle` folder, leaving the file in
    /// place. A second try, once the winner has made the folder, goes through
    /// (`docs/notes/2026-10-22-rm-close-unsuccessful.md`).
    #[tokio::test]
    async fn delete_retries_once_when_the_close_is_unsuccessful() {
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        for directory in [false, true] {
            let mock = Arc::new(MockTransport::new());
            mock.queue_response(close_failed_delete_compound(
                file_id,
                NtStatus::UNSUCCESSFUL,
            ));
            mock.queue_response(build_close_response());
            mock.queue_response(build_compound_response_frame(&[
                build_create_response(file_id, 0),
                build_set_info_response(),
                build_close_response(),
            ]));
            let mut conn = setup_connection(&mock);

            let result = if directory {
                stat_tree().delete_directory(&mut conn, "dir").await
            } else {
                stat_tree().delete_file(&mut conn, "racing.txt").await
            };

            result.unwrap_or_else(|e| panic!("directory={directory}: {e}"));
            // Compound, standalone CLOSE of the handle it left open, compound.
            assert_eq!(mock.sent_count(), 3, "directory={directory}");
            assert_eq!(sent_create(&mock, 2).name, sent_create(&mock, 0).name);
        }
    }

    /// One retry, not a loop: a CLOSE that keeps failing surfaces its status.
    #[tokio::test]
    async fn delete_gives_up_after_one_retry_of_an_unsuccessful_close() {
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        let mock = Arc::new(MockTransport::new());
        for _ in 0..2 {
            mock.queue_response(close_failed_delete_compound(
                file_id,
                NtStatus::UNSUCCESSFUL,
            ));
            mock.queue_response(build_close_response());
        }
        let mut conn = setup_connection(&mock);

        let err = stat_tree()
            .delete_file(&mut conn, "stuck.txt")
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            Error::Protocol {
                status: NtStatus::UNSUCCESSFUL,
                command: Command::Close
            }
        ));
        assert_eq!(mock.sent_count(), 4);
    }

    // ── Write file tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn write_file_sends_create_write_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xCC,
            volatile: 0xDD,
        };

        // Small data goes as a CREATE+WRITE+FLUSH compound, then a standalone CLOSE.
        let create_resp = build_create_response(file_id, 0);
        let write_resp = build_write_response(5);
        let flush_resp = build_flush_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            write_resp,
            flush_resp,
        ]));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let written = tree
            .write_file(&mut conn, "out.txt", b"hello")
            .await
            .unwrap();
        assert_eq!(written, 5);
        // The compound frame and the CLOSE.
        assert_eq!(mock.sent_count(), 2);
    }

    // ── Stat tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn stat_sends_compound_and_returns_file_info() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xEE,
            volatile: 0xFF,
        };

        // STAT = compound CREATE + QUERY_INFO(basic) + QUERY_INFO(standard) + CLOSE
        let create_resp = build_create_response(file_id, 0);
        let basic = build_file_basic_info(
            132_000_000_000_000_000,
            132_100_000_000_000_000,
            133_000_000_000_000_000,
            133_200_000_000_000_000,
            0x20, // ARCHIVE
        );
        let basic_resp = build_query_info_response(basic);
        let std_info = build_file_standard_info(4096, 2048, 1, false, false);
        let std_resp = build_query_info_response(std_info);
        let close_resp = build_close_response();

        let frame = build_compound_response_frame(&[
            create_resp,
            basic_resp,
            std_resp,
            no_tag(),
            close_resp,
        ]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let info = tree.stat(&mut conn, "doc.txt").await.unwrap();
        assert_eq!(info.size, 2048);
        assert!(!info.is_directory);
        assert_eq!(info.created, FileTime(132_000_000_000_000_000));
        assert_eq!(info.modified, FileTime(133_000_000_000_000_000));
        assert_eq!(info.accessed, FileTime(132_100_000_000_000_000));
        assert_eq!(info.changed, FileTime(133_200_000_000_000_000));
        assert_eq!(
            info.attributes,
            FileAttributes::new(FileAttributes::ARCHIVE)
        );
        // One compound frame sent.
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn stat_create_failure_returns_error() {
        let mock = Arc::new(MockTransport::new());

        // Build compound response where CREATE fails (all ops cascade).
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };
        let create_resp = pack_message(&create_hdr, &err_body);

        let mut q1_hdr = Header::new_request(Command::QueryInfo);
        q1_hdr.flags.set_response();
        q1_hdr.credits = 32;
        q1_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let q1_resp = pack_message(&q1_hdr, &err_body);

        let mut q2_hdr = Header::new_request(Command::QueryInfo);
        q2_hdr.flags.set_response();
        q2_hdr.credits = 32;
        q2_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let q2_resp = pack_message(&q2_hdr, &err_body);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_resp = pack_message(&close_hdr, &err_body);

        let frame = build_compound_response_frame(&[
            create_resp,
            q1_resp,
            q2_resp.clone(),
            q2_resp,
            close_resp,
        ]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.stat(&mut conn, "nonexistent.txt").await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn stat_query_failure_issues_standalone_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xEE,
            volatile: 0xFF,
        };

        // Compound: CREATE succeeds, first QUERY_INFO fails, rest cascade.
        let create_resp = build_create_response(file_id, 0);

        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        let mut q1_hdr = Header::new_request(Command::QueryInfo);
        q1_hdr.flags.set_response();
        q1_hdr.credits = 32;
        q1_hdr.status = NtStatus::UNSUCCESSFUL;
        let q1_resp = pack_message(&q1_hdr, &err_body);

        let mut q2_hdr = Header::new_request(Command::QueryInfo);
        q2_hdr.flags.set_response();
        q2_hdr.credits = 32;
        q2_hdr.status = NtStatus::UNSUCCESSFUL;
        let q2_resp = pack_message(&q2_hdr, &err_body);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::UNSUCCESSFUL;
        let close_resp = pack_message(&close_hdr, &err_body);

        let frame = build_compound_response_frame(&[
            create_resp,
            q1_resp,
            q2_resp.clone(),
            q2_resp,
            close_resp,
        ]);
        mock.queue_response(frame);

        // Queue response for the standalone CLOSE retry.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.stat(&mut conn, "tricky.txt").await;
        assert!(result.is_err());
        // Compound frame + standalone CLOSE = 2 messages sent.
        assert_eq!(mock.sent_count(), 2);
    }

    // ── Stat with identity ──────────────────────────────────────────

    /// Every sub-request of the compound `mock` sent `index`-th: its command,
    /// and for a QUERY_INFO the info type and class it asked for.
    fn sent_compound(mock: &MockTransport, index: usize) -> Vec<(Command, Option<(InfoType, u8)>)> {
        let sent = mock.sent_message(index).unwrap();
        let mut ops = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&sent[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            let class = (header.command == Command::QueryInfo).then(|| {
                let req = QueryInfoRequest::unpack(&mut cursor).unwrap();
                (req.info_type, req.file_info_class)
            });
            ops.push((header.command, class));
            if header.next_command == 0 {
                return ops;
            }
            offset += header.next_command as usize;
        }
    }

    const STAT_FILE: FileId = FileId {
        persistent: 0xEE,
        volatile: 0xFF,
    };

    fn stat_tree() -> Tree {
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

    fn stat_basic() -> Vec<u8> {
        build_query_info_response(build_file_basic_info(1, 2, 3, 4, 0x20))
    }

    fn stat_standard() -> Vec<u8> {
        build_query_info_response(build_file_standard_info(4096, 2048, 1, false, false))
    }

    fn index_answer(n: u64) -> Vec<u8> {
        build_query_info_response(n.to_le_bytes().to_vec())
    }

    /// `FileFsVolumeInformation`: creation time, then the serial.
    fn volume_answer(serial: u32) -> Vec<u8> {
        let mut out = vec![0u8; 8];
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&[0u8; 6]);
        build_query_info_response(out)
    }

    /// `FileAttributeTagInformation` (MS-FSCC § 2.4.6): attributes, then
    /// the tag.
    fn tag_answer(attributes: u32, tag: u32) -> Vec<u8> {
        let mut out = attributes.to_le_bytes().to_vec();
        out.extend_from_slice(&tag.to_le_bytes());
        build_query_info_response(out)
    }

    /// The tag answer for a file that isn't a reparse point.
    fn no_tag() -> Vec<u8> {
        tag_answer(FileAttributes::ARCHIVE, 0)
    }

    const TAG: Option<(InfoType, u8)> = Some((InfoType::File, FILE_ATTRIBUTE_TAG_INFORMATION));
    const BASIC: Option<(InfoType, u8)> = Some((InfoType::File, FILE_BASIC_INFORMATION));
    const STANDARD: Option<(InfoType, u8)> = Some((InfoType::File, FILE_STANDARD_INFORMATION));

    #[tokio::test]
    async fn plain_stat_asks_for_the_tag_and_no_identity() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            stat_basic(),
            stat_standard(),
            no_tag(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        stat_tree().stat(&mut conn, "doc.txt").await.unwrap();

        assert_eq!(
            sent_compound(&mock, 0),
            vec![
                (Command::Create, None),
                (Command::QueryInfo, BASIC),
                (Command::QueryInfo, STANDARD),
                (Command::QueryInfo, TAG),
                (Command::Close, None),
            ]
        );
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn stat_with_identity_asks_for_it_after_the_metadata_in_one_compound() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            stat_basic(),
            stat_standard(),
            index_answer(0x77),
            volume_answer(0xBEEF),
            no_tag(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        let (info, identity) = stat_tree()
            .stat_with_identity(&mut conn, "doc.txt")
            .await
            .unwrap();

        assert_eq!((info.size, info.changed), (2048, FileTime(4)));
        assert_eq!(
            identity,
            Some(FileIdentity {
                volume_serial: 0xBEEF,
                index_number: 0x77,
            })
        );
        // Pitfall 31: the optional queries go after the required ones, so a
        // server that cascades a refusal can't take the metadata down.
        assert_eq!(
            sent_compound(&mock, 0),
            vec![
                (Command::Create, None),
                (Command::QueryInfo, BASIC),
                (Command::QueryInfo, STANDARD),
                (Command::QueryInfo, Some((InfoType::File, 6))),
                (Command::QueryInfo, Some((InfoType::Filesystem, 1))),
                (Command::QueryInfo, TAG),
                (Command::Close, None),
            ]
        );
        assert_eq!(mock.sent_count(), 1, "one round trip");
    }

    #[tokio::test]
    async fn a_refused_identity_still_stats_and_closes_the_handle_the_cascade_left_open() {
        // MS-SMB2 § 3.3.5.2.7.2: a server may fail every related op after a
        // failed one, CLOSE included, which leaves the handle open.
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            stat_basic(),
            stat_standard(),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_close_error_response(NtStatus::NOT_SUPPORTED),
        ]));
        mock.queue_response(build_close_response());
        let mut conn = setup_connection(&mock);
        let (info, identity) = stat_tree()
            .stat_with_identity(&mut conn, "doc.txt")
            .await
            .unwrap();

        assert_eq!(info.size, 2048);
        assert_eq!(identity, None);
        assert_eq!(mock.sent_count(), 2, "compound + standalone CLOSE");
        assert_eq!(sent_compound(&mock, 1), vec![(Command::Close, None)]);
    }

    #[tokio::test]
    async fn an_identity_with_either_half_zero_is_none() {
        // The same rule a checkpoint uses (`FileIdentity::is_proof`), so a
        // stat's identity and a writer's for the same file compare equal.
        for (index, serial) in [(0x77, 0), (0, 0xBEEF), (0, 0)] {
            let mock = Arc::new(MockTransport::new());
            mock.queue_response(build_compound_response_frame(&[
                build_create_response(STAT_FILE, 0),
                stat_basic(),
                stat_standard(),
                index_answer(index),
                volume_answer(serial),
                no_tag(),
                build_close_response(),
            ]));
            let mut conn = setup_connection(&mock);
            let (_, identity) = stat_tree()
                .stat_with_identity(&mut conn, "doc.txt")
                .await
                .unwrap();
            assert_eq!(identity, None, "index {index:#x}, serial {serial:#x}");
        }
    }

    /// A stat describes the link itself (MS-SMB2 § 2.2.13), as its listing
    /// entry does, and says which kind of reparse point it is.
    #[tokio::test]
    async fn stat_of_a_junction_describes_the_junction_and_carries_its_tag() {
        let junction = FileAttributes::DIRECTORY | FileAttributes::REPARSE_POINT;
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            build_query_info_response(build_file_basic_info(1, 2, 3, 4, junction)),
            build_query_info_response(build_file_standard_info(0, 0, 1, false, true)),
            tag_answer(junction, 0xA000_0003),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        let info = stat_tree().stat(&mut conn, "junction").await.unwrap();

        assert_eq!(info.reparse_tag, Some(ReparseTag::MOUNT_POINT));
        assert!(info.is_directory && info.is_link());
        let options = sent_create(&mock, 0).create_options;
        assert_ne!(options & FILE_OPEN_REPARSE_POINT, 0, "{options:#x}");
    }

    #[tokio::test]
    async fn stat_of_an_ordinary_file_has_no_tag() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            stat_basic(),
            stat_standard(),
            // A tag with no reparse bit beside it MUST be ignored (§ 2.4.6).
            tag_answer(FileAttributes::ARCHIVE, 0xA000_0003),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        let info = stat_tree().stat(&mut conn, "doc.txt").await.unwrap();
        assert_eq!(info.reparse_tag, None);
        assert!(!info.is_link());
    }

    #[tokio::test]
    async fn a_refused_tag_query_still_stats_and_closes_the_handle() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(STAT_FILE, 0),
            stat_basic(),
            stat_standard(),
            build_query_info_error_response(NtStatus::INVALID_INFO_CLASS),
            build_close_error_response(NtStatus::INVALID_INFO_CLASS),
        ]));
        mock.queue_response(build_close_response());
        let mut conn = setup_connection(&mock);
        let info = stat_tree().stat(&mut conn, "doc.txt").await.unwrap();

        assert_eq!((info.size, info.reparse_tag), (2048, None));
        assert_eq!(mock.sent_count(), 2, "compound + standalone CLOSE");
    }

    // ── Batch stat tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn stat_files_batch_happy_path() {
        let mock = Arc::new(MockTransport::new());

        // Queue 3 compound responses (CREATE+QUERY+QUERY+CLOSE each).
        for i in 0..3u64 {
            let file_id = FileId {
                persistent: i + 1,
                volatile: i + 100,
            };
            let create_resp = build_create_response(file_id, 0);
            let basic = build_file_basic_info(
                132_000_000_000_000_000 + i,
                132_100_000_000_000_000 + i,
                133_000_000_000_000_000 + i,
                133_000_000_000_000_000 + i,
                0x20,
            );
            let basic_resp = build_query_info_response(basic);
            let std_info = build_file_standard_info(4096, 1024 * (i + 1), 1, false, false);
            let std_resp = build_query_info_response(std_info);
            let close_resp = build_close_response();
            mock.queue_response(build_compound_response_frame(&[
                create_resp,
                basic_resp,
                std_resp,
                no_tag(),
                close_resp,
            ]));
        }

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .stat_files(&mut conn, &["a.txt", "b.txt", "c.txt"])
            .await;

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].as_ref().unwrap().size, 1024);
        assert_eq!(results[1].as_ref().unwrap().size, 2048);
        assert_eq!(results[2].as_ref().unwrap().size, 3072);
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn stat_files_batch_partial_failure() {
        let mock = Arc::new(MockTransport::new());

        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        // File 1: success
        let file_id = FileId {
            persistent: 1,
            volatile: 100,
        };
        let create_resp = build_create_response(file_id, 0);
        let basic = build_file_basic_info(
            132_000_000_000_000_000,
            132_100_000_000_000_000,
            133_000_000_000_000_000,
            133_000_000_000_000_000,
            0x20,
        );
        let basic_resp = build_query_info_response(basic);
        let std_info = build_file_standard_info(4096, 512, 1, false, false);
        let std_resp = build_query_info_response(std_info);
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            basic_resp,
            std_resp,
            no_tag(),
            close_resp,
        ]));

        // File 2: CREATE fails -- cascaded failure
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_err = pack_message(&create_hdr, &err_body);

        let mut q1_hdr = Header::new_request(Command::QueryInfo);
        q1_hdr.flags.set_response();
        q1_hdr.credits = 32;
        q1_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let q1_err = pack_message(&q1_hdr, &err_body);

        let mut q2_hdr = Header::new_request(Command::QueryInfo);
        q2_hdr.flags.set_response();
        q2_hdr.credits = 32;
        q2_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let q2_err = pack_message(&q2_hdr, &err_body);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_err = pack_message(&close_hdr, &err_body);
        mock.queue_response(build_compound_response_frame(&[
            create_err,
            q1_err,
            q2_err.clone(),
            q2_err,
            close_err,
        ]));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .stat_files(&mut conn, &["exists.txt", "missing.txt"])
            .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().unwrap().size, 512);
        assert!(results[1].is_err());
        assert_eq!(
            results[1].as_ref().unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn stat_files_empty_returns_empty() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results: Vec<Result<FileInfo>> = tree.stat_files(&mut conn, &[]).await;
        assert!(results.is_empty());
    }

    // ── Rename tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn rename_sends_compound_create_setinfo_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x11,
            volatile: 0x22,
        };

        // RENAME = compound CREATE + SET_INFO + CLOSE
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.rename(&mut conn, "old.txt", "new.txt").await.unwrap();

        // One compound frame sent.
        assert_eq!(mock.sent_count(), 1);

        // Verify the CREATE has DELETE access (required for rename)
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        assert!(req.desired_access.contains(FileAccessMask::DELETE));
    }

    /// The `FILE_RENAME_INFORMATION` buffer of the SET_INFO in a sent rename
    /// compound (CREATE + SET_INFO + CLOSE).
    fn sent_rename_info(frame: &[u8]) -> Vec<u8> {
        let mut cursor = ReadCursor::new(frame);
        let create = Header::unpack(&mut cursor).unwrap();
        let mut cursor = ReadCursor::new(&frame[create.next_command as usize..]);
        let header = Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::SetInfo);
        let req = SetInfoRequest::unpack(&mut cursor).unwrap();
        assert_eq!(req.file_info_class, FILE_RENAME_INFORMATION);
        req.buffer
    }

    #[tokio::test]
    async fn rename_replaces_an_existing_destination_only_when_asked() {
        let mock = Arc::new(MockTransport::new());
        for _ in 0..3 {
            mock.queue_response(build_compound_response_frame(&[
                build_create_response(FileId::SENTINEL, 0),
                build_set_info_response(),
                build_close_response(),
            ]));
        }
        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.rename(&mut conn, "a.tmp", "a").await.unwrap();
        tree.rename_with(&mut conn, "b.tmp", "b", RenameOptions::new())
            .await
            .unwrap();
        tree.rename_with(
            &mut conn,
            "c.tmp",
            "c",
            RenameOptions::new().replace_if_exists(true),
        )
        .await
        .unwrap();

        // ReplaceIfExists is the buffer's first byte; the name follows the
        // 20-byte fixed part either way.
        let infos: Vec<Vec<u8>> = (0..3)
            .map(|i| sent_rename_info(&mock.sent_message(i).unwrap()))
            .collect();
        assert_eq!(infos[0][0], 0, "rename never replaces");
        assert_eq!(infos[1][0], 0, "the default options don't replace");
        assert_eq!(infos[2][0], 1, "replace_if_exists sets ReplaceIfExists");
        assert_eq!(&infos[2][20..], &[b'c', 0]);
    }

    #[tokio::test]
    async fn rename_create_failure_returns_error() {
        let mock = Arc::new(MockTransport::new());

        // Build compound response where CREATE fails.
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_resp = pack_message(
            &create_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut setinfo_hdr = Header::new_request(Command::SetInfo);
        setinfo_hdr.flags.set_response();
        setinfo_hdr.credits = 32;
        setinfo_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let setinfo_resp = pack_message(
            &setinfo_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.rename(&mut conn, "old.txt", "new.txt").await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
        // Only the one compound frame, no standalone CLOSE needed.
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn rename_setinfo_failure_issues_standalone_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x11,
            volatile: 0x22,
        };

        // Compound: CREATE succeeds, SET_INFO fails, CLOSE cascades failure.
        let create_resp = build_create_response(file_id, 0);

        let mut setinfo_hdr = Header::new_request(Command::SetInfo);
        setinfo_hdr.flags.set_response();
        setinfo_hdr.credits = 32;
        setinfo_hdr.status = NtStatus::UNSUCCESSFUL;
        let setinfo_resp = pack_message(
            &setinfo_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::UNSUCCESSFUL;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        // Queue response for the standalone CLOSE retry.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.rename(&mut conn, "old.txt", "new.txt").await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), Some(NtStatus::UNSUCCESSFUL));
        // Compound frame + standalone CLOSE = 2 messages sent.
        assert_eq!(mock.sent_count(), 2);
    }

    // ── Batch rename tests ────────────────────────────────────────────

    #[tokio::test]
    async fn rename_files_batch_happy_path() {
        let mock = Arc::new(MockTransport::new());

        // Queue 3 compound responses (CREATE+SET_INFO+CLOSE each).
        for i in 0..3u64 {
            let file_id = FileId {
                persistent: i + 1,
                volatile: i + 100,
            };
            let create_resp = build_create_response(file_id, 0);
            let setinfo_resp = build_set_info_response();
            let close_resp = build_close_response();
            mock.queue_response(build_compound_response_frame(&[
                create_resp,
                setinfo_resp,
                close_resp,
            ]));
        }

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .rename_files(
                &mut conn,
                &[
                    ("a.txt", "a2.txt"),
                    ("b.txt", "b2.txt"),
                    ("c.txt", "c2.txt"),
                ],
            )
            .await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert!(results[2].is_ok());
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn rename_files_batch_partial_failure() {
        let mock = Arc::new(MockTransport::new());

        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        // File 1: success
        let file_id = FileId {
            persistent: 1,
            volatile: 100,
        };
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            setinfo_resp,
            close_resp,
        ]));

        // File 2: CREATE fails (not found)
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_err = pack_message(&create_hdr, &err_body);

        let mut si_hdr = Header::new_request(Command::SetInfo);
        si_hdr.flags.set_response();
        si_hdr.credits = 32;
        si_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let si_err = pack_message(&si_hdr, &err_body);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_err = pack_message(&close_hdr, &err_body);
        mock.queue_response(build_compound_response_frame(&[
            create_err, si_err, close_err,
        ]));

        // File 3: success
        let file_id = FileId {
            persistent: 3,
            volatile: 102,
        };
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            setinfo_resp,
            close_resp,
        ]));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .rename_files(
                &mut conn,
                &[
                    ("a.txt", "a2.txt"),
                    ("missing.txt", "m2.txt"),
                    ("c.txt", "c2.txt"),
                ],
            )
            .await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert_eq!(
            results[1].as_ref().unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
        assert!(results[2].is_ok());
    }

    #[tokio::test]
    async fn rename_files_empty_returns_empty() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results: Vec<Result<()>> = tree.rename_files(&mut conn, &[]).await;
        assert!(results.is_empty());
        assert_eq!(mock.sent_count(), 0);
    }

    // ── Create directory tests ───────────────────────────────────────

    #[tokio::test]
    async fn create_directory_sends_create_and_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x33,
            volatile: 0x44,
        };

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.create_directory(&mut conn, "new_dir").await.unwrap();
        assert_eq!(mock.sent_count(), 2);

        // Verify the CREATE has FILE_DIRECTORY_FILE option and FileCreate disposition
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        assert_eq!(req.create_disposition, CreateDisposition::FileCreate);
        assert_ne!(req.create_options & FILE_DIRECTORY_FILE, 0);
    }

    // ── Exclusive-create writer open tests ────────────────────────────

    #[tokio::test]
    async fn exclusive_writer_sends_file_create_disposition() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        mock.queue_response(crate::client::test_helpers::build_writer_open_response(
            file_id, 0, None, None,
        ));
        mock.queue_response(build_close_response());

        let conn = setup_connection(&mock);
        let tree = Arc::new(Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        });

        let writer = tree
            .create_file_writer_exclusive(conn, "new.bin")
            .await
            .unwrap();
        writer.abort().await.unwrap();

        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        assert_eq!(
            req.create_disposition,
            CreateDisposition::FileCreate,
            "exclusive-create writer must use FileCreate, not FileOverwriteIf"
        );
        // File, not directory.
        assert_ne!(req.create_options & FILE_NON_DIRECTORY_FILE, 0);
    }

    #[tokio::test]
    async fn exclusive_writer_maps_collision_to_already_exists() {
        let mock = Arc::new(MockTransport::new());
        // STATUS_OBJECT_NAME_COLLISION = 0xC0000035; the server response any
        // time `FileCreate` hits an existing file.
        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_COLLISION),
            build_query_info_error_response(NtStatus::OBJECT_NAME_COLLISION),
            build_query_info_error_response(NtStatus::OBJECT_NAME_COLLISION),
            build_query_info_error_response(NtStatus::OBJECT_NAME_COLLISION),
        ]));

        let conn = setup_connection(&mock);
        let tree = Arc::new(Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        });

        let err = tree
            .create_file_writer_exclusive(conn, "existing.bin")
            .await
            .err()
            .expect("exclusive-create on an existing file must error");
        assert_eq!(
            err.kind(),
            crate::ErrorKind::AlreadyExists,
            "STATUS_OBJECT_NAME_COLLISION must map to ErrorKind::AlreadyExists, got: {err}"
        );
    }

    // ── Delete directory tests ───────────────────────────────────────

    #[tokio::test]
    async fn delete_directory_sends_compound_create_and_close() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x55,
            volatile: 0x66,
        };

        // DELETE = compound CREATE + SET_INFO(disposition) + CLOSE
        let create_resp = build_create_response(file_id, 0);
        let setinfo_resp = build_set_info_response();
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, setinfo_resp, close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        tree.delete_directory(&mut conn, "old_dir").await.unwrap();

        // One compound frame sent.
        assert_eq!(mock.sent_count(), 1);

        // The CREATE opens the directory; the SET_INFO is what deletes it.
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        assert!(req.desired_access.contains(FileAccessMask::DELETE));
        assert_ne!(req.create_options & FILE_DIRECTORY_FILE, 0);
    }

    #[tokio::test]
    async fn delete_directory_surfaces_not_empty_instead_of_reporting_success() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x51,
            volatile: 0x52,
        };

        // A non-empty directory: CREATE opens it, SET_INFO refuses. Pre-fix
        // this used FILE_DELETE_ON_CLOSE, where Samba answers every op with
        // SUCCESS and deletes nothing, so the caller was told it worked.
        let create_resp = build_create_response(file_id, 0);
        let mut setinfo_hdr = Header::new_request(Command::SetInfo);
        setinfo_hdr.flags.set_response();
        setinfo_hdr.credits = 32;
        setinfo_hdr.status = NtStatus::DIRECTORY_NOT_EMPTY;
        let setinfo_resp = pack_message(
            &setinfo_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::DIRECTORY_NOT_EMPTY;
        let close_resp = pack_message(
            &close_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            setinfo_resp,
            close_resp,
        ]));
        // The standalone CLOSE that releases the handle.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.delete_directory(&mut conn, "full_dir").await;

        assert_eq!(
            result.unwrap_err().status(),
            Some(NtStatus::DIRECTORY_NOT_EMPTY)
        );
        // The compound plus the standalone CLOSE, so the handle isn't leaked.
        assert_eq!(mock.sent_count(), 2);
    }

    #[test]
    fn all_or_first_err_rejects_a_short_chain() {
        // Callers index responses[2] straight after this returns, so a chain
        // that came back short has to be an error, not a panic waiting to
        // happen.
        let result = all_or_first_err(vec![], 3);
        assert!(
            result.is_err(),
            "a 3-op chain answered with 0 frames must be an error"
        );
    }

    // ── Batch delete tests ───────────────────────────────────────────

    #[tokio::test]
    async fn delete_files_batch_happy_path() {
        let mock = Arc::new(MockTransport::new());

        // Queue 3 compound responses (CREATE+SET_INFO+CLOSE each).
        for i in 0..3u64 {
            let file_id = FileId {
                persistent: i + 1,
                volatile: i + 100,
            };
            let create_resp = build_create_response(file_id, 0);
            let setinfo_resp = build_set_info_response();
            let close_resp = build_close_response();
            mock.queue_response(build_compound_response_frame(&[
                create_resp,
                setinfo_resp,
                close_resp,
            ]));
        }

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .delete_files(&mut conn, &["a.txt", "b.txt", "c.txt"])
            .await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert!(results[2].is_ok());
        // 3 compound frames sent (one per file).
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn delete_files_batch_partial_failure() {
        let mock = Arc::new(MockTransport::new());

        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        // File 1: success
        let file_id = FileId {
            persistent: 1,
            volatile: 100,
        };
        let create_resp = build_create_response(file_id, 0);
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            build_set_info_response(),
            build_close_response(),
        ]));

        // File 2: CREATE fails (not found) -- cascaded failure
        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_err = pack_message(&create_hdr, &err_body);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_err = pack_message(&close_hdr, &err_body);

        let mut setinfo_hdr = Header::new_request(Command::SetInfo);
        setinfo_hdr.flags.set_response();
        setinfo_hdr.credits = 32;
        setinfo_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let setinfo_err = pack_message(&setinfo_hdr, &err_body);
        mock.queue_response(build_compound_response_frame(&[
            create_err,
            setinfo_err,
            close_err,
        ]));

        // File 3: success
        let file_id = FileId {
            persistent: 3,
            volatile: 102,
        };
        let create_resp = build_create_response(file_id, 0);
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            build_set_info_response(),
            build_close_response(),
        ]));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree
            .delete_files(&mut conn, &["a.txt", "missing.txt", "c.txt"])
            .await;

        assert_eq!(results.len(), 3);
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert_eq!(
            results[1].as_ref().unwrap_err().status(),
            Some(NtStatus::OBJECT_NAME_NOT_FOUND)
        );
        assert!(results[2].is_ok());
    }

    #[tokio::test]
    async fn delete_files_batch_close_failure_issues_cleanup() {
        let mock = Arc::new(MockTransport::new());

        let err_body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        // File 1: CREATE succeeds, CLOSE fails
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        let create_resp = build_create_response(file_id, 0);

        let mut close_hdr = Header::new_request(Command::Close);
        close_hdr.flags.set_response();
        close_hdr.credits = 32;
        close_hdr.status = NtStatus::ACCESS_DENIED; // UNSUCCESSFUL is retried
        let close_fail = pack_message(&close_hdr, &err_body);
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            build_set_info_response(),
            close_fail,
        ]));

        // File 1's handle is closed before file 2 starts, so its standalone
        // CLOSE response is queued between the two compounds.
        mock.queue_response(build_close_response());

        // File 2: success
        let file_id2 = FileId {
            persistent: 0xCC,
            volatile: 0xDD,
        };
        let create_resp2 = build_create_response(file_id2, 0);
        let close_resp2 = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp2,
            build_set_info_response(),
            close_resp2,
        ]));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree.delete_files(&mut conn, &["leaky.txt", "ok.txt"]).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].is_err());
        assert!(results[1].is_ok());
        // 2 compound frames + 1 standalone CLOSE = 3 messages sent.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn delete_files_empty_returns_empty() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let results = tree.delete_files(&mut conn, &[]).await;
        assert!(results.is_empty());
        assert_eq!(mock.sent_count(), 0);
    }

    // ── Pipelined read tests ────────────────────────────────────────

    fn build_read_response_with_msg_id(
        status: NtStatus,
        msg_id: MessageId,
        data: Vec<u8>,
    ) -> Vec<u8> {
        let mut h = Header::new_request(Command::Read);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;
        h.message_id = msg_id;

        if status == NtStatus::END_OF_FILE {
            use crate::msg::header::ErrorResponse;
            let body = ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            };
            return pack_message(&h, &body);
        }

        let body = ReadResponse {
            data_offset: 0x50,
            data_remaining: 0,
            flags: 0,
            data,
        };

        pack_message(&h, &body)
    }

    fn build_write_response_with_msg_id(msg_id: MessageId, count: u32) -> Vec<u8> {
        use crate::msg::write::WriteResponse;
        let mut h = Header::new_request(Command::Write);
        h.flags.set_response();
        h.credits = 32;
        h.message_id = msg_id;

        let body = WriteResponse {
            count,
            remaining: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
        };

        pack_message(&h, &body)
    }

    #[tokio::test]
    async fn pipelined_read_four_chunks() {
        // File: 256 KB = 4 chunks of 64 KB.
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(20);
        let file_id = FileId {
            persistent: 0x100,
            volatile: 0x200,
        };
        let file_size = 256 * 1024u64;

        // Build 256 KB of test data with a recognizable pattern.
        let mut expected_data = vec![0u8; file_size as usize];
        for (i, byte) in expected_data.iter_mut().enumerate() {
            *byte = (i % 251) as u8; // prime to avoid alignment artifacts
        }

        // Queue: CREATE response.
        mock.queue_response(build_create_response(file_id, file_size));

        // Queue: 4 READ responses (in order, matching the MessageIds
        // that send_request will assign).
        // After CREATE, the next message_id = 1 (CREATE consumed 0).
        // Actually, connection starts at next_message_id=0. But setup_connection
        // doesn't call negotiate (which would consume msg_id 0).
        // send_request for CREATE will use msg_id 0, then the 4 READs will
        // use msg_ids 1, 2, 3, 4.
        for i in 0..4 {
            let offset = i * 65536;
            let chunk = expected_data[offset..offset + 65536].to_vec();
            mock.queue_response(build_read_response_with_msg_id(
                NtStatus::SUCCESS,
                MessageId((i / 65536 + 1) as u64), // msg_ids 1..4
                chunk,
            ));
        }
        // Fix: the message IDs. send_request increments next_message_id each time.
        // After CREATE (msg_id=0), the 4 READs get msg_ids 1, 2, 3, 4.
        // Let me rebuild these correctly.
        // Actually I already did it wrong above. Let me clear and redo.
        // The loop above computed msg_id as (i / 65536 + 1) which is always 1.
        // Let me fix this.

        // Clear the mock and redo.
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_create_response(file_id, file_size));

        for i in 0u64..4 {
            let offset = (i * 65536) as usize;
            let chunk = expected_data[offset..offset + 65536].to_vec();
            mock.queue_response(build_read_response_with_msg_id(
                NtStatus::SUCCESS,
                MessageId(i + 1), // msg_ids 1, 2, 3, 4
                chunk,
            ));
        }

        // Queue: CLOSE response.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "big.bin")
            .await
            .unwrap();

        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);

        // 1 CREATE + 4 READs + 1 CLOSE = 6 messages sent.
        assert_eq!(mock.sent_count(), 6);
    }

    #[tokio::test]
    async fn pipelined_read_responses_out_of_order() {
        // File: 192 KB = 3 chunks of 64 KB. Responses arrive in reverse order.
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(20);
        let file_id = FileId {
            persistent: 0x300,
            volatile: 0x400,
        };
        let file_size = 192 * 1024u64;

        let mut expected_data = vec![0u8; file_size as usize];
        for (i, byte) in expected_data.iter_mut().enumerate() {
            *byte = (i % 199) as u8;
        }

        mock.queue_response(build_create_response(file_id, file_size));

        // Queue responses in REVERSE order (msg_id 3, 2, 1) to test reassembly.
        for i in (0u64..3).rev() {
            let offset = (i * 65536) as usize;
            let chunk = expected_data[offset..offset + 65536].to_vec();
            mock.queue_response(build_read_response_with_msg_id(
                NtStatus::SUCCESS,
                MessageId(i + 1),
                chunk,
            ));
        }

        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "reverse.bin")
            .await
            .unwrap();

        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);
    }

    #[tokio::test]
    async fn pipelined_read_zero_byte_file() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x500,
            volatile: 0x600,
        };

        // CREATE reports file_size=0.
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "empty.bin")
            .await
            .unwrap();

        assert!(data.is_empty());
        // 1 CREATE + 1 CLOSE = 2 messages (no READs needed).
        assert_eq!(mock.sent_count(), 2);
    }

    #[tokio::test]
    async fn pipelined_read_end_of_file_mid_window() {
        // File claims to be 128 KB (2 chunks), but second chunk returns STATUS_END_OF_FILE.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x700,
            volatile: 0x800,
        };
        let file_size = 128 * 1024u64;
        let first_chunk = vec![0xAA; 65536];

        mock.queue_response(build_create_response(file_id, file_size));
        // First chunk succeeds.
        mock.queue_response(build_read_response_with_msg_id(
            NtStatus::SUCCESS,
            MessageId(1),
            first_chunk.clone(),
        ));
        // Second chunk returns END_OF_FILE.
        mock.queue_response(build_read_response_with_msg_id(
            NtStatus::END_OF_FILE,
            MessageId(2),
            vec![],
        ));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "truncated.bin")
            .await
            .unwrap();

        // We got the full buffer since file_size was 128 KB.
        // The second chunk area stays as zeros (from vec initialization).
        assert_eq!(data.len(), file_size as usize);
        assert_eq!(&data[..65536], &first_chunk);
    }

    #[tokio::test]
    async fn pipelined_read_window_sliding() {
        // File: 192 KB = 3 chunks. Credits = 2, so we need 2 windows.
        let file_id = FileId {
            persistent: 0x900,
            volatile: 0xA00,
        };
        let file_size = 192 * 1024u64;

        let mut expected_data = vec![0u8; file_size as usize];
        for (i, byte) in expected_data.iter_mut().enumerate() {
            *byte = (i % 173) as u8;
        }

        // Build with limited credits to force window sliding.
        // CREATE response grants only 2 credits (instead of default 32),
        // so the pipeline can only send 2 reads per window.
        let mock = Arc::new(MockTransport::new());

        let create_resp = {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.credits = 2; // Only grant 2 credits.
            let body = CreateResponse {
                oplock_level: OplockLevel::None,
                flags: 0,
                create_action: CreateAction::FileOpened,
                creation_time: FileTime::ZERO,
                last_access_time: FileTime::ZERO,
                last_write_time: FileTime::ZERO,
                change_time: FileTime::ZERO,
                allocation_size: 0,
                end_of_file: file_size,
                file_attributes: 0,
                file_id,
                create_contexts: vec![],
            };
            pack_message(&h, &body)
        };
        mock.queue_response(create_resp);

        // Window 1: 2 READs (chunks 0, 1). Responses grant 2 credits each.
        for i in 0u64..2 {
            let offset = (i * 65536) as usize;
            let chunk_data = expected_data[offset..offset + 65536].to_vec();
            let mut h = Header::new_request(Command::Read);
            h.flags.set_response();
            h.credits = 2; // Grant 2 credits per response.
            h.message_id = MessageId(i + 1);
            let body = ReadResponse {
                data_offset: 0x50,
                data_remaining: 0,
                flags: 0,
                data: chunk_data,
            };
            mock.queue_response(pack_message(&h, &body));
        }

        // Window 2: 1 READ (chunk 2).
        {
            let offset = (2 * 65536) as usize;
            let chunk_data = expected_data[offset..offset + 65536].to_vec();
            let mut h = Header::new_request(Command::Read);
            h.flags.set_response();
            h.credits = 2;
            h.message_id = MessageId(3);
            let body = ReadResponse {
                data_offset: 0x50,
                data_remaining: 0,
                flags: 0,
                data: chunk_data,
            };
            mock.queue_response(pack_message(&h, &body));
        }

        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "sliding.bin")
            .await
            .unwrap();

        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);
        // 1 CREATE + 3 READs + 1 CLOSE = 5.
        assert_eq!(mock.sent_count(), 5);
    }

    #[tokio::test]
    async fn sliding_window_sends_immediately_after_receive() {
        // File: 512 KB = 8 chunks of 64 KB. Only 4 credits available initially.
        // With sliding window: 4 sends, then each receive triggers a new send.
        // Total: 8 sends interleaved with 8 receives (not 2 batches of 4).
        let file_id = FileId {
            persistent: 0xF00,
            volatile: 0xF01,
        };
        let file_size = 8 * 65536u64;

        let mut expected_data = vec![0u8; file_size as usize];
        for (i, byte) in expected_data.iter_mut().enumerate() {
            *byte = (i % 137) as u8;
        }

        let mock = Arc::new(MockTransport::new());

        // CREATE response grants 4 credits (not the default 32).
        let create_resp = {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.credits = 4;
            let body = CreateResponse {
                oplock_level: OplockLevel::None,
                flags: 0,
                create_action: CreateAction::FileOpened,
                creation_time: FileTime::ZERO,
                last_access_time: FileTime::ZERO,
                last_write_time: FileTime::ZERO,
                change_time: FileTime::ZERO,
                allocation_size: 0,
                end_of_file: file_size,
                file_attributes: 0,
                file_id,
                create_contexts: vec![],
            };
            pack_message(&h, &body)
        };
        mock.queue_response(create_resp);

        // Queue 8 READ responses. Each grants 1 credit so the window
        // stays at 1 after the initial 4 are consumed (4 - 4 + 1 per response).
        // With sliding window, after initial 4 sends, each response triggers 1 more send.
        for i in 0u64..8 {
            let offset = (i * 65536) as usize;
            let chunk_data = expected_data[offset..offset + 65536].to_vec();
            let mut h = Header::new_request(Command::Read);
            h.flags.set_response();
            h.credits = 1; // Grant 1 credit per response.
            h.message_id = MessageId(i + 1);
            let body = ReadResponse {
                data_offset: 0x50,
                data_remaining: 0,
                flags: 0,
                data: chunk_data,
            };
            mock.queue_response(pack_message(&h, &body));
        }

        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let data = tree
            .read_file_pipelined(&mut conn, "sliding_test.bin")
            .await
            .unwrap();

        assert_eq!(data.len(), expected_data.len());
        assert_eq!(data, expected_data);

        // 1 CREATE + 8 READs + 1 CLOSE = 10 messages sent.
        assert_eq!(mock.sent_count(), 10);
    }

    // ── Pipelined read with progress tests ────────────────────────────

    #[tokio::test]
    async fn read_pipelined_with_progress_reports_progress() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF1,
            volatile: 0xF2,
        };
        // 2 chunks of 65536 bytes each.
        let file_size = 65536u64 * 2;
        let expected_data = vec![0xABu8; file_size as usize];

        // CREATE response with file size.
        let create_resp = {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.credits = 32;
            let body = CreateResponse {
                oplock_level: OplockLevel::None,
                flags: 0,
                create_action: CreateAction::FileOpened,
                creation_time: FileTime::ZERO,
                last_access_time: FileTime::ZERO,
                last_write_time: FileTime::ZERO,
                change_time: FileTime::ZERO,
                allocation_size: 0,
                end_of_file: file_size,
                file_attributes: 0,
                file_id,
                create_contexts: vec![],
            };
            pack_message(&h, &body)
        };
        mock.queue_response(create_resp);

        // 2 READ responses.
        for i in 0..2u64 {
            let offset = (i * 65536) as usize;
            let chunk = expected_data[offset..offset + 65536].to_vec();
            let resp = build_read_response_with_msg_id(NtStatus::SUCCESS, MessageId(i + 1), chunk);
            mock.queue_response(resp);
        }

        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut progress_reports = Vec::new();
        let data = tree
            .read_file_pipelined_with_progress(&mut conn, "progress_test.bin", |p| {
                progress_reports.push(p.bytes_transferred);
                ControlFlow::Continue(())
            })
            .await
            .unwrap();

        assert_eq!(data.len(), file_size as usize);
        // Should have received 2 progress callbacks (one per chunk).
        assert_eq!(progress_reports.len(), 2);
        assert_eq!(progress_reports[0], 65536);
        assert_eq!(progress_reports[1], file_size);
    }

    #[tokio::test]
    async fn read_pipelined_with_progress_cancellation() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF3,
            volatile: 0xF4,
        };
        // 4 chunks of 65536 bytes.
        let file_size = 65536u64 * 4;

        let create_resp = {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.credits = 32;
            let body = CreateResponse {
                oplock_level: OplockLevel::None,
                flags: 0,
                create_action: CreateAction::FileOpened,
                creation_time: FileTime::ZERO,
                last_access_time: FileTime::ZERO,
                last_write_time: FileTime::ZERO,
                change_time: FileTime::ZERO,
                allocation_size: 0,
                end_of_file: file_size,
                file_attributes: 0,
                file_id,
                create_contexts: vec![],
            };
            pack_message(&h, &body)
        };
        mock.queue_response(create_resp);

        // Queue all 4 READ responses (some won't be consumed due to cancellation).
        for i in 0..4u64 {
            let chunk = vec![0x42u8; 65536];
            let resp = build_read_response_with_msg_id(NtStatus::SUCCESS, MessageId(i + 1), chunk);
            mock.queue_response(resp);
        }

        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        // Cancel after the first chunk.
        let result = tree
            .read_file_pipelined_with_progress(&mut conn, "cancel_test.bin", |_p| {
                ControlFlow::Break(())
            })
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Cancelled => {} // expected
            other => panic!("expected Cancelled, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_pipelined_with_progress_empty_file() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF5,
            volatile: 0xF6,
        };

        // CREATE response with size=0.
        let create_resp = {
            let mut h = Header::new_request(Command::Create);
            h.flags.set_response();
            h.credits = 32;
            let body = CreateResponse {
                oplock_level: OplockLevel::None,
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
                create_contexts: vec![],
            };
            pack_message(&h, &body)
        };
        mock.queue_response(create_resp);
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut progress_called = false;
        let data = tree
            .read_file_pipelined_with_progress(&mut conn, "empty.bin", |p| {
                progress_called = true;
                assert_eq!(p.bytes_transferred, 0);
                assert_eq!(p.total_bytes, Some(0));
                ControlFlow::Continue(())
            })
            .await
            .unwrap();

        assert!(data.is_empty());
        assert!(progress_called);
    }

    // ── Pipelined write tests ───────────────────────────────────────

    #[tokio::test]
    async fn pipelined_write_four_chunks() {
        let mock = Arc::new(MockTransport::new());
        let tree_id = TreeId(20);
        let file_id = FileId {
            persistent: 0xB00,
            volatile: 0xC00,
        };
        let data_to_write = vec![0x42u8; 256 * 1024]; // 256 KB = 4 chunks

        // CREATE response.
        mock.queue_response(build_create_response(file_id, 0));

        // 4 WRITE responses.
        for i in 0u64..4 {
            mock.queue_response(build_write_response_with_msg_id(MessageId(i + 1), 65536));
        }

        // FLUSH + CLOSE responses.
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id,
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let written = tree
            .write_file_pipelined(&mut conn, "big_write.bin", &data_to_write)
            .await
            .unwrap();

        assert_eq!(written, 256 * 1024);
        // 1 CREATE + 4 WRITEs + 1 FLUSH + 1 CLOSE = 7.
        assert_eq!(mock.sent_count(), 7);

        // Verify that each WRITE request contains the correct data chunk.
        for i in 0..4 {
            let sent = mock.sent_message(i + 1).unwrap(); // skip CREATE at index 0
            let mut cursor = ReadCursor::new(&sent);
            let _header = Header::unpack(&mut cursor).unwrap();
            let req = WriteRequest::unpack(&mut cursor).unwrap();
            assert_eq!(req.data.len(), 65536);
            assert_eq!(req.offset, i as u64 * 65536);
            assert!(req.data.iter().all(|&b| b == 0x42));
        }
    }

    #[tokio::test]
    async fn pipelined_write_last_chunk_smaller() {
        // 100 KB = 1 full chunk (64 KB) + 1 partial chunk (36 KB).
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xD00,
            volatile: 0xE00,
        };
        let data_to_write = vec![0x55u8; 100 * 1024];

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response_with_msg_id(MessageId(1), 65536));
        mock.queue_response(build_write_response_with_msg_id(MessageId(2), 36 * 1024));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let written = tree
            .write_file_pipelined(&mut conn, "partial.bin", &data_to_write)
            .await
            .unwrap();

        assert_eq!(written, 65536 + 36 * 1024);
        assert_eq!(mock.sent_count(), 5); // CREATE + 2 WRITEs + FLUSH + CLOSE
    }

    #[tokio::test]
    async fn a_write_budget_below_the_pipeline_window_bounds_a_pipelined_write() {
        // 512 KB = 8 chunks of 64 KB, against a budget that holds exactly one.
        // On a link this fast the window would keep all 8 outstanding; the
        // budget is the only thing that can say otherwise, and it has to do it
        // without the writer parking on budget its own frames are holding.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF00,
            volatile: 0xF01,
        };
        let data_to_write = vec![0x37u8; 512 * 1024];

        mock.queue_response(build_create_response(file_id, 0));
        for i in 0u64..8 {
            mock.queue_response(build_write_response_with_msg_id(MessageId(i + 1), 65536));
        }
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        conn.set_write_budget(65536);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(21),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let written = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tree.write_file_pipelined(&mut conn, "budgeted.bin", &data_to_write),
        )
        .await
        .expect("a budget below the pipeline window must never wedge the writer")
        .unwrap();

        assert_eq!(written, 512 * 1024);
        // 1 CREATE + 8 WRITEs + 1 FLUSH + 1 CLOSE = 11. Nothing is dropped or
        // re-sent because the budget made the writer wait.
        assert_eq!(mock.sent_count(), 11);
        for i in 0..8 {
            let sent = mock.sent_message(i + 1).unwrap();
            let mut cursor = ReadCursor::new(&sent);
            let _header = Header::unpack(&mut cursor).unwrap();
            let req = WriteRequest::unpack(&mut cursor).unwrap();
            assert_eq!(req.offset, i as u64 * 65536, "chunks must stay in order");
            assert_eq!(req.data.len(), 65536);
        }
    }

    #[tokio::test]
    async fn a_write_budget_below_the_pipeline_window_bounds_a_streamed_write() {
        // Same bound on the callback-driven path, where a chunk already pulled
        // from the consumer has to be held back rather than dropped or re-pulled
        // when the budget hands a response over instead of a permit.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF10,
            volatile: 0xF11,
        };
        let sizes = [100usize, 200, 150, 400, 50, 300];

        mock.queue_response(build_create_response(file_id, 0));
        for size in sizes {
            mock.queue_response(build_write_response(size as u32));
        }
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        conn.set_write_budget(0); // floors at one 64 KiB granule: one frame at a time
        let tree = Tree {
            pin: None,
            tree_id: TreeId(22),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut chunk_iter = sizes
            .iter()
            .map(|&size| Ok(vec![0x9Cu8; size]))
            .collect::<Vec<_>>()
            .into_iter();
        let mut next_chunk =
            move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> { chunk_iter.next() };

        let written = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tree.write_file_streamed(&mut conn, "budgeted_stream.bin", &mut next_chunk),
        )
        .await
        .expect("a budget below the pipeline window must never wedge the writer")
        .unwrap();

        assert_eq!(written, sizes.iter().sum::<usize>() as u64);
        // 1 CREATE + 6 WRITEs + 1 FLUSH + 1 CLOSE = 9.
        assert_eq!(mock.sent_count(), 9);
        let mut expected_offset = 0u64;
        for (i, size) in sizes.iter().enumerate() {
            let sent = mock.sent_message(i + 1).unwrap();
            let mut cursor = ReadCursor::new(&sent);
            let _header = Header::unpack(&mut cursor).unwrap();
            let req = WriteRequest::unpack(&mut cursor).unwrap();
            assert_eq!(
                req.offset, expected_offset,
                "a chunk held back for budget must go out next, at its own offset"
            );
            assert_eq!(req.data.len(), *size);
            expected_offset += *size as u64;
        }
    }

    #[tokio::test]
    async fn a_spent_write_budget_holds_back_a_pipelined_write() {
        // The proof that this path consults the budget at all: with every byte
        // of it held by somebody else, the CREATE goes out and the WRITEs do
        // not, however much room the pipeline window has.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF20,
            volatile: 0xF21,
        };
        let data_to_write = vec![0x11u8; 128 * 1024];

        mock.queue_response(build_create_response(file_id, 0));
        for i in 0u64..2 {
            mock.queue_response(build_write_response_with_msg_id(MessageId(i + 1), 65536));
        }
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let holder = conn.clone();
        let held = holder
            .try_reserve_write_budget(holder.write_budget())
            .expect("an idle connection can afford its whole budget");

        let tree = Tree {
            pin: None,
            tree_id: TreeId(23),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };
        let sent = mock.clone();
        let task = tokio::spawn(async move {
            tree.write_file_pipelined(&mut conn, "held.bin", &data_to_write)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            sent.sent_count(),
            1,
            "only the CREATE may go out while the write budget is spent"
        );

        drop(held);
        let written = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("returning the budget must release the writer")
            .unwrap()
            .unwrap();
        assert_eq!(written, 128 * 1024);
        assert_eq!(sent.sent_count(), 5); // CREATE + 2 WRITEs + FLUSH + CLOSE
    }

    #[tokio::test]
    async fn a_spent_write_budget_holds_back_a_streamed_write() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xF30,
            volatile: 0xF31,
        };

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(500));
        mock.queue_response(build_write_response(700));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let holder = conn.clone();
        let held = holder
            .try_reserve_write_budget(holder.write_budget())
            .expect("an idle connection can afford its whole budget");

        let tree = Tree {
            pin: None,
            tree_id: TreeId(24),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };
        let sent = mock.clone();
        let task = tokio::spawn(async move {
            let mut chunk_iter = vec![Ok(vec![0x1Au8; 500]), Ok(vec![0x1Bu8; 700])].into_iter();
            let mut next_chunk = move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> {
                chunk_iter.next()
            };
            tree.write_file_streamed(&mut conn, "held_stream.bin", &mut next_chunk)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            sent.sent_count(),
            1,
            "only the CREATE may go out while the write budget is spent"
        );

        drop(held);
        let written = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("returning the budget must release the writer")
            .unwrap()
            .unwrap();
        assert_eq!(written, 1200);
        assert_eq!(sent.sent_count(), 5); // CREATE + 2 WRITEs + FLUSH + CLOSE
    }

    // ── Compound request tests ──────────────────────────────────────

    /// Build a compound response frame with proper NextCommand offsets and padding.
    use crate::client::test_helpers::build_compound_response_frame;

    #[tokio::test]
    async fn read_file_compound_returns_file_data() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        // Set up tree.
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        // Build compound response frame: CREATE + READ + CLOSE.
        let file_id = FileId {
            persistent: 0x42,
            volatile: 0x99,
        };
        let file_data = b"Hello, compound!".to_vec();

        let create_resp = build_create_response(file_id, file_data.len() as u64);
        let read_resp = build_read_response(NtStatus::SUCCESS, file_data.clone());
        let close_resp = build_close_response();

        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        let data = tree
            .read_file_compound(&mut conn, "test.txt")
            .await
            .unwrap();

        assert_eq!(data, b"Hello, compound!");
        // Should have sent one compound frame (plus the tree connect).
        assert_eq!(mock.sent_count(), 2); // TreeConnect + compound
    }

    #[tokio::test]
    async fn read_file_compound_handles_empty_file() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // Build compound response: CREATE ok, READ returns END_OF_FILE, CLOSE ok.
        let create_resp = build_create_response(file_id, 0);

        // For END_OF_FILE, we need an error response body.
        let read_resp = build_read_response(NtStatus::END_OF_FILE, vec![]);
        let close_resp = build_close_response();

        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        let data = tree
            .read_file_compound(&mut conn, "empty.txt")
            .await
            .unwrap();

        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn read_file_compound_create_failure_returns_error() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        // Build compound response where CREATE fails with OBJECT_NAME_NOT_FOUND.
        // When CREATE fails, server cascades error to READ and CLOSE.
        let mut create_resp_header = Header::new_request(Command::Create);
        create_resp_header.flags.set_response();
        create_resp_header.credits = 32;
        create_resp_header.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_resp = pack_message(
            &create_resp_header,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut read_resp_header = Header::new_request(Command::Read);
        read_resp_header.flags.set_response();
        read_resp_header.credits = 32;
        read_resp_header.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let read_resp = pack_message(
            &read_resp_header,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let mut close_resp_header = Header::new_request(Command::Close);
        close_resp_header.flags.set_response();
        close_resp_header.credits = 32;
        close_resp_header.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_resp = pack_message(
            &close_resp_header,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        let result = tree.read_file_compound(&mut conn, "nonexistent.txt").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::OBJECT_NAME_NOT_FOUND));
    }

    #[tokio::test]
    async fn read_file_compound_read_failure_issues_standalone_close() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 0x42,
            volatile: 0x99,
        };

        // CREATE succeeds.
        let create_resp = build_create_response(file_id, 1024);

        // READ fails with INSUFFICIENT_RESOURCES.
        let mut read_resp_header = Header::new_request(Command::Read);
        read_resp_header.flags.set_response();
        read_resp_header.credits = 32;
        read_resp_header.status = NtStatus::INSUFFICIENT_RESOURCES;
        let read_resp = pack_message(
            &read_resp_header,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        // CLOSE also fails (cascaded).
        let mut close_resp_header = Header::new_request(Command::Close);
        close_resp_header.flags.set_response();
        close_resp_header.credits = 32;
        close_resp_header.status = NtStatus::INSUFFICIENT_RESOURCES;
        let close_resp = pack_message(
            &close_resp_header,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );

        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        // Queue a standalone CLOSE response for the cleanup.
        mock.queue_response(build_close_response());

        let result = tree.read_file_compound(&mut conn, "problem.txt").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::INSUFFICIENT_RESOURCES));

        // Should have sent: TreeConnect + compound + standalone CLOSE = 3.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn read_file_compound_sends_correct_request_structure() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        let create_resp = build_create_response(file_id, 5);
        let read_resp = build_read_response(NtStatus::SUCCESS, vec![1, 2, 3, 4, 5]);
        let close_resp = build_close_response();
        let frame = build_compound_response_frame(&[create_resp, read_resp, close_resp]);
        mock.queue_response(frame);

        tree.read_file_compound(&mut conn, "verify.txt")
            .await
            .unwrap();

        // The second sent message is the compound request.
        let compound = mock.sent_message(1).unwrap();

        // Verify it contains 3 headers linked by NextCommand.
        let mut cursor = ReadCursor::new(&compound);
        let h1 = Header::unpack(&mut cursor).unwrap();
        assert_eq!(h1.command, Command::Create);
        assert!(!h1.flags.is_related());
        assert!(h1.next_command > 0);
        assert_eq!(h1.tree_id, Some(TreeId(7)));

        let off2 = h1.next_command as usize;
        let mut cursor2 = ReadCursor::new(&compound[off2..]);
        let h2 = Header::unpack(&mut cursor2).unwrap();
        assert_eq!(h2.command, Command::Read);
        assert!(h2.flags.is_related());
        assert!(h2.next_command > 0);

        // Verify READ uses sentinel FileId.
        let read_parsed = ReadRequest::unpack(&mut cursor2).unwrap();
        assert_eq!(read_parsed.file_id, FileId::SENTINEL);

        let off3 = off2 + h2.next_command as usize;
        let mut cursor3 = ReadCursor::new(&compound[off3..]);
        let h3 = Header::unpack(&mut cursor3).unwrap();
        assert_eq!(h3.command, Command::Close);
        assert!(h3.flags.is_related());
        assert_eq!(h3.next_command, 0);

        // Verify CLOSE uses sentinel FileId.
        let close_parsed = CloseRequest::unpack(&mut cursor3).unwrap();
        assert_eq!(close_parsed.file_id, FileId::SENTINEL);
    }

    /// 8 MiB, the `MaxReadSize` real servers commonly negotiate.
    const EIGHT_MIB: u32 = 8 * 1024 * 1024;

    /// The `CreditCharge` each sub-request of a compound frame carries, in order.
    fn compound_credit_charges(frame: &[u8]) -> Vec<u16> {
        let mut charges = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&frame[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            charges.push(header.credit_charge.0);
            if header.next_command == 0 {
                break;
            }
            offset += header.next_command as usize;
        }
        charges
    }

    /// The `ReadRequest` of a compound frame whose second sub-request is a READ.
    fn compound_read_request(frame: &[u8]) -> ReadRequest {
        let mut cursor = ReadCursor::new(frame);
        let h1 = Header::unpack(&mut cursor).unwrap();
        let mut cursor2 = ReadCursor::new(&frame[h1.next_command as usize..]);
        Header::unpack(&mut cursor2).unwrap();
        ReadRequest::unpack(&mut cursor2).unwrap()
    }

    #[tokio::test]
    async fn read_file_compound_sized_rejects_a_file_that_grew_past_the_expected_size() {
        // The caller's scan said 1000 bytes; by the time the READ ran the file
        // was 5000. A 1000-byte READ comes back full, which is exactly what a
        // complete read of a 1000-byte file looks like, so nothing downstream
        // could tell the difference. The CREATE response's end-of-file is the
        // server's authoritative size, and it rides in the same frame.
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection_with_max_read(&mock, EIGHT_MIB);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        let create_resp = build_create_response(file_id, 5000);
        let read_resp = build_read_response(NtStatus::SUCCESS, vec![0xAB; 1000]);
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            read_resp,
            close_resp,
        ]));

        let err = tree
            .read_file_compound_sized(&mut conn, "grew.bin", 1000)
            .await
            .expect_err("a file that outgrew the request must not come back truncated");

        assert_eq!(err.kind(), crate::ErrorKind::TooLarge);
        assert!(
            matches!(
                err,
                Error::FileTooLargeForSingleRead {
                    size: 5000,
                    requested: 1000
                }
            ),
            "expected FileTooLargeForSingleRead, got {err:?}"
        );
    }

    #[tokio::test]
    async fn read_file_compound_sized_charges_only_for_what_it_asks_for() {
        // The charge follows the expected response size (MS-SMB2 § 3.2.4.1.2),
        // so asking for 4 MiB of an 8 MiB window costs 64 credits for the READ
        // plus one each for CREATE and CLOSE, not the 130 a full-window read
        // would take.
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection_with_max_read(&mock, EIGHT_MIB);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        let create_resp = build_create_response(file_id, 5);
        let read_resp = build_read_response(NtStatus::SUCCESS, vec![1, 2, 3, 4, 5]);
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            read_resp,
            close_resp,
        ]));

        tree.read_file_compound_sized(&mut conn, "small.bin", 4 * 1024 * 1024)
            .await
            .unwrap();

        let compound = mock.sent_message(1).unwrap();
        assert_eq!(compound_credit_charges(&compound), vec![1, 64, 1]);
        assert_eq!(compound_read_request(&compound).length, 4 * 1024 * 1024);
    }

    #[tokio::test]
    async fn read_file_compound_still_asks_for_the_whole_window() {
        // The unsized entry point knows nothing about the file, so it keeps
        // asking for a full `MaxReadSize` and paying the full charge.
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection_with_max_read(&mock, EIGHT_MIB);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        let create_resp = build_create_response(file_id, 5);
        let read_resp = build_read_response(NtStatus::SUCCESS, vec![1, 2, 3, 4, 5]);
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            read_resp,
            close_resp,
        ]));

        let data = tree
            .read_file_compound(&mut conn, "small.bin")
            .await
            .unwrap();

        assert_eq!(data, vec![1, 2, 3, 4, 5]);
        let compound = mock.sent_message(1).unwrap();
        assert_eq!(compound_credit_charges(&compound), vec![1, 128, 1]);
        assert_eq!(compound_read_request(&compound).length, EIGHT_MIB);
    }

    #[tokio::test]
    async fn read_file_compound_sized_never_asks_for_zero_bytes() {
        // An empty file still needs a READ on the wire, and a zero-length READ
        // charging zero credits is not a request the spec allows.
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection_with_max_read(&mock, EIGHT_MIB);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        let create_resp = build_create_response(file_id, 0);
        let read_resp = build_read_response(NtStatus::END_OF_FILE, vec![]);
        let close_resp = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            read_resp,
            close_resp,
        ]));

        let data = tree
            .read_file_compound_sized(&mut conn, "empty.txt", 0)
            .await
            .unwrap();

        assert!(data.is_empty());
        let compound = mock.sent_message(1).unwrap();
        assert_eq!(compound_credit_charges(&compound), vec![1, 1, 1]);
        assert_eq!(compound_read_request(&compound).length, 1);
    }

    // ── Compound write tests ────────────────────────────────────────

    /// What a server answers a small write that goes through: the
    /// CREATE + WRITE + FLUSH chain, then the standalone CLOSE.
    fn queue_small_write_replies(mock: &MockTransport, file_id: FileId, written: u32) {
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id, 0),
            build_write_response(written),
            build_flush_response(),
        ]));
        mock.queue_response(build_close_response());
    }

    /// The FileId of a sent frame that is a CLOSE on its own.
    fn standalone_close(frame: &[u8]) -> FileId {
        let mut cursor = ReadCursor::new(frame);
        let header = Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::Close);
        assert_eq!(header.next_command, 0, "expected a frame of its own");
        CloseRequest::unpack(&mut cursor).unwrap().file_id
    }

    #[tokio::test]
    async fn write_file_compound_returns_bytes_written() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 0x42,
            volatile: 0x99,
        };
        let file_data = b"Hello, compound write!";
        queue_small_write_replies(&mock, file_id, file_data.len() as u32);

        let written = tree
            .write_file_compound(&mut conn, "test.txt", file_data)
            .await
            .unwrap();

        assert_eq!(written, file_data.len() as u64);
        // TreeConnect + the CREATE + WRITE + FLUSH chain + CLOSE.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn write_file_compound_empty_file() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        queue_small_write_replies(&mock, file_id, 0);

        let written = tree
            .write_file_compound(&mut conn, "empty.txt", b"")
            .await
            .unwrap();

        assert_eq!(written, 0);
    }

    #[tokio::test]
    async fn write_file_compound_create_failure_returns_error() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        // When CREATE fails, the server cascades the error down the chain.
        mock.queue_response(build_cascaded_compound_failure(
            NtStatus::OBJECT_NAME_NOT_FOUND,
        ));

        let result = tree
            .write_file_compound(&mut conn, "bad/path.txt", b"data")
            .await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::OBJECT_NAME_NOT_FOUND));
        // No handle was opened, so nothing follows the chain.
        assert_eq!(mock.sent_count(), 2);
    }

    #[tokio::test]
    async fn write_file_compound_write_failure_issues_standalone_close() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 0x42,
            volatile: 0x99,
        };
        // The WRITE fails, and the FLUSH after it (cascaded).
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id, 0),
            build_status_reply(Command::Write, NtStatus::INSUFFICIENT_RESOURCES),
            build_status_reply(Command::Flush, NtStatus::INSUFFICIENT_RESOURCES),
        ]));
        mock.queue_response(build_close_response());

        let err = tree
            .write_file_compound(&mut conn, "problem.txt", b"data")
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                Error::Protocol {
                    status: NtStatus::INSUFFICIENT_RESOURCES,
                    command: Command::Write,
                }
            ),
            "the WRITE's failure is the one reported: {err:?}"
        );
        // TreeConnect + the chain + a standalone CLOSE.
        assert_eq!(mock.sent_count(), 3);
        assert_eq!(standalone_close(&mock.sent_message(2).unwrap()), file_id);
    }

    #[tokio::test]
    async fn write_file_compound_sends_correct_request_structure() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        queue_small_write_replies(&mock, file_id, 5);

        tree.write_file_compound(&mut conn, "verify.txt", &[1, 2, 3, 4, 5])
            .await
            .unwrap();

        // The second sent message is the CREATE + WRITE + FLUSH chain.
        let compound = mock.sent_message(1).unwrap();

        let mut cursor = ReadCursor::new(&compound);
        let h1 = Header::unpack(&mut cursor).unwrap();
        assert_eq!(h1.command, Command::Create);
        assert!(!h1.flags.is_related());
        assert!(h1.next_command > 0);
        assert_eq!(h1.tree_id, Some(TreeId(7)));

        let off2 = h1.next_command as usize;
        let mut cursor2 = ReadCursor::new(&compound[off2..]);
        let h2 = Header::unpack(&mut cursor2).unwrap();
        assert_eq!(h2.command, Command::Write);
        assert!(h2.flags.is_related());
        assert!(h2.next_command > 0);

        // The WRITE uses the sentinel FileId.
        let write_parsed = WriteRequest::unpack(&mut cursor2).unwrap();
        assert_eq!(write_parsed.file_id, FileId::SENTINEL);
        assert_eq!(write_parsed.data, vec![1, 2, 3, 4, 5]);

        let off3 = off2 + h2.next_command as usize;
        let mut cursor3 = ReadCursor::new(&compound[off3..]);
        let h3 = Header::unpack(&mut cursor3).unwrap();
        assert_eq!(h3.command, Command::Flush);
        assert!(h3.flags.is_related());
        assert_eq!(h3.next_command, 0, "the FLUSH ends the chain");

        // The FLUSH uses the sentinel FileId.
        let flush_parsed = FlushRequest::unpack(&mut cursor3).unwrap();
        assert_eq!(flush_parsed.file_id, FileId::SENTINEL);

        // Then the CLOSE on its own, on the opened handle.
        assert_eq!(standalone_close(&mock.sent_message(2).unwrap()), file_id);
    }

    /// Regression anchor for a server reset on small-file copies (Samba up to
    /// 4.15, and NAS firmware built on it, 2026-10-19). A FLUSH with anything
    /// after it in a compound is refused, and on those servers the refusal
    /// crashes the connection, so the FLUSH ends every chain, on the first
    /// write and any later one.
    #[tokio::test]
    async fn a_small_write_ends_its_chain_on_the_flush() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let id = |n| FileId {
            persistent: n,
            volatile: n,
        };
        queue_small_write_replies(&mock, id(1), 3);
        queue_small_write_replies(&mock, id(2), 5);
        tree.write_file_compound(&mut conn, "first.txt", b"abc")
            .await
            .unwrap();
        tree.write_file_compound_exclusive(&mut conn, "second.txt", b"hello")
            .await
            .unwrap();

        let sent: Vec<Vec<Command>> = (1..mock.sent_count())
            .map(|i| compound_commands(&mock.sent_message(i).unwrap()))
            .collect();
        let chain = vec![Command::Create, Command::Write, Command::Flush];
        assert_eq!(
            sent,
            vec![
                chain.clone(),
                vec![Command::Close],
                chain,
                vec![Command::Close]
            ]
        );
    }

    #[tokio::test]
    async fn a_refused_flush_fails_the_write_and_still_closes_the_handle() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 7,
            volatile: 8,
        };
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id, 0),
            build_write_response(3),
            build_status_reply(Command::Flush, NtStatus::DISK_FULL),
        ]));
        mock.queue_response(build_close_response());

        let err = tree
            .write_file_compound(&mut conn, "unflushed.txt", b"abc")
            .await
            .expect_err("data the server couldn't persist must not read as written");

        assert!(
            matches!(
                err,
                Error::Protocol {
                    status: NtStatus::DISK_FULL,
                    command: Command::Flush,
                }
            ),
            "got: {err:?}"
        );
        assert_eq!(
            standalone_close(&mock.sent_message(2).unwrap()),
            file_id,
            "the handle is closed even though the FLUSH failed"
        );
    }

    /// The CREATE disposition the compound write at `sent_index` asked for.
    fn compound_create_disposition(mock: &MockTransport, sent_index: usize) -> CreateDisposition {
        let compound = mock.sent_message(sent_index).unwrap();
        let mut cursor = ReadCursor::new(&compound);
        let header = Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::Create);
        CreateRequest::unpack(&mut cursor)
            .unwrap()
            .create_disposition
    }

    /// The CREATE + WRITE + FLUSH chain's replies when the CREATE failed with
    /// `status` and the server cascaded it down the chain.
    fn build_cascaded_compound_failure(status: NtStatus) -> Vec<u8> {
        build_compound_response_frame(&[
            build_status_reply(Command::Create, status),
            build_status_reply(Command::Write, status),
            build_status_reply(Command::Flush, status),
        ])
    }

    /// Every sub-request's command in a sent frame, in order.
    fn compound_commands(frame: &[u8]) -> Vec<Command> {
        let mut commands = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&frame[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            commands.push(header.command);
            if header.next_command == 0 {
                break;
            }
            offset += header.next_command as usize;
        }
        commands
    }

    /// A reply carrying only `status`, for `command`.
    fn build_status_reply(command: Command, status: NtStatus) -> Vec<u8> {
        let mut h = Header::new_request(command);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;
        pack_message(
            &h,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        )
    }

    #[tokio::test]
    async fn write_file_compound_replaces_whatever_holds_the_name() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        queue_small_write_replies(&mock, file_id, 3);
        tree.write_file_compound(&mut conn, "replace.txt", b"abc")
            .await
            .unwrap();

        assert_eq!(
            compound_create_disposition(&mock, 1),
            CreateDisposition::FileOverwriteIf
        );
    }

    #[tokio::test]
    async fn write_file_compound_exclusive_asks_for_a_new_file() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        queue_small_write_replies(&mock, file_id, 3);
        let written = tree
            .write_file_compound_exclusive(&mut conn, "new.txt", b"abc")
            .await
            .unwrap();

        assert_eq!(written, 3);
        assert_eq!(
            compound_create_disposition(&mock, 1),
            CreateDisposition::FileCreate,
            "an exclusive compound write must use FileCreate, never FileOverwriteIf"
        );
    }

    #[tokio::test]
    async fn write_file_compound_exclusive_onto_a_taken_name_is_already_exists_and_sends_nothing_more(
    ) {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        mock.queue_response(build_cascaded_compound_failure(
            NtStatus::OBJECT_NAME_COLLISION,
        ));
        let err = tree
            .write_file_compound_exclusive(&mut conn, "taken.txt", b"abc")
            .await
            .expect_err("a name that exists must refuse an exclusive write");

        assert_eq!(err.kind(), crate::ErrorKind::AlreadyExists, "got: {err}");
        assert!(
            matches!(
                err,
                Error::Protocol {
                    command: Command::Create,
                    ..
                }
            ),
            "the refusal must name the CREATE, so a caller knows nothing was opened: {err:?}"
        );
        // TreeConnect + the compound, and no standalone CLOSE: no handle was opened.
        assert_eq!(mock.sent_count(), 2);
    }

    // ── BUFFER_OVERFLOW tests ───────────────────────────────────────

    #[tokio::test]
    async fn stat_accepts_buffer_overflow_as_partial_data() {
        // STATUS_BUFFER_OVERFLOW is a warning, not an error. The response
        // body contains valid partial data and should be parsed.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xCC,
            volatile: 0xDD,
        };

        // STAT = compound CREATE + QUERY_INFO(basic, BUFFER_OVERFLOW) + QUERY_INFO(standard) + CLOSE
        let create_resp = build_create_response(file_id, 0);

        let basic = build_file_basic_info(
            132_000_000_000_000_000,
            132_100_000_000_000_000,
            133_000_000_000_000_000,
            133_000_000_000_000_000,
            0x20, // ARCHIVE
        );
        let basic_resp = build_query_info_response_with_status(NtStatus::BUFFER_OVERFLOW, basic);

        let std_info = build_file_standard_info(4096, 1024, 1, false, false);
        let std_resp = build_query_info_response(std_info);

        let close_resp = build_close_response();

        let frame = build_compound_response_frame(&[
            create_resp,
            basic_resp,
            std_resp,
            no_tag(),
            close_resp,
        ]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(10),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        // Should succeed despite BUFFER_OVERFLOW on the basic info query.
        let info = tree.stat(&mut conn, "partial.txt").await.unwrap();
        assert_eq!(info.size, 1024);
        assert!(!info.is_directory);
        assert_eq!(info.created, FileTime(132_000_000_000_000_000));
        // One compound frame sent.
        assert_eq!(mock.sent_count(), 1);
    }

    // ── Streamed write tests ───────────────────────────────────────

    #[tokio::test]
    async fn write_file_streamed_basic() {
        // Provide 3 small chunks, verify CREATE + 3 WRITEs + CLOSE.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };

        let chunk1 = vec![0x01; 100];
        let chunk2 = vec![0x02; 200];
        let chunk3 = vec![0x03; 150];
        let chunks = vec![Ok(chunk1.clone()), Ok(chunk2.clone()), Ok(chunk3.clone())];
        let mut chunk_iter = chunks.into_iter();

        // Queue: CREATE, 3x WRITE, FLUSH, CLOSE.
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_write_response(200));
        mock.queue_response(build_write_response(150));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(30),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut next_chunk =
            move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> { chunk_iter.next() };

        let written = tree
            .write_file_streamed(&mut conn, "streamed.bin", &mut next_chunk)
            .await
            .unwrap();

        assert_eq!(written, 450); // 100 + 200 + 150

        // Verify CREATE + 3 WRITEs + FLUSH + CLOSE = 6 messages.
        assert_eq!(mock.sent_count(), 6);

        // Verify WRITE offsets and data.
        // Message 0 = CREATE, 1..3 = WRITEs, 4 = FLUSH, 5 = CLOSE.
        let sent1 = mock.sent_message(1).unwrap();
        let mut cursor1 = ReadCursor::new(&sent1);
        let _ = Header::unpack(&mut cursor1).unwrap();
        let req1 = WriteRequest::unpack(&mut cursor1).unwrap();
        assert_eq!(req1.offset, 0);
        assert_eq!(req1.data, chunk1);

        let sent2 = mock.sent_message(2).unwrap();
        let mut cursor2 = ReadCursor::new(&sent2);
        let _ = Header::unpack(&mut cursor2).unwrap();
        let req2 = WriteRequest::unpack(&mut cursor2).unwrap();
        assert_eq!(req2.offset, 100);
        assert_eq!(req2.data, chunk2);

        let sent3 = mock.sent_message(3).unwrap();
        let mut cursor3 = ReadCursor::new(&sent3);
        let _ = Header::unpack(&mut cursor3).unwrap();
        let req3 = WriteRequest::unpack(&mut cursor3).unwrap();
        assert_eq!(req3.offset, 300);
        assert_eq!(req3.data, chunk3);

        // Verify last message is CLOSE.
        let sent5 = mock.sent_message(5).unwrap();
        let mut cursor5 = ReadCursor::new(&sent5);
        let h5 = Header::unpack(&mut cursor5).unwrap();
        assert_eq!(h5.command, Command::Close);
    }

    #[tokio::test]
    async fn write_file_streamed_empty() {
        // Callback returns None immediately -> CREATE + FLUSH + CLOSE (empty file).
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xCC,
            volatile: 0xDD,
        };

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(31),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut next_chunk = || -> Option<std::result::Result<Vec<u8>, std::io::Error>> { None };

        let written = tree
            .write_file_streamed(&mut conn, "empty_stream.bin", &mut next_chunk)
            .await
            .unwrap();

        assert_eq!(written, 0);
        // CREATE + FLUSH + CLOSE = 3 messages.
        assert_eq!(mock.sent_count(), 3);

        // Verify CREATE then FLUSH then CLOSE.
        let sent0 = mock.sent_message(0).unwrap();
        let mut c0 = ReadCursor::new(&sent0);
        let h0 = Header::unpack(&mut c0).unwrap();
        assert_eq!(h0.command, Command::Create);

        let sent1 = mock.sent_message(1).unwrap();
        let mut c1 = ReadCursor::new(&sent1);
        let h1 = Header::unpack(&mut c1).unwrap();
        assert_eq!(h1.command, Command::Flush);

        let sent2 = mock.sent_message(2).unwrap();
        let mut c2 = ReadCursor::new(&sent2);
        let h2 = Header::unpack(&mut c2).unwrap();
        assert_eq!(h2.command, Command::Close);
    }

    #[tokio::test]
    async fn write_file_streamed_callback_error() {
        // Callback returns Ok on first call, Err on second.
        // Verify: handle is closed (CLOSE sent) and error is propagated.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0xEE,
            volatile: 0xFF,
        };

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(64));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(32),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut call_count = 0u32;
        let mut next_chunk = move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> {
            call_count += 1;
            match call_count {
                1 => Some(Ok(vec![0x42; 64])),
                2 => Some(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "source stream broke",
                ))),
                _ => None,
            }
        };

        let result = tree
            .write_file_streamed(&mut conn, "error_stream.bin", &mut next_chunk)
            .await;

        assert!(result.is_err(), "expected error from callback to propagate");

        // Verify CLOSE was still sent (handle cleanup).
        // Messages: CREATE + WRITE + CLOSE = 3.
        assert_eq!(mock.sent_count(), 3);

        let sent_last = mock.sent_message(2).unwrap();
        let mut cl = ReadCursor::new(&sent_last);
        let hl = Header::unpack(&mut cl).unwrap();
        assert_eq!(hl.command, Command::Close);
    }

    #[tokio::test]
    async fn write_file_streamed_callback_error_is_not_connection_lost() {
        // A callback error is a consumer issue, not a connection failure.
        // The error kind should NOT be ConnectionLost — the connection is
        // still usable after write_file_streamed drains in-flight responses.
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 0x11,
            volatile: 0x22,
        };

        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(64));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(40),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut call_count = 0u32;
        let mut next_chunk = move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> {
            call_count += 1;
            match call_count {
                1 => Some(Ok(vec![0x42; 64])),
                2 => Some(Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "user cancelled",
                ))),
                _ => None,
            }
        };

        let err = tree
            .write_file_streamed(&mut conn, "cancel_test.bin", &mut next_chunk)
            .await
            .unwrap_err();

        // The error should NOT be classified as ConnectionLost.
        // A callback cancellation doesn't break the SMB connection — all
        // in-flight responses were drained and the handle was closed cleanly.
        assert_ne!(
            err.kind(),
            crate::ErrorKind::ConnectionLost,
            "callback error should not be classified as ConnectionLost; the connection is still healthy"
        );
    }

    #[tokio::test]
    async fn write_file_streamed_callback_error_connection_still_usable() {
        // After a callback error in write_file_streamed, the connection should
        // be in a clean state: all in-flight WRITE responses drained, handle
        // CLOSEd. A subsequent operation (read_file) should work.
        let mock = Arc::new(MockTransport::new());
        let write_file_id = FileId {
            persistent: 0x33,
            volatile: 0x44,
        };
        let read_file_id = FileId {
            persistent: 0x55,
            volatile: 0x66,
        };

        // Phase 1: Streamed write that errors after 2 chunks.
        // With max_write=65536, 2 small chunks fit in the initial window.
        mock.queue_response(build_create_response(write_file_id, 0));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_write_response(200));
        mock.queue_response(build_close_response());

        // Phase 2: A subsequent read_file (compound: CREATE+READ+CLOSE).
        let read_data = b"hello from the server";
        mock.queue_response(build_compound_read_response(
            read_file_id,
            read_data.to_vec(),
        ));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(41),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        // Streamed write: 2 chunks succeed, then callback errors.
        let mut call_count = 0u32;
        let mut next_chunk = move || -> Option<std::result::Result<Vec<u8>, std::io::Error>> {
            call_count += 1;
            match call_count {
                1 => Some(Ok(vec![0xAA; 100])),
                2 => Some(Ok(vec![0xBB; 200])),
                3 => Some(Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "cancelled by user",
                ))),
                _ => None,
            }
        };

        let write_result = tree
            .write_file_streamed(&mut conn, "partial.bin", &mut next_chunk)
            .await;
        assert!(
            write_result.is_err(),
            "write should fail due to callback error"
        );

        // Now verify the connection still works: read a file.
        let data = tree
            .read_file_compound(&mut conn, "other.txt")
            .await
            .unwrap();
        assert_eq!(data, read_data);
    }

    /// Builds a compound CREATE+READ+CLOSE response (single transport frame)
    /// for use with `read_file_compound`.
    fn build_compound_read_response(file_id: FileId, data: Vec<u8>) -> Vec<u8> {
        use crate::msg::read::ReadResponse;

        // CREATE response (chained: next_command points to READ response)
        let mut h1 = Header::new_request(Command::Create);
        h1.flags.set_response();
        h1.credits = 32;
        let create_body = CreateResponse {
            oplock_level: OplockLevel::None,
            flags: 0u8,
            create_action: CreateAction::FileOpened,
            creation_time: crate::pack::FileTime(0),
            last_access_time: crate::pack::FileTime(0),
            last_write_time: crate::pack::FileTime(0),
            change_time: crate::pack::FileTime(0),
            allocation_size: 0,
            end_of_file: data.len() as u64,
            file_attributes: 0x80,
            file_id,
            create_contexts: vec![],
        };
        let create_bytes = pack_message(&h1, &create_body);

        // READ response (chained: next_command points to CLOSE response)
        let mut h2 = Header::new_request(Command::Read);
        h2.flags.set_response();
        h2.credits = 32;
        let read_body = ReadResponse {
            data_offset: 0x50,
            data: data.clone(),
            data_remaining: 0,
            flags: 0,
        };
        let read_bytes = pack_message(&h2, &read_body);

        // CLOSE response (last in chain)
        let close_bytes = build_close_response();

        // Patch next_command offsets for compounding
        let mut frame = Vec::new();

        let mut create_buf = create_bytes;
        let create_len = create_buf.len();
        // Align to 8 bytes
        let padded_create_len = (create_len + 7) & !7;
        create_buf.resize(padded_create_len, 0);
        // Set NextCommand in header (offset 20, 4 bytes LE)
        let next_cmd = padded_create_len as u32;
        create_buf[20..24].copy_from_slice(&next_cmd.to_le_bytes());
        // Set RELATED_OPERATIONS flag on subsequent headers? No — compound READ
        // uses the same file_id from CREATE, but read_file_compound sends
        // FileId::SENTINEL which gets filled in by the server. For mock,
        // we just need the responses to parse correctly.
        frame.extend_from_slice(&create_buf);

        let mut read_buf = read_bytes;
        let read_len = read_buf.len();
        let padded_read_len = (read_len + 7) & !7;
        read_buf.resize(padded_read_len, 0);
        let next_cmd2 = padded_read_len as u32;
        read_buf[20..24].copy_from_slice(&next_cmd2.to_le_bytes());
        frame.extend_from_slice(&read_buf);

        frame.extend_from_slice(&close_bytes);

        frame
    }

    // ── Tree::download (streaming via &mut Connection) ─────────────────────

    /// Happy path: `Tree::download` returns a `FileDownload` that yields
    /// all chunks of a small file in order and closes the handle cleanly.
    #[tokio::test]
    async fn tree_download_streams_small_file() {
        let mock = Arc::new(MockTransport::new());

        let file_id = FileId {
            persistent: 0xA1,
            volatile: 0xB2,
        };
        let payload = b"streaming hello from Tree::download".to_vec();

        // CREATE (open_file) — server returns handle + size.
        mock.queue_response(build_create_response(file_id, payload.len() as u64));
        // Single READ covering the whole file (payload fits in one
        // max_read_size=65536 chunk).
        mock.queue_response(build_read_response(NtStatus::SUCCESS, payload.clone()));
        // CLOSE after the last chunk.
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(11),
            share_name: "share".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut download = tree
            .download(&mut conn, "hello.txt")
            .await
            .expect("download");
        assert_eq!(download.size(), payload.len() as u64);

        let mut received = Vec::new();
        while let Some(chunk) = download.next_chunk().await {
            let bytes = chunk.expect("chunk");
            received.extend_from_slice(&bytes);
        }
        assert_eq!(received, payload);

        // CREATE + READ + CLOSE = 3 messages on the wire.
        assert_eq!(mock.sent_count(), 3);
        mock.assert_fully_consumed();
    }

    /// Error path: if CREATE fails, `Tree::download` surfaces the NTSTATUS
    /// as `Error::Protocol` without ever constructing a `FileDownload`.
    #[tokio::test]
    async fn tree_download_create_failure_returns_protocol_error() {
        let mock = Arc::new(MockTransport::new());

        let mut create_hdr = Header::new_request(Command::Create);
        create_hdr.flags.set_response();
        create_hdr.credits = 32;
        create_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_err = pack_message(
            &create_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        mock.queue_response(create_err);

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(12),
            share_name: "share".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let result = tree.download(&mut conn, "missing.txt").await;
        let err = result.err().expect("expected error");
        assert_eq!(err.status(), Some(NtStatus::OBJECT_NAME_NOT_FOUND));
    }

    /// Dropping a `FileDownload` mid-stream (before draining all chunks)
    /// must not panic, and the client stays healthy.
    #[tokio::test]
    async fn tree_download_drop_mid_stream_does_not_panic() {
        let mock = Arc::new(MockTransport::new());

        let file_id = FileId {
            persistent: 0xC3,
            volatile: 0xD4,
        };
        // 3x max_read_size payload so at least one READ remains unsent
        // after the caller drops early.
        let total = 3 * 65536usize;
        mock.queue_response(build_create_response(file_id, total as u64));
        // Queue one READ response; we'll consume only that one and drop.
        mock.queue_response(build_read_response(NtStatus::SUCCESS, vec![0xAB; 65536]));

        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(13),
            share_name: "share".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };

        let mut download = tree.download(&mut conn, "big.bin").await.expect("download");

        let first = download
            .next_chunk()
            .await
            .expect("first chunk exists")
            .expect("first chunk ok");
        assert_eq!(first.len(), 65536);

        // Drop mid-stream -- must not panic.
        drop(download);
    }

    /// Two `Tree::download` futures on cloned `Connection`s must both
    /// complete with correct data, proving the headline reason for adding
    /// `Tree::download`: concurrent downloads on one SMB session.
    ///
    /// The mock transport routes responses via the Phase 3 receiver task
    /// (shared across all `Connection::clone()`s), so msg-id demux is what
    /// wires each CREATE/READ/CLOSE to the right waiter. We queue the
    /// responses AFTER the sends land (just like
    /// `concurrent_execute_on_one_connection_all_succeed` in
    /// `connection.rs`) so `enable_auto_rewrite_msg_id` can stamp them in
    /// FIFO send order. Both downloads use the same payload, so it doesn't
    /// matter which task's READ lands in which slot — whoever gets routed
    /// their msg_id wins the correct bytes.
    ///
    /// Gotcha/Why: if the two tasks fetched DIFFERENT payloads, the FIFO
    /// auto-rewrite in `MockTransport` would mis-pair sends and responses
    /// unless we hand-serialized each phase across tasks, which would
    /// defeat the concurrency the test is meant to prove. Keeping the
    /// payloads identical lets both downloads race freely while we still
    /// assert correctness of the whole pipeline.
    #[tokio::test(flavor = "multi_thread")]
    async fn tree_download_concurrent_on_cloned_connections() {
        use std::time::{Duration, Instant};

        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let params = crate::client::connection::NegotiatedParams {
            dialect: crate::types::Dialect::Smb2_0_2,
            max_read_size: 65536,
            max_write_size: 65536,
            max_transact_size: 65536,
            server_guid: crate::pack::Guid::ZERO,
            signing_required: false,
            capabilities: crate::types::flags::Capabilities::default(),
            gmac_negotiated: false,
            cipher: None,
            compression_supported: false,
        };
        let mut conn_primary = crate::client::connection::Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn_primary.set_test_params(params);
        conn_primary.set_session_id(crate::types::SessionId(0x1234));
        conn_primary.set_credits(512);
        let mut conn_secondary = conn_primary.clone();

        let tree = Arc::new(Tree {
            pin: None,
            tree_id: TreeId(14),
            share_name: "share".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        });

        let payload = b"shared-body-for-both-readers".to_vec();

        let file_id_1 = FileId {
            persistent: 0x0A,
            volatile: 0x1A,
        };
        let file_id_2 = FileId {
            persistent: 0x0B,
            volatile: 0x1B,
        };

        let tree_a = Arc::clone(&tree);
        let payload_a = payload.clone();
        let handle_a = tokio::spawn(async move {
            let mut dl = tree_a
                .download(&mut conn_primary, "same.txt")
                .await
                .expect("download a");
            let mut buf = Vec::new();
            while let Some(c) = dl.next_chunk().await {
                buf.extend_from_slice(&c.expect("chunk a"));
            }
            assert_eq!(buf, payload_a);
        });

        let tree_b = Arc::clone(&tree);
        let payload_b = payload.clone();
        let handle_b = tokio::spawn(async move {
            let mut dl = tree_b
                .download(&mut conn_secondary, "same.txt")
                .await
                .expect("download b");
            let mut buf = Vec::new();
            while let Some(c) = dl.next_chunk().await {
                buf.extend_from_slice(&c.expect("chunk b"));
            }
            assert_eq!(buf, payload_b);
        });

        // Wait for both CREATE sends before queuing responses.
        let deadline = Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 2 {
            if Instant::now() > deadline {
                panic!("CREATE sends did not land: {}", mock.sent_count());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        mock.queue_response(build_create_response(file_id_1, payload.len() as u64));
        mock.queue_response(build_create_response(file_id_2, payload.len() as u64));

        // Wait for both READs, then answer with identical payloads.
        let deadline = Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 4 {
            if Instant::now() > deadline {
                panic!("READ sends did not land: {}", mock.sent_count());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        mock.queue_response(build_read_response(NtStatus::SUCCESS, payload.clone()));
        mock.queue_response(build_read_response(NtStatus::SUCCESS, payload.clone()));

        // Both downloads issue a CLOSE after the final chunk.
        let deadline = Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 6 {
            if Instant::now() > deadline {
                panic!("CLOSE sends did not land: {}", mock.sent_count());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        mock.queue_response(build_close_response());
        mock.queue_response(build_close_response());

        handle_a.await.expect("task a panicked");
        handle_b.await.expect("task b panicked");

        assert_eq!(mock.sent_count(), 6); // 2 CREATE + 2 READ + 2 CLOSE
    }

    // ── Dates from the CREATE a read already sent ───────────────────────

    #[tokio::test]
    async fn a_compound_read_hands_out_the_dates_its_create_returned() {
        use crate::client::test_helpers::{build_create_response_dated, CREATE_TIMES};
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        mock.queue_response(build_compound_response_frame(&[
            build_create_response_dated(file_id, 5),
            build_read_response(NtStatus::SUCCESS, vec![1, 2, 3, 4, 5]),
            build_close_response(),
        ]));

        let (data, info) = tree
            .read_file_compound_with_info(&mut conn, "dated.txt")
            .await
            .unwrap();

        assert_eq!(data, [1, 2, 3, 4, 5]);
        let [created, accessed, modified, _] = CREATE_TIMES;
        assert_eq!(
            (
                info.size,
                info.is_directory,
                info.created,
                info.modified,
                info.accessed
            ),
            (5, false, created, modified, accessed)
        );
        assert_eq!(mock.sent_count(), 2, "tree connect + one compound, no stat");
    }

    #[tokio::test]
    async fn a_download_hands_out_the_dates_its_create_returned() {
        use crate::client::test_helpers::{build_create_response_dated, CREATE_TIMES};
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };
        mock.queue_response(build_create_response_dated(file_id, 3));
        mock.queue_response(build_read_response(NtStatus::SUCCESS, vec![7, 8, 9]));
        mock.queue_response(build_close_response());

        let download = tree.download(&mut conn, "dated.bin").await.unwrap();
        let info = download
            .info()
            .expect("Tree::download knows the dates")
            .clone();
        assert_eq!(download.collect().await.unwrap(), [7, 8, 9]);

        let [created, accessed, modified, changed] = CREATE_TIMES;
        assert_eq!(
            (
                info.size,
                info.created,
                info.modified,
                info.accessed,
                info.changed
            ),
            (3, created, modified, accessed, changed)
        );
    }

    /// A download around a handle the caller opened never saw a CREATE
    /// response, so it doesn't pretend to know the dates.
    #[tokio::test]
    async fn a_download_of_a_callers_handle_knows_no_dates() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let tree = Tree {
            pin: None,
            tree_id: TreeId(7),
            share_name: "share".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        };
        let download = FileDownload::new(
            &tree,
            &mut conn,
            FileId {
                persistent: 1,
                volatile: 2,
            },
            3,
            65536,
        );
        assert!(download.info().is_none());
    }
}
