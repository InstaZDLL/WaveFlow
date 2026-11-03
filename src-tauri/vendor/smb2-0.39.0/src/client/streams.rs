//! Named streams (alternate data streams) and what a share says about them.
//!
//! A stream is addressed as `path:name` on the wire, and ❌ that `:` can't come
//! from the caller's path: [`crate::name::encode_path`] maps every `:` in a path
//! to U+F022, because a `:` in a file name is legitimate (AGENTS.md pitfall 23).
//! So the stream name travels as its own argument and `Tree::stream_wire_name`
//! is the one place the two meet. An inner `:` in a stream name is mapped the
//! same way, which is exactly what macOS `smbfs` puts on the wire for an xattr
//! like `com.apple.metadata:_kMDItemUserTags`. It also means a caller can't
//! reach the default stream (`::$DATA`, the file's own content) through a stream
//! name, and `delete_stream` can't delete a file.
//!
//! Every open shares read, write, and delete, so touching a stream on a file
//! someone else has open doesn't hit a sharing violation. None of them set
//! `FILE_NON_DIRECTORY_FILE`, since directories carry streams too.
//!
//! Server behavior, with evidence: `docs/notes/named-streams-and-xattrs.md`.

use log::{debug, trace};

use crate::client::connection::{CompoundOp, Connection, Frame};
use crate::client::credits;
use crate::client::tree::{all_or_first_err, SHARE_ALL};
use crate::client::watcher::decode_utf16le;
use crate::error::Result;
use crate::msg::close::CloseRequest;
use crate::msg::create::{CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel};
use crate::msg::query_info::{InfoType, QueryInfoRequest, QueryInfoResponse};
use crate::msg::read::{ReadRequest, ReadResponse, SMB2_CHANNEL_NONE};
use crate::msg::set_info::{InfoType as SetInfoType, SetInfoRequest};
use crate::msg::write::{WriteRequest, WriteResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::types::flags::FileAccessMask;
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, FileId, OplockLevel};
use crate::{Error, Tree};

/// FileStreamInformation class for QUERY_INFO (MS-FSCC § 2.4.49).
const FILE_STREAM_INFORMATION: u8 = 22;

/// FileFsAttributeInformation class for QUERY_INFO (MS-FSCC § 2.5.1).
const FILE_FS_ATTRIBUTE_INFORMATION: u8 = 5;

/// FileDispositionInformation class for SET_INFO (MS-FSCC § 2.4.11).
const FILE_DISPOSITION_INFORMATION: u8 = 13;

/// `FILE_NAMED_STREAMS` in `FileSystemAttributes` (MS-FSCC § 2.5.1).
const FILE_NAMED_STREAMS: u32 = 0x0004_0000;

/// What one stream listing asks for first: one credit's worth, far more than
/// any real file's stream list.
const STREAM_LIST_BUFFER: u32 = 65536;

/// What a listing the first buffer couldn't hold asks for, capped by the
/// server's `MaxTransactSize`.
const STREAM_LIST_RETRY_BUFFER: u32 = 1024 * 1024;

/// One named stream of a file or directory, from [`Tree::list_streams`].
///
/// The file's own content (the unnamed `::$DATA` stream) is never listed, so an
/// ordinary file with no streams lists as empty on every server, whether or not
/// the server reports that default stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StreamInfo {
    /// The stream's own name, without the leading `:` or the `:$DATA` suffix,
    /// and mapped back out of the private-use area (see [`crate::name`]), so it
    /// goes straight back into [`Tree::read_stream`].
    pub name: String,
    /// The stream's size in bytes.
    pub size: u64,
    /// The space it takes on disk, in bytes.
    pub allocation_size: u64,
}

/// What a share's file system says about itself, from [`Tree::fs_attributes`].
/// One query per share; cache the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FsAttributes {
    /// Whether the share's file system has named streams (the
    /// `FILE_NAMED_STREAMS` bit).
    pub supports_named_streams: bool,
    /// The file system's name as the server reports it, like `NTFS` or `ZFS`.
    pub file_system_name: String,
    /// The longest file name component the file system takes.
    pub max_component_length: u32,
    /// The raw `FileSystemAttributes` flags (MS-FSCC § 2.5.1).
    pub flags: u32,
}

/// Parse a `FILE_STREAM_INFORMATION` chain (MS-FSCC § 2.4.49).
///
/// Skips the default stream, and any entry that isn't a `$DATA` stream. Stream
/// names are decoded like every other name off the wire (AGENTS.md pitfall 23).
pub(crate) fn parse_stream_information(data: &[u8]) -> Result<Vec<StreamInfo>> {
    const FIXED: usize = 24;
    let mut streams = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let entry = &data[offset..];
        if entry.len() < FIXED {
            return Err(Error::invalid_data(
                "FILE_STREAM_INFORMATION truncated: not enough bytes for its fixed fields",
            ));
        }
        let next = u32::from_le_bytes(entry[0..4].try_into().unwrap()) as usize;
        let name_len = u32::from_le_bytes(entry[4..8].try_into().unwrap()) as usize;
        let size = u64::from_le_bytes(entry[8..16].try_into().unwrap());
        let allocation_size = u64::from_le_bytes(entry[16..24].try_into().unwrap());
        let name_bytes = FIXED
            .checked_add(name_len)
            .and_then(|end| entry.get(FIXED..end))
            .ok_or_else(|| {
                Error::invalid_data("FILE_STREAM_INFORMATION name extends beyond its buffer")
            })?;
        let raw = decode_utf16le(name_bytes)?;
        if let Some(name) = named_data_stream(&raw)? {
            streams.push(StreamInfo {
                name: crate::name::decode_name(name).into_owned(),
                size,
                allocation_size,
            });
        }
        if next == 0 {
            break;
        }
        if next < FIXED {
            return Err(Error::invalid_data(
                "FILE_STREAM_INFORMATION next-entry offset points inside its own entry",
            ));
        }
        offset = offset.checked_add(next).ok_or_else(|| {
            Error::invalid_data("FILE_STREAM_INFORMATION next-entry offset overflows")
        })?;
    }
    Ok(streams)
}

/// The name in a `:name:$DATA` entry, or `None` for the default stream and for
/// a stream of any other type.
fn named_data_stream(raw: &str) -> Result<Option<&str>> {
    let body = raw.strip_prefix(':').ok_or_else(|| {
        Error::invalid_data(format!(
            "stream entry {raw:?} doesn't start with its separator"
        ))
    })?;
    let Some((name, kind)) = body.rsplit_once(':') else {
        return Err(Error::invalid_data(format!(
            "stream entry {raw:?} has no type"
        )));
    };
    if name.is_empty() || !kind.eq_ignore_ascii_case("$DATA") {
        return Ok(None);
    }
    Ok(Some(name))
}

/// Parse `FILE_FS_ATTRIBUTE_INFORMATION` (MS-FSCC § 2.5.1).
pub(crate) fn parse_fs_attributes(data: &[u8]) -> Result<FsAttributes> {
    const FIXED: usize = 12;
    if data.len() < FIXED {
        return Err(Error::invalid_data(format!(
            "FileFsAttributeInformation too short: {} bytes",
            data.len()
        )));
    }
    let flags = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let max_component_length = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let name_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let name_bytes = FIXED
        .checked_add(name_len)
        .and_then(|end| data.get(FIXED..end))
        .ok_or_else(|| {
            Error::invalid_data("FileFsAttributeInformation name extends beyond its buffer")
        })?;
    Ok(FsAttributes {
        supports_named_streams: flags & FILE_NAMED_STREAMS != 0,
        file_system_name: decode_utf16le(name_bytes)?,
        max_component_length,
        flags,
    })
}

/// Whether `status` from a stream's CREATE says only "that name isn't there",
/// which a server answers alike for a missing stream and a missing file.
pub(super) fn is_missing_name(status: NtStatus) -> bool {
    matches!(status, NtStatus::OBJECT_NAME_NOT_FOUND)
}

/// A `CREATE` for a stream or its file. Never `FILE_NON_DIRECTORY_FILE`, and
/// shared all round (module docs).
fn open_request(name: String, access: u32, create_disposition: CreateDisposition) -> CreateRequest {
    CreateRequest {
        requested_oplock_level: OplockLevel::None,
        impersonation_level: ImpersonationLevel::Impersonation,
        desired_access: FileAccessMask::new(access),
        file_attributes: 0,
        share_access: SHARE_ALL,
        create_disposition,
        create_options: 0,
        name,
        create_contexts: vec![],
    }
}

fn protocol(status: NtStatus, command: Command) -> Error {
    Error::Protocol { status, command }
}

/// What a refused stream CREATE means. Both the path and the stream name went
/// out mapped to a legal form, so a server calling the name invalid has no named
/// streams: that's what a Samba without a streams module answers.
fn no_streams_if_refused<T>(result: Result<T>, path: &str) -> Result<T> {
    match result {
        Err(Error::Protocol {
            status: NtStatus::OBJECT_NAME_INVALID,
            command: Command::Create,
        }) => Err(Error::NamedStreamsUnsupported {
            path: path.to_string(),
        }),
        other => other,
    }
}

impl Tree {
    /// The wire name of `stream` on `path`.
    ///
    /// The one place a path and a stream name meet (module docs). Refuses an
    /// empty name, since `path:` would name the file itself, and a `/`, which
    /// no mapping covers and no server takes.
    pub(super) fn stream_wire_name(&self, path: &str, stream: &str) -> Result<String> {
        if stream.is_empty() {
            return Err(Error::invalid_data("a stream name can't be empty"));
        }
        if stream.contains('/') {
            return Err(Error::invalid_data(format!(
                "a stream name can't contain '/': {stream:?}"
            )));
        }
        // `dir/` would otherwise become `dir\:stream`, a stream on an unnamed
        // child of `dir`.
        let base = self.format_path(path.trim_end_matches('/'));
        Ok(format!("{base}:{}", crate::name::encode_name(stream)))
    }

    fn compound_op<'a>(
        &self,
        command: Command,
        body: &'a dyn crate::pack::Pack,
        credit_charge: u16,
    ) -> CompoundOp<'a> {
        CompoundOp {
            command,
            body,
            tree_id: Some(self.tree_id),
            credit_charge: CreditCharge(credit_charge),
        }
    }

    /// CREATE + QUERY_INFO + CLOSE on `wire_name`, in one round trip. Hands back
    /// the QUERY_INFO's frame whatever its status, having closed the handle if
    /// the compound's CLOSE didn't.
    async fn query_compound(
        &self,
        conn: &mut Connection,
        wire_name: String,
        create_options: u32,
        query: &QueryInfoRequest,
    ) -> Result<Frame> {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let mut create = open_request(
            wire_name,
            FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            CreateDisposition::FileOpen,
        );
        create.create_options = create_options;
        let close = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };
        let query_charge = credits::charge_for_payload(u64::from(query.output_buffer_length));
        let ops = [
            self.compound_op(Command::Create, &create, 1),
            self.compound_op(Command::QueryInfo, query, query_charge),
            self.compound_op(Command::Close, &close, 1),
        ];
        let mut responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        if responses[0].header.status != NtStatus::SUCCESS {
            return Err(protocol(responses[0].header.status, Command::Create));
        }
        if responses[2].header.status != NtStatus::SUCCESS {
            let file_id = CreateResponse::unpack(&mut ReadCursor::new(&responses[0].body))?.file_id;
            debug!(
                "streams: compound CLOSE returned {:?}, closing standalone",
                responses[2].header.status
            );
            let _ = self.close_handle(conn, file_id).await;
        }
        Ok(responses.swap_remove(1))
    }

    /// One attempt at a stream listing. `None` when `buffer` couldn't hold it.
    async fn list_streams_with_buffer(
        &self,
        conn: &mut Connection,
        path: &str,
        buffer: u32,
    ) -> Result<Option<Vec<StreamInfo>>> {
        let query = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_STREAM_INFORMATION,
            output_buffer_length: buffer,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };
        let wire_name = self.format_path(path.trim_end_matches('/'));
        let frame = self.query_compound(conn, wire_name, 0, &query).await?;
        match frame.header.status {
            NtStatus::SUCCESS => {
                let resp = QueryInfoResponse::unpack(&mut ReadCursor::new(&frame.body))?;
                parse_stream_information(&resp.output_buffer).map(Some)
            }
            // Either way the list is cut short, and a short list reads as "this
            // file has no such stream", so it's never returned (pitfall 10).
            NtStatus::BUFFER_OVERFLOW | NtStatus::BUFFER_TOO_SMALL => Ok(None),
            status => Err(protocol(status, Command::QueryInfo)),
        }
    }

    /// List a file's or directory's named streams, in one round trip.
    ///
    /// The file's own content isn't one of them (see [`StreamInfo`]). A server
    /// with no stream support answers the query with an error; ask
    /// [`fs_attributes`](Self::fs_attributes) to know up front.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// for stream in tree.list_streams(conn, "notes.txt").await? {
    ///     println!("{}: {} bytes", stream.name, stream.size);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn list_streams(&self, conn: &mut Connection, path: &str) -> Result<Vec<StreamInfo>> {
        trace!("streams: list path={path:?}");
        if let Some(streams) = self
            .list_streams_with_buffer(conn, path, STREAM_LIST_BUFFER)
            .await?
        {
            return Ok(streams);
        }
        let max_transact = conn.params().map_or(65536, |p| p.max_transact_size);
        let buffer = STREAM_LIST_RETRY_BUFFER.min(max_transact);
        self.list_streams_with_buffer(conn, path, buffer)
            .await?
            .ok_or_else(|| {
                Error::invalid_data(format!(
                    "the stream list doesn't fit in {buffer} bytes, path {path:?}"
                ))
            })
    }

    /// Read a whole named stream.
    ///
    /// `stream` is the stream's own name, without the leading `:` or the
    /// `:$DATA` suffix; an inner `:` is mapped the way macOS `smbfs` maps it.
    /// One round trip when the stream fits in one READ, which is nearly always.
    ///
    /// A missing stream and a missing file are both
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound), because servers answer
    /// both with `STATUS_OBJECT_NAME_NOT_FOUND`.
    /// [`get_xattr`](Self::get_xattr) tells them apart. A share with no named
    /// streams answers [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported),
    /// as do the other stream calls that open one.
    pub async fn read_stream(
        &self,
        conn: &mut Connection,
        path: &str,
        stream: &str,
    ) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        let max_read = conn.params().map_or(65536, |p| p.max_read_size);
        self.read_stream_sized(conn, path, stream, u64::from(max_read))
            .await
    }

    /// [`read_stream`](Self::read_stream) asking for `expected` bytes up front,
    /// which is what keeps a batch of small streams at one credit per READ. A
    /// bigger stream costs more round trips, never a short answer.
    pub(super) async fn read_stream_sized(
        &self,
        conn: &mut Connection,
        path: &str,
        stream: &str,
        expected: u64,
    ) -> Result<Vec<u8>> {
        let wire_name = self.stream_wire_name(path, stream)?;
        match self
            .read_compound_named(conn, path, wire_name.clone(), expected)
            .await
        {
            Ok((data, _)) => Ok(data),
            Err(Error::FileTooLargeForSingleRead { size, .. }) => {
                self.read_stream_chunked(conn, wire_name, size).await
            }
            Err(e) => no_streams_if_refused(Err(e), path),
        }
    }

    /// Read a stream too big for one READ, one READ at a time. Only a large
    /// resource fork gets here, so it isn't worth a read-ahead window.
    async fn read_stream_chunked(
        &self,
        conn: &mut Connection,
        wire_name: String,
        size: u64,
    ) -> Result<Vec<u8>> {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let max_read = conn.params().map_or(65536, |p| p.max_read_size);
        let chunk = conn.fundable_chunk(max_read);
        let create = open_request(
            wire_name,
            FileAccessMask::FILE_READ_DATA
                | FileAccessMask::FILE_READ_ATTRIBUTES
                | FileAccessMask::SYNCHRONIZE,
            CreateDisposition::FileOpen,
        );
        let frame = conn
            .execute(Command::Create, &create, Some(self.tree_id))
            .await?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(protocol(frame.header.status, Command::Create));
        }
        let file_id = CreateResponse::unpack(&mut ReadCursor::new(&frame.body))?.file_id;

        let mut data = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
        let outcome = loop {
            let req = ReadRequest {
                padding: 0x50,
                flags: 0,
                length: chunk,
                offset: data.len() as u64,
                file_id,
                minimum_count: 0,
                channel: SMB2_CHANNEL_NONE,
                remaining_bytes: 0,
                read_channel_info: vec![],
            };
            let charge = credits::charge_for_payload(u64::from(chunk));
            let frame = match conn
                .execute_with_credits(
                    Command::Read,
                    &req,
                    Some(self.tree_id),
                    CreditCharge(charge),
                )
                .await
            {
                Ok(frame) => frame,
                Err(e) => break Err(e),
            };
            match frame.header.status {
                NtStatus::END_OF_FILE => break Ok(()),
                NtStatus::SUCCESS => {}
                status => break Err(protocol(status, Command::Read)),
            }
            let resp = match ReadResponse::unpack(&mut ReadCursor::new(&frame.body)) {
                Ok(resp) => resp,
                Err(e) => break Err(e),
            };
            if resp.data.is_empty() {
                break Ok(());
            }
            data.extend_from_slice(&resp.data);
        };
        let _ = self.close_handle(conn, file_id).await;
        outcome.map(|()| data)
    }

    /// Whether `path` is there, in one round trip: CREATE + CLOSE.
    ///
    /// Follows links like every stream op, so what it answers is what the
    /// stream ops would reach. `Err` is the server's refusal of the path.
    pub(super) async fn probe_path(&self, conn: &mut Connection, path: &str) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let create = open_request(
            self.format_path(path.trim_end_matches('/')),
            FileAccessMask::FILE_READ_ATTRIBUTES | FileAccessMask::SYNCHRONIZE,
            CreateDisposition::FileOpen,
        );
        let close = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };
        let ops = [
            self.compound_op(Command::Create, &create, 1),
            self.compound_op(Command::Close, &close, 1),
        ];
        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        if responses[0].header.status != NtStatus::SUCCESS {
            return Err(protocol(responses[0].header.status, Command::Create));
        }
        if responses[1].header.status != NtStatus::SUCCESS {
            let file_id = CreateResponse::unpack(&mut ReadCursor::new(&responses[0].body))?.file_id;
            let _ = self.close_handle(conn, file_id).await;
        }
        Ok(())
    }

    /// Replace a named stream's whole content, creating the stream if needed.
    ///
    /// ❌ **It never creates the file.** A stream CREATE that may create is
    /// answered by creating the file too, on both Samba and Windows, so a
    /// write to a file someone just deleted would leave an empty file in its
    /// place. So this makes sure the file is there first: a missing file is
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound). That's a second round
    /// trip, and it leaves a window: a file deleted between the two comes back
    /// empty, holding only this stream.
    ///
    /// A shorter value leaves no trailing bytes (`FILE_OVERWRITE_IF`). A value
    /// that doesn't fit one frame is written a chunk at a time. A server's own
    /// limit still applies: `streams_xattr` stops at the file system's xattr
    /// size (64 KiB on Linux), with `STATUS_FILE_SYSTEM_LIMITATION`.
    ///
    /// The server empties the stream before the data arrives, so a write that
    /// fails loses the old value; this then deletes the stream, so nothing
    /// partial reads as real.
    ///
    /// Whether this changes the file's modification time is up to the server,
    /// and they differ: `docs/notes/named-streams-and-xattrs.md`.
    pub async fn write_stream(
        &self,
        conn: &mut Connection,
        path: &str,
        stream: &str,
        data: &[u8],
    ) -> Result<()> {
        let wire_name = self.stream_wire_name(path, stream)?;
        self.probe_path(conn, path).await?;
        trace!(
            "streams: write path={path:?} stream={stream:?}, len={}",
            data.len()
        );
        let written = self.write_stream_named(conn, wire_name, data).await;
        if let Err(Error::Protocol {
            command: Command::Write,
            ..
        }) = &written
        {
            // The CREATE already emptied the stream, or made it new. An empty
            // or partial value left behind reads as a real one, and `streams_xattr`
            // refuses anything past the file system's xattr limit this way, so
            // the failed write leaves no stream at all.
            let _ = self.delete_stream(conn, path, stream).await;
        }
        no_streams_if_refused(written, path)?;
        debug!("streams: wrote path={path:?} stream={stream:?}");
        Ok(())
    }

    /// CREATE(`FILE_OVERWRITE_IF`) + WRITE + CLOSE, or the chunked form.
    async fn write_stream_named(
        &self,
        conn: &mut Connection,
        wire_name: String,
        data: &[u8],
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let create = open_request(
            wire_name,
            FileAccessMask::FILE_WRITE_DATA
                | FileAccessMask::FILE_READ_ATTRIBUTES
                | FileAccessMask::SYNCHRONIZE,
            CreateDisposition::FileOverwriteIf,
        );
        if data.len() as u64 > conn.compound_write_limit() {
            return self.write_stream_chunked(conn, &create, data).await;
        }

        let close = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };
        let write = sentinel_write(FileId::SENTINEL, 0, data);
        let write_charge = credits::charge_for_payload(data.len() as u64);
        let mut ops = vec![self.compound_op(Command::Create, &create, 1)];
        // A zero-length WRITE is a no-op with a charge of zero, which isn't a
        // request the spec allows; the CREATE has already emptied the stream.
        if !data.is_empty() {
            ops.push(self.compound_op(Command::Write, &write, write_charge));
        }
        ops.push(self.compound_op(Command::Close, &close, 1));

        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        if responses[0].header.status != NtStatus::SUCCESS {
            return Err(protocol(responses[0].header.status, Command::Create));
        }
        let file_id = CreateResponse::unpack(&mut ReadCursor::new(&responses[0].body))?.file_id;
        let close_frame = &responses[responses.len() - 1];
        if !data.is_empty() {
            let write_frame = &responses[1];
            if write_frame.header.status != NtStatus::SUCCESS {
                if close_frame.header.status != NtStatus::SUCCESS {
                    let _ = self.close_handle(conn, file_id).await;
                }
                return Err(protocol(write_frame.header.status, Command::Write));
            }
            let written = WriteResponse::unpack(&mut ReadCursor::new(&write_frame.body))?.count;
            if written as usize != data.len() {
                if close_frame.header.status != NtStatus::SUCCESS {
                    let _ = self.close_handle(conn, file_id).await;
                }
                return Err(Error::invalid_data(format!(
                    "the server wrote {written} of a stream's {} bytes",
                    data.len()
                )));
            }
        }
        if close_frame.header.status != NtStatus::SUCCESS {
            // The bytes are in; only the handle is left.
            debug!(
                "streams: compound CLOSE returned {:?}, closing standalone",
                close_frame.header.status
            );
            let _ = self.close_handle(conn, file_id).await;
        }
        Ok(())
    }

    /// A value too big for one frame: CREATE, a WRITE per chunk, CLOSE.
    async fn write_stream_chunked(
        &self,
        conn: &mut Connection,
        create: &CreateRequest,
        data: &[u8],
    ) -> Result<()> {
        let max_write = conn.params().map_or(65536, |p| p.max_write_size);
        let chunk = conn.fundable_chunk(max_write) as usize;
        let frame = conn
            .execute(Command::Create, create, Some(self.tree_id))
            .await?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(protocol(frame.header.status, Command::Create));
        }
        let file_id = CreateResponse::unpack(&mut ReadCursor::new(&frame.body))?.file_id;

        let mut outcome = Ok(());
        for (index, piece) in data.chunks(chunk).enumerate() {
            let write = sentinel_write(file_id, (index * chunk) as u64, piece);
            let charge = credits::charge_for_payload(piece.len() as u64);
            let frame = conn
                .execute_with_credits(
                    Command::Write,
                    &write,
                    Some(self.tree_id),
                    CreditCharge(charge),
                )
                .await;
            let step = frame.and_then(|frame| {
                if frame.header.status != NtStatus::SUCCESS {
                    return Err(protocol(frame.header.status, Command::Write));
                }
                let written = WriteResponse::unpack(&mut ReadCursor::new(&frame.body))?.count;
                if written as usize != piece.len() {
                    return Err(Error::invalid_data(format!(
                        "the server wrote {written} of a {}-byte chunk",
                        piece.len()
                    )));
                }
                Ok(())
            });
            if step.is_err() {
                outcome = step;
                break;
            }
        }
        let closed = self.close_handle(conn, file_id).await;
        outcome.and(closed)
    }

    /// Delete a named stream, leaving the file and its other streams alone.
    ///
    /// Two round trips: CREATE + SET_INFO (delete disposition) + CLOSE, then a
    /// listing to check, because a server can report success and keep the stream:
    /// that's [`Error::StreamDeleteIgnored`]. A missing stream and a missing file
    /// are both [`ErrorKind::NotFound`](crate::ErrorKind::NotFound).
    pub async fn delete_stream(
        &self,
        conn: &mut Connection,
        path: &str,
        stream: &str,
    ) -> Result<()> {
        let conn = &mut self.pinned_conn(conn);
        let conn = &mut conn.pinned_to_session();
        let create = open_request(
            self.stream_wire_name(path, stream)?,
            FileAccessMask::DELETE | FileAccessMask::FILE_READ_ATTRIBUTES,
            CreateDisposition::FileOpen,
        );
        let disposition = SetInfoRequest {
            info_type: SetInfoType::File,
            file_info_class: FILE_DISPOSITION_INFORMATION,
            additional_information: 0,
            file_id: FileId::SENTINEL,
            buffer: vec![1], // DeletePending = true (MS-FSCC § 2.4.11).
        };
        let close = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };
        let ops = [
            self.compound_op(Command::Create, &create, 1),
            self.compound_op(Command::SetInfo, &disposition, 1),
            self.compound_op(Command::Close, &close, 1),
        ];
        let responses = all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        if responses[0].header.status != NtStatus::SUCCESS {
            let refused = Err(protocol(responses[0].header.status, Command::Create));
            return no_streams_if_refused(refused, path);
        }
        // A cascaded CLOSE fails with the op before it and leaves the handle
        // open on the server (pitfall 2).
        for (index, command) in [(1, Command::SetInfo), (2, Command::Close)] {
            let status = responses[index].header.status;
            if status != NtStatus::SUCCESS {
                let file_id =
                    CreateResponse::unpack(&mut ReadCursor::new(&responses[0].body))?.file_id;
                let _ = self.close_handle(conn, file_id).await;
                return Err(protocol(status, command));
            }
        }

        // A server can answer all of that with success and keep the stream, so
        // the answer isn't taken for the result. A listing that fails says
        // nothing either way, and the server did say yes.
        if let Ok(listed) = self.list_streams(conn, path).await {
            if listed.iter().any(|s| s.name.eq_ignore_ascii_case(stream)) {
                return Err(Error::StreamDeleteIgnored {
                    path: path.to_string(),
                    stream: stream.to_string(),
                });
            }
        }
        debug!("streams: deleted path={path:?} stream={stream:?}");
        Ok(())
    }

    /// What this share's file system says about itself, including whether it
    /// has named streams. One round trip: cache the answer per share.
    ///
    /// A Samba share reports `FILE_NAMED_STREAMS` only with a streams module
    /// loaded. The file system name is what the server chooses to say: Samba
    /// says `NTFS` on any disk unless told otherwise.
    pub async fn fs_attributes(&self, conn: &mut Connection) -> Result<FsAttributes> {
        trace!("streams: fs_attributes share={:?}", self.share_name);
        let query = QueryInfoRequest {
            info_type: InfoType::Filesystem,
            file_info_class: FILE_FS_ATTRIBUTE_INFORMATION,
            // The fixed part, and a file system name far longer than any.
            output_buffer_length: 1024,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        };
        let frame = self
            .query_compound(conn, String::new(), FILE_DIRECTORY_FILE, &query)
            .await?;
        if !frame.header.status.is_success_or_partial() {
            return Err(protocol(frame.header.status, Command::QueryInfo));
        }
        let resp = QueryInfoResponse::unpack(&mut ReadCursor::new(&frame.body))?;
        parse_fs_attributes(&resp.output_buffer)
    }
}

/// Create option: the target must be a directory.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

/// A WRITE of `data` at `offset`. `DataOffset` = header (64) + the WRITE's fixed
/// body (48) = 0x70.
fn sentinel_write(file_id: FileId, offset: u64, data: &[u8]) -> WriteRequest {
    WriteRequest {
        data_offset: 0x70,
        offset,
        file_id,
        channel: 0,
        remaining_bytes: 0,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
        flags: 0,
        data: data.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::client::test_helpers::{
        build_close_error_response, build_close_response, build_compound_response_frame,
        build_create_error_response, build_create_response, build_query_info_error_response,
        build_query_info_response, build_query_info_response_with_status, build_read_response,
        build_set_info_response, build_tree_connect_response, build_write_error_response,
        build_write_response, setup_connection,
    };
    use crate::msg::header::Header;
    use crate::msg::tree_connect::ShareType;
    use crate::transport::MockTransport;
    use crate::types::TreeId;
    use crate::ErrorKind;

    const FILE: FileId = FileId {
        persistent: 0x42,
        volatile: 0x99,
    };
    const TAG: &str = "com.apple.metadata:_kMDItemUserTags";
    const TAG_WIRE: &str = "com.apple.metadata\u{F022}_kMDItemUserTags";

    async fn connected() -> (Arc<MockTransport>, Connection, Tree) {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();
        (mock, conn, tree)
    }

    /// The commands of a sent compound, and its CREATE when it has one.
    fn sent(mock: &MockTransport, index: usize) -> (Vec<Command>, Option<CreateRequest>) {
        let bytes = mock.sent_message(index).unwrap();
        let mut commands = Vec::new();
        let mut create = None;
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&bytes[offset..]);
            let header = Header::unpack(&mut cursor).unwrap();
            if header.command == Command::Create {
                create = Some(CreateRequest::unpack(&mut cursor).unwrap());
            }
            commands.push(header.command);
            if header.next_command == 0 {
                return (commands, create);
            }
            offset += header.next_command as usize;
        }
    }

    fn stream_entry(name: &str, size: u64, next: u32) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut entry = Vec::new();
        entry.extend_from_slice(&next.to_le_bytes());
        entry.extend_from_slice(&(units.len() as u32).to_le_bytes());
        entry.extend_from_slice(&size.to_le_bytes());
        entry.extend_from_slice(&(size * 2).to_le_bytes());
        entry.extend_from_slice(&units);
        if next > 0 {
            entry.resize(next as usize, 0);
        }
        entry
    }

    /// A file with no named streams, as a listing answers it.
    fn empty_listing() -> Vec<u8> {
        build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(vec![]),
            build_close_response(),
        ])
    }

    // ── Parsers ───────────────────────────────────────────────────────

    #[test]
    fn a_stream_list_skips_the_default_stream_and_decodes_names() {
        let mut data = stream_entry("::$DATA", 7, 40);
        data.extend(stream_entry(
            ":com.apple.metadata\u{F022}_kMDItemUserTags:$DATA",
            63,
            0,
        ));
        let streams = parse_stream_information(&data).unwrap();
        assert_eq!(
            streams,
            [StreamInfo {
                name: TAG.to_string(),
                size: 63,
                allocation_size: 126,
            }]
        );
    }

    #[test]
    fn a_stream_list_skips_streams_that_arent_data() {
        let data = stream_entry(":index:$INDEX_ALLOCATION", 1, 0);
        assert!(parse_stream_information(&data).unwrap().is_empty());
    }

    #[test]
    fn an_empty_stream_list_is_empty() {
        assert!(parse_stream_information(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_stream_list_refuses_what_runs_off_its_buffer() {
        let mut data = stream_entry(":a:$DATA", 1, 0);
        data.truncate(data.len() - 2);
        assert!(parse_stream_information(&data).is_err());
        assert!(parse_stream_information(&[0u8; 10]).is_err());
    }

    #[test]
    fn a_next_offset_inside_its_own_entry_cant_loop_forever() {
        let data = stream_entry(":a:$DATA", 1, 8);
        assert!(parse_stream_information(&data).is_err());
    }

    #[test]
    fn a_stream_entry_without_its_separators_is_refused() {
        assert!(parse_stream_information(&stream_entry("plain", 1, 0)).is_err());
        assert!(parse_stream_information(&stream_entry(":plain", 1, 0)).is_err());
    }

    #[test]
    fn fs_attributes_read_the_named_streams_bit() {
        let name: Vec<u8> = "NTFS".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut data = Vec::new();
        data.extend_from_slice(&FILE_NAMED_STREAMS.to_le_bytes());
        data.extend_from_slice(&255u32.to_le_bytes());
        data.extend_from_slice(&(name.len() as u32).to_le_bytes());
        data.extend_from_slice(&name);
        let attributes = parse_fs_attributes(&data).unwrap();
        assert!(attributes.supports_named_streams);
        assert_eq!(attributes.file_system_name, "NTFS");
        assert_eq!(attributes.max_component_length, 255);

        data[0..4].copy_from_slice(&0u32.to_le_bytes());
        assert!(!parse_fs_attributes(&data).unwrap().supports_named_streams);
    }

    #[test]
    fn fs_attributes_refuse_a_name_past_the_buffer() {
        let mut data = vec![0u8; 12];
        data[8..12].copy_from_slice(&100u32.to_le_bytes());
        assert!(parse_fs_attributes(&data).is_err());
        assert!(parse_fs_attributes(&[0u8; 4]).is_err());
    }

    // ── The name on the wire ──────────────────────────────────────────

    #[tokio::test]
    async fn a_stream_name_travels_separately_from_the_path() {
        let (_, _, tree) = connected().await;
        assert_eq!(
            tree.stream_wire_name("dir/f.txt", TAG).unwrap(),
            format!("dir\\f.txt:{TAG_WIRE}")
        );
        // A `:` in the FILE name stays a name character, and is not a stream.
        assert_eq!(
            tree.stream_wire_name("a:b.txt", "s").unwrap(),
            "a\u{F022}b.txt:s"
        );
        // A trailing slash doesn't make it a stream on an unnamed child.
        assert_eq!(tree.stream_wire_name("dir/", "s").unwrap(), "dir:s");
        assert_eq!(tree.stream_wire_name("", "s").unwrap(), ":s");
    }

    #[tokio::test]
    async fn a_dfs_tree_prefixes_the_path_and_not_the_stream() {
        let (_, _, mut tree) = connected().await;
        tree.is_dfs = true;
        tree.server = "srv".to_string();
        assert_eq!(
            tree.stream_wire_name("f.txt", "s").unwrap(),
            "srv\\share\\f.txt:s"
        );
    }

    #[tokio::test]
    async fn no_stream_name_can_reach_the_file_itself() {
        let (_, _, tree) = connected().await;
        assert!(tree.stream_wire_name("f.txt", "").is_err());
        assert!(tree.stream_wire_name("f.txt", "a/b").is_err());
        // The default stream's own spelling is mapped into a harmless name.
        assert_eq!(
            tree.stream_wire_name("f.txt", ":$DATA").unwrap(),
            "f.txt:\u{F022}$DATA"
        );
    }

    // ── Compounds ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_stream_is_one_compound_on_the_stream() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 3),
            build_read_response(vec![1, 2, 3]),
            build_close_response(),
        ]));
        let data = tree.read_stream(&mut conn, "f.txt", TAG).await.unwrap();
        assert_eq!(data, [1, 2, 3]);
        let (commands, create) = sent(&mock, 1);
        assert_eq!(commands, [Command::Create, Command::Read, Command::Close]);
        assert_eq!(create.unwrap().name, format!("f.txt:{TAG_WIRE}"));
        assert_eq!(mock.sent_count(), 2);
    }

    #[tokio::test]
    async fn every_stream_open_shares_everything_and_takes_directories() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            crate::client::test_helpers::build_read_error_response(NtStatus::END_OF_FILE),
            build_close_response(),
        ]));
        tree.read_stream(&mut conn, "dir", "s").await.unwrap();
        // The probe, then the write.
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_write_response(1),
            build_close_response(),
        ]));
        tree.write_stream(&mut conn, "dir", "s", b"x")
            .await
            .unwrap();
        for index in [1, 3] {
            let create = sent(&mock, index).1.unwrap();
            assert_eq!(create.share_access.0, SHARE_ALL.0, "request {index}");
            assert_eq!(create.create_options, 0, "request {index}");
        }
    }

    #[tokio::test]
    async fn a_failed_read_closes_the_handle_on_its_own() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 3),
            crate::client::test_helpers::build_read_error_response(NtStatus::ACCESS_DENIED),
            build_close_error_response(NtStatus::ACCESS_DENIED),
        ]));
        mock.queue_response(build_close_response());
        let err = tree.read_stream(&mut conn, "f.txt", "s").await.unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::ACCESS_DENIED));
        assert_eq!(sent(&mock, 2).0, [Command::Close]);
    }

    #[tokio::test]
    async fn a_stream_bigger_than_the_read_comes_back_whole() {
        let (mock, mut conn, tree) = connected().await;
        // The compound asks for 64 KiB of a 70,000-byte stream, and is told so.
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 70_000),
            build_read_response(vec![1; 65536]),
            build_close_response(),
        ]));
        mock.queue_response(build_create_response(FILE, 70_000));
        mock.queue_response(build_read_response(vec![1; 65536]));
        mock.queue_response(build_read_response(vec![1; 4464]));
        mock.queue_response(crate::client::test_helpers::build_read_error_response(
            NtStatus::END_OF_FILE,
        ));
        mock.queue_response(build_close_response());
        let data = tree.read_stream(&mut conn, "f.txt", "s").await.unwrap();
        assert_eq!(data.len(), 70_000);
        assert_eq!(
            *sent(&mock, mock.sent_count() - 1).0.last().unwrap(),
            Command::Close
        );
    }

    #[tokio::test]
    async fn writing_to_a_missing_file_never_opens_the_stream() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_close_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ]));
        let err = tree
            .write_stream(&mut conn, "gone.txt", "s", b"x")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        // TreeConnect and the probe, which opened the file and not the stream.
        assert_eq!(mock.sent_count(), 2);
        let probe = sent(&mock, 1).1.unwrap();
        assert_eq!(probe.name, "gone.txt");
        assert_eq!(probe.create_disposition, CreateDisposition::FileOpen);
    }

    #[tokio::test]
    async fn a_write_replaces_the_stream_in_one_compound() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_write_response(3),
            build_close_response(),
        ]));
        tree.write_stream(&mut conn, "f.txt", TAG, b"abc")
            .await
            .unwrap();
        let (commands, create) = sent(&mock, 2);
        assert_eq!(commands, [Command::Create, Command::Write, Command::Close]);
        let create = create.unwrap();
        assert_eq!(
            create.create_disposition,
            CreateDisposition::FileOverwriteIf
        );
        assert_eq!(create.name, format!("f.txt:{TAG_WIRE}"));
    }

    #[tokio::test]
    async fn an_empty_value_sends_no_write() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        tree.write_stream(&mut conn, "f.txt", "s", b"")
            .await
            .unwrap();
        assert_eq!(sent(&mock, 2).0, [Command::Create, Command::Close]);
    }

    #[tokio::test]
    async fn a_refused_write_closes_the_handle_and_leaves_no_stream() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_write_error_response(NtStatus::FILE_SYSTEM_LIMITATION),
            build_close_error_response(NtStatus::FILE_SYSTEM_LIMITATION),
        ]));
        mock.queue_response(build_close_response());
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        mock.queue_response(empty_listing());
        let err = tree
            .write_stream(&mut conn, "f.txt", "s", b"too big")
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::FILE_SYSTEM_LIMITATION));
        assert_eq!(sent(&mock, 3).0, [Command::Close]);
        let (commands, create) = sent(&mock, 4);
        assert_eq!(
            commands,
            [Command::Create, Command::SetInfo, Command::Close]
        );
        assert_eq!(create.unwrap().name, "f.txt:s");
    }

    #[tokio::test]
    async fn a_short_write_is_an_error() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_write_response(1),
            build_close_response(),
        ]));
        assert!(tree
            .write_stream(&mut conn, "f.txt", "s", b"abc")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn deleting_a_stream_opens_the_stream_and_not_the_file() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        mock.queue_response(empty_listing());
        tree.delete_stream(&mut conn, "f.txt", TAG).await.unwrap();
        let (commands, create) = sent(&mock, 1);
        assert_eq!(
            commands,
            [Command::Create, Command::SetInfo, Command::Close]
        );
        assert_eq!(create.unwrap().name, format!("f.txt:{TAG_WIRE}"));
    }

    /// A QNAP answers success for some streams and keeps them.
    #[tokio::test]
    async fn a_delete_the_server_ignored_is_not_a_success() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_set_info_response(),
            build_close_response(),
        ]));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(stream_entry(":s:$DATA", 5, 0)),
            build_close_response(),
        ]));
        let err = tree
            .delete_stream(&mut conn, "f.txt", "s")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::StreamDeleteIgnored { .. }), "{err:?}");
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn a_refused_stream_delete_closes_the_handle() {
        let (mock, mut conn, tree) = connected().await;
        let mut refused = crate::msg::header::Header::new_request(Command::SetInfo);
        refused.flags.set_response();
        refused.credits = 32;
        refused.status = NtStatus::ACCESS_DENIED;
        let refused = crate::client::connection::pack_message(
            &refused,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            refused,
            build_close_error_response(NtStatus::ACCESS_DENIED),
        ]));
        mock.queue_response(build_close_response());
        let err = tree
            .delete_stream(&mut conn, "f.txt", "s")
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::ACCESS_DENIED));
        assert_eq!(sent(&mock, 2).0, [Command::Close]);
    }

    #[tokio::test]
    async fn listing_streams_opens_the_file_and_asks_class_22() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(stream_entry(":s:$DATA", 5, 0)),
            build_close_response(),
        ]));
        let streams = tree.list_streams(&mut conn, "dir/").await.unwrap();
        assert_eq!(streams[0].name, "s");
        let (commands, create) = sent(&mock, 1);
        assert_eq!(
            commands,
            [Command::Create, Command::QueryInfo, Command::Close]
        );
        assert_eq!(create.unwrap().name, "dir");
    }

    /// Pitfall 10: a cut-off list reads as "no such stream", so it's asked for
    /// again with room, and never returned as it stands.
    #[tokio::test]
    async fn a_stream_list_that_overflowed_is_asked_for_again() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response_with_status(
                NtStatus::BUFFER_OVERFLOW,
                stream_entry(":a:$DATA", 1, 0),
            ),
            build_close_response(),
        ]));
        let mut whole = stream_entry(":a:$DATA", 1, 40);
        whole.extend(stream_entry(":b:$DATA", 1, 0));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(whole),
            build_close_response(),
        ]));
        let streams = tree.list_streams(&mut conn, "f.txt").await.unwrap();
        assert_eq!(streams.len(), 2);
    }

    #[tokio::test]
    async fn a_stream_list_that_never_fits_is_an_error_and_not_a_short_list() {
        let (mock, mut conn, tree) = connected().await;
        for _ in 0..2 {
            mock.queue_response(build_compound_response_frame(&[
                build_create_response(FILE, 0),
                build_query_info_response_with_status(NtStatus::BUFFER_OVERFLOW, vec![]),
                build_close_response(),
            ]));
        }
        assert!(tree.list_streams(&mut conn, "f.txt").await.is_err());
    }

    #[tokio::test]
    async fn a_refused_stream_list_closes_the_handle() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            build_close_error_response(NtStatus::NOT_SUPPORTED),
        ]));
        mock.queue_response(build_close_response());
        let err = tree.list_streams(&mut conn, "f.txt").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
        assert_eq!(sent(&mock, 2).0, [Command::Close]);
    }
}
