//! A file's timestamps and attributes: setting them, and handing out the ones
//! a read already got.
//!
//! ## Setting
//!
//! [`Tree::set_times`] (by path) and [`Tree::set_handle_times`] (on an open
//! handle) send SET_INFO `FileBasicInformation` (MS-FSCC § 2.4.7). Every time
//! a caller leaves out goes on the wire as 0, which the spec defines as "MUST
//! NOT change this attribute", so a caller setting only the modification time
//! changes only that. Unset attributes go as 0 too, which leaves them alone.
//!
//! **Attributes are replaced, never merged**: the server swaps the file's
//! whole settable set (MS-FSA § 2.1.5.15.2) for what's sent. That makes 0 a
//! trap: it can't mean "none", so empty attributes go as `NORMAL`, the one
//! value that clears them. Only the settable bits go at all, because the rest
//! describe the file (and Directory on a file is refused outright), so
//! attributes another file reported can be passed straight through.
//!
//! **Read-only blocks a delete and a rename onto it**, on Windows and on a
//! Samba that maps it (MS-FSCC § 2.6: a read-only file "cannot" be written or
//! deleted). A copy that wants it should set it once the file has landed under
//! its final name, and a cleanup that deletes one clears it first.
//!
//! The two other special values, -1 ("stop updating this time on this
//! handle") and -2 ("start again"), aren't offered: they're handle-scoped
//! switches rather than dates, and Samba treats -1 as "don't change" anyway.
//! A [`FileTime`] whose top bit is set would put one of them (or a value the
//! spec forbids) on the wire, so it's refused before anything is sent.
//!
//! **A server stamps a file on close if it was written through that handle**,
//! unless that handle had its time set explicitly. So a copy that wants the
//! source's date either sets it on the writer's own handle before closing
//! ([`FileWriter::set_times`](crate::FileWriter::set_times)), or by path once
//! the writer has finished. Setting it by path while the writer is still open
//! loses to the writer's close.
//!
//! ## Reading
//!
//! Every CREATE response carries the file's four times, its size, and its
//! attributes, so a read that opens the file already knows its dates and
//! whether it's Hidden or Read-only. [`FileInfo`](crate::FileInfo) built from it is
//! what [`FileDownload::info`](crate::FileDownload::info),
//! [`FileReader::info`](crate::FileReader::info), and
//! [`Tree::read_file_compound_with_info`] hand out, with no extra round trip.

use log::{debug, trace, warn};

use crate::client::connection::{CompoundOp, Connection};
use crate::client::tree::{all_or_first_err, FILE_BASIC_INFORMATION, FILE_OPEN_REPARSE_POINT};
use crate::error::Result;
use crate::msg::close::CloseRequest;
use crate::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use crate::msg::set_info::{InfoType, SetInfoRequest};
use crate::pack::{FileTime, ReadCursor, Unpack};
use crate::types::flags::{FileAccessMask, FileAttributes};
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, FileId, OplockLevel};
use crate::{Error, Tree};

/// Which of a file's timestamps (and, optionally, its attributes) to set, for
/// [`Tree::set_times`], [`Tree::set_handle_times`], and
/// [`FileWriter::set_times`](crate::FileWriter::set_times). It's everything
/// one SET_INFO `FileBasicInformation` changes, so a copy keeps its source's
/// dates and Hidden flag in one frame.
///
/// Every field starts out unset, and an unset one is left as the server has
/// it. Shaped like [`std::fs::FileTimes`]: build one with [`new`](Self::new)
/// and the `set_*` methods, which take a [`FileTime`] (for example one
/// [`Tree::stat`] returned, unchanged) or a [`SystemTime`](std::time::SystemTime).
///
/// ```
/// use std::time::{Duration, SystemTime};
/// use smb2::FileTimes;
///
/// // Only the modification time; creation and access times stay as they are.
/// let times = FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
/// assert!(times.created.is_none());
/// ```
///
/// A time of [`FileTime::ZERO`] means "don't change" on the wire, so setting
/// one is the same as leaving it unset. A time before 1601 converts to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileTimes {
    /// When the file was created (`CreationTime`).
    pub created: Option<FileTime>,
    /// When the file was last read (`LastAccessTime`).
    pub accessed: Option<FileTime>,
    /// When the file's contents last changed (`LastWriteTime`). What every
    /// file manager shows as "Modified".
    pub modified: Option<FileTime>,
    /// When the file's contents or metadata last changed (`ChangeTime`).
    pub changed: Option<FileTime>,
    /// The attributes to give the file (Hidden, Read-only, ...), or `None` to
    /// leave them as they are. They ride the same SET_INFO as the times; see
    /// [`set_attributes`](Self::set_attributes).
    pub attributes: Option<FileAttributes>,
}

impl FileTimes {
    /// No times set: sending it changes nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the creation time.
    #[must_use]
    pub fn set_created(mut self, t: impl Into<FileTime>) -> Self {
        self.created = Some(t.into());
        self
    }

    /// Set the last access time.
    #[must_use]
    pub fn set_accessed(mut self, t: impl Into<FileTime>) -> Self {
        self.accessed = Some(t.into());
        self
    }

    /// Set the last write (modification) time.
    #[must_use]
    pub fn set_modified(mut self, t: impl Into<FileTime>) -> Self {
        self.modified = Some(t.into());
        self
    }

    /// Set the change time.
    #[must_use]
    pub fn set_changed(mut self, t: impl Into<FileTime>) -> Self {
        self.changed = Some(t.into());
        self
    }

    /// Set the file's attributes, replacing the ones it has.
    ///
    /// The whole settable set is replaced, so a file sent only
    /// [`HIDDEN`](FileAttributes::HIDDEN) also loses Read-only and Archive.
    /// Empty attributes clear them all (they go on the wire as
    /// [`NORMAL`](FileAttributes::NORMAL), since 0 would mean "don't
    /// change"). Only the [`SETTABLE`](FileAttributes::SETTABLE) bits are
    /// sent: the rest (Directory, Sparse, Compressed, ...) describe the file
    /// rather than set it, and a server refuses Directory on a file, so the
    /// attributes another file reported can be passed in as they are.
    #[must_use]
    pub fn set_attributes(mut self, attributes: FileAttributes) -> Self {
        self.attributes = Some(attributes);
        self
    }
}

/// The 40-byte `FILE_BASIC_INFORMATION` that sets what `times` names and
/// leaves everything else alone.
fn basic_info_buffer(times: &FileTimes) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(40);
    for t in [times.created, times.accessed, times.modified, times.changed] {
        buf.extend_from_slice(&wire_time(t)?.to_le_bytes());
    }
    buf.extend_from_slice(&wire_attributes(times.attributes).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // Reserved
    Ok(buf)
}

/// The attributes as they go on the wire: 0 ("don't change") when unset,
/// only the settable bits otherwise, and `NORMAL` when none of those are left,
/// because 0 would leave the old ones in place instead of clearing them
/// (MS-FSCC § 2.6).
fn wire_attributes(attributes: Option<FileAttributes>) -> u32 {
    match attributes {
        None => 0,
        Some(a) => match a.bits() & FileAttributes::SETTABLE {
            0 => FileAttributes::NORMAL,
            bits => bits,
        },
    }
}

/// One time as it goes on the wire: 0 ("don't change") when unset, and
/// refused when the top bit is set, since as a signed FILETIME that's -1, -2,
/// or a value MS-FSCC § 2.4.7 forbids, none of them a date.
fn wire_time(t: Option<FileTime>) -> Result<u64> {
    match t {
        None => Ok(0),
        Some(FileTime(raw)) if raw > i64::MAX as u64 => Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{raw:#x} isn't a date a server can store (MS-FSCC § 2.4.7)"),
        ))),
        Some(FileTime(raw)) => Ok(raw),
    }
}

/// The SET_INFO that carries `buffer` as `FileBasicInformation`.
fn set_basic_info(file_id: FileId, buffer: Vec<u8>) -> SetInfoRequest {
    SetInfoRequest {
        info_type: InfoType::File,
        file_info_class: FILE_BASIC_INFORMATION,
        additional_information: 0,
        file_id,
        buffer,
    }
}

impl Tree {
    /// Set a file's or directory's timestamps (and attributes, when `times`
    /// carries them), leaving whatever `times` doesn't name as it is.
    ///
    /// One round trip: CREATE (attribute-write access, sharing everything) +
    /// SET_INFO `FileBasicInformation` + CLOSE as a compound. Works on
    /// directories too.
    ///
    /// A server stamps a file it was written through when that handle closes,
    /// so call this once any writer on the file has finished, or set the times
    /// on the writer itself with
    /// [`FileWriter::set_times`](crate::FileWriter::set_times).
    ///
    /// A time with its top bit set is refused before anything is sent (as an
    /// [`Error::Io`] of kind `InvalidInput`): on the wire it would mean a
    /// handle-scoped switch rather than a date (MS-FSCC § 2.4.7).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// use smb2::FileTimes;
    ///
    /// let source = tree.stat(conn, "original.jpg").await?;
    /// tree.set_times(conn, "copy.jpg", FileTimes::new().set_modified(source.modified))
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_times(
        &self,
        conn: &mut Connection,
        path: &str,
        times: FileTimes,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let buffer = basic_info_buffer(&times)?;
        let normalized = self.format_path(path);
        trace!("tree: set_times path={path:?} times={times:?}");

        // Attribute-write access is all FileBasicInformation needs (MS-FSCC
        // § 2.4.7), and sharing everything keeps the open from tripping over a
        // reader or writer that holds the file. No file-or-directory option,
        // so a directory opens as well as a file, and a link opens itself:
        // setting a junction's times or attributes must not touch its target.
        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_WRITE_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: 0,
            share_access: ShareAccess(
                ShareAccess::FILE_SHARE_READ
                    | ShareAccess::FILE_SHARE_WRITE
                    | ShareAccess::FILE_SHARE_DELETE,
            ),
            create_disposition: CreateDisposition::FileOpen,
            create_options: FILE_OPEN_REPARSE_POINT,
            name: normalized.clone(),
            create_contexts: vec![],
        };
        let set_req = set_basic_info(FileId::SENTINEL, buffer);
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
        let ops = [
            op(Command::Create, &create_req),
            op(Command::SetInfo, &set_req),
            op(Command::Close, &close_req),
        ];

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        let (create, set, close) = (&responses[0], &responses[1], &responses[2]);

        // A refused CREATE cascades to the rest, and there's no handle.
        if create.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create.header.status,
                command: Command::Create,
            });
        }
        // A refused SET_INFO takes the related CLOSE down with it, so the
        // handle needs a CLOSE of its own.
        if set.header.status != NtStatus::SUCCESS {
            let file_id = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?.file_id;
            warn!(
                "tree: compound SET_INFO (times) failed ({:?}), issuing standalone CLOSE",
                set.header.status
            );
            let _ = self.close_handle(conn, file_id).await;
            return Err(Error::Protocol {
                status: set.header.status,
                command: Command::SetInfo,
            });
        }
        if close.header.status != NtStatus::SUCCESS {
            debug!(
                "tree: compound CLOSE returned {:?} (non-fatal, times already set)",
                close.header.status
            );
        }

        debug!("tree: set times path={path:?}");
        Ok(())
    }

    /// [`set_times`](Self::set_times) on a handle that's already open: one
    /// SET_INFO, no CREATE or CLOSE.
    ///
    /// The handle needs `FILE_WRITE_ATTRIBUTES`, which every write open in
    /// this crate carries. Times set this way stick: the server doesn't
    /// restamp them when that handle is written through or closed
    /// (MS-FSA's `UserSetModificationTime`; Samba's sticky write time).
    pub async fn set_handle_times(
        &self,
        conn: &mut Connection,
        file_id: FileId,
        times: FileTimes,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let req = set_basic_info(file_id, basic_info_buffer(&times)?);
        trace!(
            "tree: set_handle_times file_id={:?} times={:?}",
            file_id,
            times
        );
        let frame = conn
            .execute(Command::SetInfo, &req, Some(self.tree_id))
            .await?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::SetInfo,
            });
        }
        debug!("tree: set times on handle {:?}", file_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::client::connection::pack_message;
    use crate::client::test_helpers::{
        build_close_response, build_compound_response_frame, build_create_response,
        build_set_info_response, setup_connection,
    };
    use crate::msg::header::Header;
    use crate::transport::MockTransport;
    use crate::types::TreeId;

    /// 2023-11-14 22:13:20 UTC.
    const MODIFIED: FileTime = FileTime(133_444_736_000_000_000);
    const CREATED: FileTime = FileTime(132_000_000_000_000_000);

    fn tree() -> Tree {
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

    fn file_id() -> FileId {
        FileId {
            persistent: 1,
            volatile: 2,
        }
    }

    fn u64_at(buf: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(buf[at..at + 8].try_into().unwrap())
    }

    /// The sub-requests of a sent compound frame, as (header, body) pairs.
    fn sub_requests(frame: &[u8]) -> Vec<(Header, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0;
        loop {
            let mut cursor = ReadCursor::new(&frame[at..]);
            let header = Header::unpack(&mut cursor).unwrap();
            let end = if header.next_command == 0 {
                frame.len()
            } else {
                at + header.next_command as usize
            };
            let body = frame[at + Header::SIZE..end].to_vec();
            let next = header.next_command;
            out.push((header, body));
            if next == 0 {
                return out;
            }
            at += next as usize;
        }
    }

    fn failed(command: Command, status: NtStatus) -> Vec<u8> {
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
    }

    #[test]
    fn only_the_times_asked_for_go_on_the_wire() {
        let buf = basic_info_buffer(&FileTimes::new().set_modified(MODIFIED)).unwrap();
        assert_eq!(buf.len(), 40);
        assert_eq!(u64_at(&buf, 0), 0, "CreationTime: don't change");
        assert_eq!(u64_at(&buf, 8), 0, "LastAccessTime: don't change");
        assert_eq!(u64_at(&buf, 16), MODIFIED.0, "LastWriteTime");
        assert_eq!(u64_at(&buf, 24), 0, "ChangeTime: don't change");
        assert_eq!(&buf[32..36], &[0; 4], "FileAttributes: don't change");
    }

    #[test]
    fn every_time_lands_in_its_own_field() {
        let times = FileTimes::new()
            .set_created(FileTime(1))
            .set_accessed(FileTime(2))
            .set_modified(FileTime(3))
            .set_changed(FileTime(4));
        let buf = basic_info_buffer(&times).unwrap();
        assert_eq!(
            [0, 8, 16, 24].map(|at| u64_at(&buf, at)),
            [1, 2, 3, 4],
            "MS-FSCC § 2.4.7 order: creation, access, write, change"
        );
    }

    fn attributes_on_the_wire(attributes: u32) -> u32 {
        let times = FileTimes::new().set_attributes(FileAttributes::new(attributes));
        let buf = basic_info_buffer(&times).unwrap();
        u32::from_le_bytes(buf[32..36].try_into().unwrap())
    }

    #[test]
    fn attributes_land_in_their_field_beside_the_times() {
        let times = FileTimes::new()
            .set_modified(MODIFIED)
            .set_attributes(FileAttributes::new(FileAttributes::HIDDEN));
        let buf = basic_info_buffer(&times).unwrap();
        assert_eq!(buf.len(), 40);
        assert_eq!(u64_at(&buf, 16), MODIFIED.0, "LastWriteTime");
        assert_eq!(&buf[32..36], &[0x02, 0, 0, 0], "FileAttributes: HIDDEN");
        assert_eq!(&buf[36..40], &[0; 4], "Reserved");
    }

    #[test]
    fn empty_attributes_clear_them_all_rather_than_meaning_dont_change() {
        assert_eq!(attributes_on_the_wire(0), FileAttributes::NORMAL);
    }

    #[test]
    fn normal_beside_another_attribute_is_dropped() {
        assert_eq!(
            attributes_on_the_wire(FileAttributes::NORMAL | FileAttributes::READONLY),
            FileAttributes::READONLY
        );
        assert_eq!(
            attributes_on_the_wire(FileAttributes::NORMAL),
            FileAttributes::NORMAL
        );
    }

    #[test]
    fn attributes_a_file_reports_but_cant_be_given_stay_off_the_wire() {
        // A directory's attributes, passed straight on: DIRECTORY on a file is
        // refused (MS-FSA 2.1.5.15.2), so only the settable bits go.
        assert_eq!(
            attributes_on_the_wire(FileAttributes::DIRECTORY | FileAttributes::HIDDEN),
            FileAttributes::HIDDEN
        );
        assert_eq!(
            attributes_on_the_wire(
                FileAttributes::DIRECTORY
                    | FileAttributes::SPARSE_FILE
                    | FileAttributes::REPARSE_POINT
                    | FileAttributes::COMPRESSED
                    | FileAttributes::ENCRYPTED
            ),
            FileAttributes::NORMAL,
            "nothing settable left means clear"
        );
        assert_eq!(attributes_on_the_wire(u32::MAX), FileAttributes::SETTABLE);
    }

    #[test]
    fn a_time_that_would_mean_a_handle_switch_is_refused() {
        for raw in [u64::MAX, u64::MAX - 1, 1 << 63] {
            let err = basic_info_buffer(&FileTimes::new().set_modified(FileTime(raw)))
                .expect_err("a negative FILETIME is not a date");
            assert!(
                matches!(&err, Error::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput),
                "{raw:#x}: {err:?}"
            );
        }
    }

    #[test]
    fn a_system_time_converts_to_the_same_filetime() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(
            FileTimes::new().set_modified(t).modified,
            Some(FileTime::from_system_time(t))
        );
    }

    #[tokio::test]
    async fn set_times_sends_create_set_info_close_as_one_compound() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id(), 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);

        tree()
            .set_times(
                &mut conn,
                "dir/photo.jpg",
                FileTimes::new().set_modified(MODIFIED),
            )
            .await
            .expect("set_times");

        assert_eq!(mock.sent_count(), 1, "one round trip");
        let subs = sub_requests(&mock.sent_message(0).unwrap());
        let commands: Vec<Command> = subs.iter().map(|(h, _)| h.command).collect();
        assert_eq!(
            commands,
            [Command::Create, Command::SetInfo, Command::Close]
        );

        let create = CreateRequest::unpack(&mut ReadCursor::new(&subs[0].1)).unwrap();
        assert_eq!(create.name, "dir\\photo.jpg");
        assert!(create
            .desired_access
            .contains(FileAccessMask::FILE_WRITE_ATTRIBUTES));
        assert!(!create
            .desired_access
            .contains(FileAccessMask::FILE_WRITE_DATA));
        assert_eq!(create.create_disposition, CreateDisposition::FileOpen);
        // Files and directories alike, and a link itself, never its target.
        assert_eq!(create.create_options, FILE_OPEN_REPARSE_POINT);

        let set = SetInfoRequest::unpack(&mut ReadCursor::new(&subs[1].1)).unwrap();
        assert_eq!(set.info_type, InfoType::File);
        assert_eq!(set.file_info_class, FILE_BASIC_INFORMATION);
        assert_eq!(set.file_id, FileId::SENTINEL);
        assert_eq!(set.buffer.len(), 40);
        assert_eq!(u64_at(&set.buffer, 16), MODIFIED.0);
        assert_eq!(u64_at(&set.buffer, 0), 0);
    }

    #[tokio::test]
    async fn set_times_on_a_dfs_share_names_the_server_and_share() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id(), 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);
        let mut dfs_tree = tree();
        dfs_tree.is_dfs = true;

        dfs_tree
            .set_times(
                &mut conn,
                "a/b.txt",
                FileTimes::new().set_modified(MODIFIED),
            )
            .await
            .expect("set_times");

        let subs = sub_requests(&mock.sent_message(0).unwrap());
        let create = CreateRequest::unpack(&mut ReadCursor::new(&subs[0].1)).unwrap();
        assert_eq!(create.name, "test-server\\test\\a\\b.txt");
    }

    #[tokio::test]
    async fn a_refused_set_info_closes_the_handle_and_says_so() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id(), 0),
            failed(Command::SetInfo, NtStatus::ACCESS_DENIED),
            failed(Command::Close, NtStatus::ACCESS_DENIED),
        ]));
        mock.queue_response(build_close_response());
        let mut conn = setup_connection(&mock);

        let err = tree()
            .set_times(
                &mut conn,
                "locked.txt",
                FileTimes::new().set_modified(MODIFIED),
            )
            .await
            .expect_err("the server refused");
        assert!(matches!(
            err,
            Error::Protocol {
                status: NtStatus::ACCESS_DENIED,
                command: Command::SetInfo
            }
        ));
        assert_eq!(mock.sent_count(), 2, "a standalone CLOSE follows");
        let close = mock.sent_message(1).unwrap();
        let mut cursor = ReadCursor::new(&close);
        assert_eq!(Header::unpack(&mut cursor).unwrap().command, Command::Close);
        assert_eq!(
            CloseRequest::unpack(&mut cursor).unwrap().file_id,
            file_id()
        );
    }

    #[tokio::test]
    async fn a_refused_create_needs_no_close() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            failed(Command::Create, NtStatus::OBJECT_NAME_NOT_FOUND),
            failed(Command::SetInfo, NtStatus::OBJECT_NAME_NOT_FOUND),
            failed(Command::Close, NtStatus::OBJECT_NAME_NOT_FOUND),
        ]));
        let mut conn = setup_connection(&mock);

        let err = tree()
            .set_times(
                &mut conn,
                "missing.txt",
                FileTimes::new().set_modified(MODIFIED),
            )
            .await
            .expect_err("no such file");
        assert_eq!(err.status(), Some(NtStatus::OBJECT_NAME_NOT_FOUND));
        assert_eq!(mock.sent_count(), 1);
    }

    #[tokio::test]
    async fn an_invalid_time_sends_nothing() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        let by_path = tree()
            .set_times(
                &mut conn,
                "x.txt",
                FileTimes::new().set_modified(FileTime(u64::MAX)),
            )
            .await;
        let on_handle = tree()
            .set_handle_times(
                &mut conn,
                file_id(),
                FileTimes::new().set_created(FileTime(u64::MAX)),
            )
            .await;
        assert!(by_path.is_err() && on_handle.is_err());
        assert_eq!(mock.sent_count(), 0);
    }

    #[tokio::test]
    async fn set_handle_times_is_one_set_info_on_that_handle() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_set_info_response());
        let mut conn = setup_connection(&mock);

        tree()
            .set_handle_times(
                &mut conn,
                file_id(),
                FileTimes::new().set_created(CREATED).set_modified(MODIFIED),
            )
            .await
            .expect("set_handle_times");

        assert_eq!(mock.sent_count(), 1);
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let header = Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::SetInfo);
        assert_eq!(header.tree_id, Some(TreeId(10)));
        let set = SetInfoRequest::unpack(&mut cursor).unwrap();
        assert_eq!(set.file_id, file_id());
        assert_eq!(set.file_info_class, FILE_BASIC_INFORMATION);
        assert_eq!(u64_at(&set.buffer, 0), CREATED.0);
        assert_eq!(u64_at(&set.buffer, 16), MODIFIED.0);
    }

    #[tokio::test]
    async fn set_handle_times_carries_the_attributes_in_the_same_set_info() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_set_info_response());
        let mut conn = setup_connection(&mock);

        tree()
            .set_handle_times(
                &mut conn,
                file_id(),
                FileTimes::new()
                    .set_modified(MODIFIED)
                    .set_attributes(FileAttributes::new(FileAttributes::HIDDEN)),
            )
            .await
            .expect("set_handle_times");

        assert_eq!(mock.sent_count(), 1, "still one frame");
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        Header::unpack(&mut cursor).unwrap();
        let set = SetInfoRequest::unpack(&mut cursor).unwrap();
        assert_eq!(u64_at(&set.buffer, 16), MODIFIED.0);
        assert_eq!(&set.buffer[32..36], &[0x02, 0, 0, 0]);
    }

    #[tokio::test]
    async fn set_times_by_path_carries_the_attributes_too() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(file_id(), 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        let mut conn = setup_connection(&mock);

        tree()
            .set_times(
                &mut conn,
                "dir",
                FileTimes::new().set_attributes(FileAttributes::new(FileAttributes::READONLY)),
            )
            .await
            .expect("set_times");

        let subs = sub_requests(&mock.sent_message(0).unwrap());
        let set = SetInfoRequest::unpack(&mut ReadCursor::new(&subs[1].1)).unwrap();
        assert_eq!(&set.buffer[0..32], &[0; 32], "no time changes");
        assert_eq!(&set.buffer[32..36], &[0x01, 0, 0, 0]);
    }

    #[tokio::test]
    async fn set_handle_times_reports_a_refusal() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(failed(Command::SetInfo, NtStatus::ACCESS_DENIED));
        let mut conn = setup_connection(&mock);

        let err = tree()
            .set_handle_times(
                &mut conn,
                file_id(),
                FileTimes::new().set_modified(MODIFIED),
            )
            .await
            .expect_err("refused");
        assert_eq!(err.status(), Some(NtStatus::ACCESS_DENIED));
    }
}
