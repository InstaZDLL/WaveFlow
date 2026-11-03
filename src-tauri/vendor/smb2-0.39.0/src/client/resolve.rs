//! Which file, exactly, did the server open?
//!
//! A consumer enforcing a policy against a path needs the name the server
//! resolved it to, which is not always the name it asked for: an 8.3 alias
//! (`PROGRA~1`) and case folding (`readme.TXT` for `README.txt`) both open a
//! file under a name that says nothing client-side about which file it is.
//! [`Tree::resolve`] asks the server, in one round trip.
//!
//! ## Two ways to ask, and neither works everywhere
//!
//! - **`FileNormalizedNameInformation` (class 48, MS-FSCC § 2.4.35)** is the
//!   question this module wants answered: the path with every 8.3 component
//!   replaced by its long name and every component in its on-disk casing,
//!   relative to the share root (MS-SMB2 § 3.3.5.20.1 makes the server refuse
//!   it otherwise). Servers MUST refuse it on dialects 2.0.2, 2.1, and 3.0.2,
//!   and Windows answers it from 10 / Server v1803 on.
//! - **`FileAllInformation` (class 18)** carries a `FileNameInformation` too,
//!   and it is the fallback. ⚠️ A server SHOULD return that name EMPTY
//!   (MS-SMB2 § 3.3.5.20.1), and current Windows does. Only Windows
//!   2008–2012 R2 filled it, with an absolute path whose root the spec leaves
//!   open. So an empty name means "not answered", never "the share root", and
//!   a filled one is trusted only for as many trailing components as the
//!   caller asked for (see `name_from_all_information`).
//!
//! Measured 2026-09-23 on Samba 4.20.6 (the `smb-guest` fixture), Samba 4.22
//! (a Raspberry Pi), and a QNAP TS-464, all SMB 3.1.1, for a file created as
//! `smb2-probe-Dir/MixedCase Name.txt` and opened as
//! `SMB2-PROBE-DIR/mixedcase name.TXT`: class 48 answered
//! `smb2-probe-Dir\MixedCase Name.txt` (no leading `\`) and class 18
//! `\smb2-probe-Dir\MixedCase Name.txt`. Opening the Samba 8.3 alias
//! (`FileAlternateNameInformation`, class 21) and asking class 48 gave the
//! long name back.
//!
//! ## One compound, ordered for servers that cascade failures
//!
//! MS-SMB2 § 3.3.5.2.7.2 lets a server fail every related operation after a
//! failed one with the same status. So the order is: CREATE, the two queries
//! `stat` needs, class 18, the two identity queries, class 48, CLOSE. Class 48
//! goes last because it is the one most likely to fail (every Windows before
//! v1803), and a failure there must not take the metadata or the class 18
//! fallback with it. It still takes the CLOSE, which is why a CLOSE that did
//! not succeed gets a standalone one (pitfall 2).
//!
//! ## DFS
//!
//! On a DFS share the CREATE carries `server\share\path` (MS-SMB2 § 3.2.4.3),
//! but class 48 is relative to the share by definition, so it needs no
//! stripping. Class 18's trailing-component rule strips any prefix the server
//! puts in front. [`SmbClient::resolve`](crate::SmbClient::resolve) follows a
//! DFS link the way `stat` does, and the path it returns is then relative to
//! the TARGET share, which is the tree the caller holds afterwards.

use log::{trace, warn};

use crate::client::connection::{CompoundOp, Connection, Frame};
use crate::client::durable::FileIdentity;
use crate::client::tree::{
    file_info_from, FileInfo, Tree, FILE_BASIC_INFORMATION, FILE_STANDARD_INFORMATION,
};
use crate::error::{Error, Result};
use crate::msg::close::CloseRequest;
use crate::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use crate::msg::query_info::{InfoType, QueryInfoRequest, QueryInfoResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::types::flags::FileAccessMask;
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, FileId, OplockLevel};

/// `FileAllInformation` (MS-FSCC § 2.4.2).
const FILE_ALL_INFORMATION: u8 = 18;
/// `FileNormalizedNameInformation` (MS-FSCC § 2.4.35).
const FILE_NORMALIZED_NAME_INFORMATION: u8 = 48;
/// Where `FileNameInformation` starts inside `FileAllInformation`: past Basic
/// (40), Standard (24), Internal (8), EA (4), Access (4), Position (8), Mode
/// (4), and Alignment (4).
const ALL_INFORMATION_NAME_OFFSET: usize = 96;

/// What a path resolved to on the server: the name it stores the file under,
/// plus what [`Tree::stat`] would say about it.
///
/// Returned by [`Tree::resolve`] and
/// [`SmbClient::resolve`](crate::SmbClient::resolve).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Resolved {
    /// The path as the server stores it: relative to the share, `/`-separated,
    /// in on-disk casing, with any 8.3 alias replaced by its long name. Names
    /// carrying characters SMB2 can't transport come back in the caller's form
    /// (see [`crate::name`]), so this path can be handed straight back to any
    /// `Tree` method. The share root is `""`.
    pub path: String,
    /// Size, timestamps, and whether it's a directory.
    pub info: FileInfo,
    /// Which file this is on the server (index number plus volume serial), or
    /// `None` when the server wouldn't say. Stable across renames, so it's the
    /// thing to compare when "is this the same file?" matters more than the
    /// name.
    pub identity: Option<FileIdentity>,
}

/// Which handle [`Tree::open_and_name`] opens, which decides what else it can
/// ask in the same round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenFor {
    /// A [`FileReader`](crate::FileReader): adds the class 18 name fallback,
    /// which needs the `FILE_READ_ATTRIBUTES` a read handle carries.
    Reading,
    /// A [`FileWriter`](crate::FileWriter): adds the file's
    /// [`FileIdentity`], which a checkpoint carries so a resume can prove it
    /// reopened the same file. No class 18: a write handle has no
    /// `FILE_READ_ATTRIBUTES`, and ❌ adding it could make an open that works
    /// today fail with `ACCESS_DENIED`. The identity classes need no access
    /// at all.
    Writing,
}

/// What [`Tree::open_and_name`] opened.
pub(super) struct NamedOpen {
    pub(super) created: CreateResponse,
    /// What the server calls the file, if it said.
    pub(super) name: Option<String>,
    /// Which file it is, for a [`OpenFor::Writing`] open whose server said
    /// so in full (see [`FileIdentity::is_proof`]).
    pub(super) identity: Option<FileIdentity>,
}

/// A `QUERY_INFO` for a name-carrying class, compounded onto a CREATE.
///
/// The buffer has room for the longest path a server stores (32,767 UTF-16
/// units), capped at 64 KiB so the request stays one credit, and at the
/// server's `MaxTransactSize`, above which it refuses the request outright.
/// A name that still doesn't fit comes back `STATUS_BUFFER_OVERFLOW`, and a
/// truncated name is never used.
fn name_query(conn: &Connection, class: u8) -> QueryInfoRequest {
    QueryInfoRequest {
        info_type: InfoType::File,
        file_info_class: class,
        output_buffer_length: conn
            .params()
            .map(|p| p.max_transact_size.min(65536))
            .unwrap_or(65536),
        additional_information: 0,
        flags: 0,
        file_id: FileId::SENTINEL,
        input_buffer: vec![],
    }
}

/// A `QUERY_INFO` for a fixed-size class, compounded onto a CREATE.
fn fixed_query(class: u8, len: u32) -> QueryInfoRequest {
    QueryInfoRequest {
        info_type: InfoType::File,
        file_info_class: class,
        output_buffer_length: len,
        additional_information: 0,
        flags: 0,
        file_id: FileId::SENTINEL,
        input_buffer: vec![],
    }
}

/// The `FILE_NAME_INFORMATION` at `offset` in a successful answer, decoded
/// from UTF-16LE. `None` for anything short of a complete, well-formed name:
/// a failed or truncated (`STATUS_BUFFER_OVERFLOW`) answer, a length that
/// runs past the buffer, or invalid UTF-16.
fn file_name_at(frame: &Frame, offset: usize) -> Option<String> {
    if frame.header.status != NtStatus::SUCCESS {
        return None;
    }
    let buf = QueryInfoResponse::unpack(&mut ReadCursor::new(&frame.body))
        .ok()?
        .output_buffer;
    let len_bytes = buf.get(offset..offset + 4)?;
    let len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
    let name = buf.get(offset + 4..(offset + 4).checked_add(len)?)?;
    super::watcher::decode_utf16le(name).ok()
}

/// Class 48's answer in wire form: relative to the share, with the leading
/// `\` Windows puts on an absolute name removed (Samba sends none).
fn name_from_normalized(raw: &str) -> String {
    raw.strip_prefix('\\').unwrap_or(raw).to_string()
}

/// Class 18's answer in wire form, cut down to the components the caller
/// actually named.
///
/// Its root is not pinned down anywhere: Samba sends `\dir\file` relative to
/// the share, and the Windows versions that fill it at all send "an absolute
/// path" (MS-SMB2 § 3.3.5.20.1, note 420), which on a DFS share or a share
/// below a volume root carries components the caller never wrote. Resolving
/// maps each component to its stored form one for one (an 8.3 alias is one
/// component, and so is its long name), so the last N components of the
/// answer are the file the caller's N components named, whatever came before
/// them. An empty answer is the spec's "SHOULD be empty", not the share root.
fn name_from_all_information(raw: &str, requested_wire: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let wanted = requested_wire.split('\\').filter(|c| !c.is_empty()).count();
    let parts: Vec<&str> = raw.split('\\').filter(|c| !c.is_empty()).collect();
    let skip = parts.len().checked_sub(wanted)?;
    Some(parts[skip..].join("\\"))
}

/// Pick the server's name for the file out of the two name answers, in
/// caller form. Class 48 wins when it answered; class 18 is the fallback.
/// `None` when neither is usable.
fn resolved_name(
    requested_wire: &str,
    all: Option<&Frame>,
    normalized: Option<&Frame>,
) -> Option<String> {
    let wire = normalized
        .and_then(|f| file_name_at(f, 0))
        .map(|raw| name_from_normalized(&raw))
        .or_else(|| {
            all.and_then(|f| file_name_at(f, ALL_INFORMATION_NAME_OFFSET))
                .and_then(|raw| name_from_all_information(&raw, requested_wire))
        })?;
    Some(crate::name::decode_path(&wire))
}

impl Tree {
    /// Ask the server which file `path` names, and what it calls it.
    ///
    /// Returns the path as the server stores it (case and 8.3 aliases
    /// resolved), the same metadata as [`stat`](Self::stat), and the file's
    /// [`FileIdentity`] when the server provides one. A consumer checking a
    /// policy against a path should check [`Resolved::path`], because the
    /// server may have opened `PROGRA~1` or `readme.TXT` as a differently
    /// named file.
    ///
    /// One round trip: CREATE, the name and metadata queries, and CLOSE go out
    /// as one compound. To know the name of a file you're about to read or
    /// write without a race between the check and the I/O, use
    /// [`FileReader::resolved_path`](crate::FileReader::resolved_path) or
    /// [`FileWriter::resolved_path`](crate::FileWriter::resolved_path), which
    /// record it when the handle opens.
    ///
    /// # Errors
    ///
    /// The CREATE's error when the path doesn't open (not found, access
    /// denied, and so on). When it opens but the server won't name it (SMB
    /// 2.x or 3.0.2, or Windows before 10 / Server v1803), an error classified
    /// [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported), so a
    /// consumer can branch to a fallback such as listing the parent.
    #[doc(alias = "canonicalize")]
    #[doc(alias = "realpath")]
    pub async fn resolve(&self, conn: &mut Connection, path: &str) -> Result<Resolved> {
        let conn = &mut self.pinned_conn(conn);
        let wire_path = self.format_path(path);
        let requested_wire = crate::name::encode_path(path);
        trace!("tree: resolve path={path:?}");

        // The same open `stat` uses: FILE_READ_ATTRIBUTES is what class 18
        // and FileBasicInformation need (MS-SMB2 § 3.3.5.20.1); class 48 and
        // the identity queries need nothing more.
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
            create_options: 0,
            name: wire_path,
            create_contexts: vec![],
        };
        let basic_req = fixed_query(FILE_BASIC_INFORMATION, 40);
        let std_req = fixed_query(FILE_STANDARD_INFORMATION, 24);
        let all_req = name_query(conn, FILE_ALL_INFORMATION);
        let index_req = FileIdentity::index_query();
        let volume_req = FileIdentity::volume_query();
        let normalized_req = name_query(conn, FILE_NORMALIZED_NAME_INFORMATION);
        let close_req = CloseRequest {
            flags: 0,
            file_id: FileId::SENTINEL,
        };

        // Order matters: see the module docs on cascading failures.
        let op = |command, body| CompoundOp {
            command,
            body,
            tree_id: Some(self.tree_id),
            credit_charge: CreditCharge(1),
        };
        let ops = [
            op(Command::Create, &create_req),
            op(Command::QueryInfo, &basic_req),
            op(Command::QueryInfo, &std_req),
            op(Command::QueryInfo, &all_req),
            op(Command::QueryInfo, &index_req),
            op(Command::QueryInfo, &volume_req),
            op(Command::QueryInfo, &normalized_req),
            op(Command::Close, &close_req),
        ];
        let frames = super::tree::all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        let [create, basic, std, all, index, volume, normalized, close] = &frames[..] else {
            unreachable!("all_or_first_err checked the count");
        };

        if create.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create.header.status,
                command: Command::Create,
            });
        }

        let outcome =
            self.resolved_from(&requested_wire, basic, std, all, index, volume, normalized);

        // A CLOSE that didn't succeed left the handle open, most often because
        // a query ahead of it failed and the server cascaded that failure.
        if close.header.status != NtStatus::SUCCESS {
            let file_id = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?.file_id;
            trace!(
                "tree: resolve's compound CLOSE returned {:?}, closing standalone",
                close.header.status
            );
            let _ = self.close_handle(conn, file_id).await;
        }

        let resolved = outcome?;
        trace!("tree: resolve done, path={:?}", resolved.path);
        Ok(resolved)
    }

    /// Open with `create_req` and ask what the server calls the file it
    /// opened, in the same round trip. The handle a
    /// [`FileReader`](crate::FileReader) or [`FileWriter`](crate::FileWriter)
    /// keeps is the one named, so a policy check on the name can't race the
    /// I/O that follows.
    ///
    /// What else rides along depends on `handle` (see [`OpenFor`]). Class 48
    /// goes last because it's the query most likely to be refused, and a
    /// server that cascades failures (MS-SMB2 § 3.3.5.2.7.2) would otherwise
    /// take everything after it down too. No query can fail the open: a
    /// server that answers none of them gives `None` for each.
    pub(super) async fn open_and_name(
        &self,
        conn: &mut Connection,
        path: &str,
        create_req: &CreateRequest,
        handle: OpenFor,
    ) -> Result<NamedOpen> {
        let conn = &mut self.pinned_conn(conn);
        let all_req = name_query(conn, FILE_ALL_INFORMATION);
        let index_req = FileIdentity::index_query();
        let volume_req = FileIdentity::volume_query();
        let normalized_req = name_query(conn, FILE_NORMALIZED_NAME_INFORMATION);
        let op = |command, body| CompoundOp {
            command,
            body,
            tree_id: Some(self.tree_id),
            credit_charge: CreditCharge(1),
        };
        let mut ops = vec![op(Command::Create, create_req)];
        match handle {
            OpenFor::Reading => ops.push(op(Command::QueryInfo, &all_req)),
            OpenFor::Writing => {
                ops.push(op(Command::QueryInfo, &index_req));
                ops.push(op(Command::QueryInfo, &volume_req));
            }
        }
        ops.push(op(Command::QueryInfo, &normalized_req));

        let frames = super::tree::all_or_first_err(conn.execute_compound(&ops).await?, ops.len())?;
        let create = &frames[0];
        if create.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create.header.status,
                command: Command::Create,
            });
        }
        let created = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?;
        let (all, identity) = match handle {
            OpenFor::Reading => (Some(&frames[1]), None),
            OpenFor::Writing => (
                None,
                FileIdentity::from_frames(Some(&frames[1]), Some(&frames[2]))
                    .filter(FileIdentity::is_proof),
            ),
        };
        let name = resolved_name(&crate::name::encode_path(path), all, frames.last());
        trace!("tree: opened path={path:?} name={name:?} identity={identity:?}");
        Ok(NamedOpen {
            created,
            name,
            identity,
        })
    }

    /// Everything after the CREATE: metadata (required), identity
    /// (optional), and the name (required, from either class).
    #[allow(clippy::too_many_arguments)]
    fn resolved_from(
        &self,
        requested_wire: &str,
        basic: &Frame,
        std: &Frame,
        all: &Frame,
        index: &Frame,
        volume: &Frame,
        normalized: &Frame,
    ) -> Result<Resolved> {
        for (frame, what) in [(basic, "basic"), (std, "standard")] {
            if !frame.header.status.is_success_or_partial() {
                warn!(
                    "tree: resolve's QUERY_INFO ({}) failed ({:?})",
                    what, frame.header.status
                );
                return Err(Error::Protocol {
                    status: frame.header.status,
                    command: Command::QueryInfo,
                });
            }
        }
        let info = file_info_from(&basic.body, &std.body)?;
        let identity = FileIdentity::from_frames(Some(index), Some(volume));

        let Some(path) = resolved_name(requested_wire, Some(all), Some(normalized)) else {
            trace!(
                "tree: resolve got no usable name (class 48: {:?}, class 18: {:?})",
                normalized.header.status,
                all.header.status
            );
            // The file opened, so whatever the name queries said, what it
            // amounts to is that this server can't name it.
            return Err(Error::Protocol {
                status: NtStatus::NOT_SUPPORTED,
                command: Command::QueryInfo,
            });
        };
        Ok(Resolved {
            path,
            info,
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_helpers::{
        build_close_error_response, build_close_response, build_compound_response_frame,
        build_create_error_response, build_create_response, build_query_info_error_response,
        build_query_info_response, build_query_info_response_with_status, file_all_information,
        file_name_information, setup_connection,
    };
    use crate::transport::MockTransport;
    use crate::types::TreeId;
    use crate::ErrorKind;
    use std::sync::Arc;

    const FILE: FileId = FileId {
        persistent: 0x51,
        volatile: 0x52,
    };

    fn a_share() -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(30),
            share_name: "public".to_string(),
            server: "nas:445".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    /// `FileBasicInformation` with an ARCHIVE attribute and recognizable times.
    fn basic() -> Vec<u8> {
        let mut out = Vec::new();
        for t in [11u64, 22, 33, 44] {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out.extend_from_slice(&0x20u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    /// `FileStandardInformation` for a 1,234-byte file.
    fn standard() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&4096u64.to_le_bytes());
        out.extend_from_slice(&1234u64.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    fn index(n: u64) -> Vec<u8> {
        n.to_le_bytes().to_vec()
    }

    fn volume(serial: u32) -> Vec<u8> {
        let mut out = vec![0u8; 8];
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&[0u8; 6]);
        out
    }

    /// One answer per position in the compound, in the order `resolve` sends.
    struct Answer {
        basic: Vec<u8>,
        all: Vec<u8>,
        index: Vec<u8>,
        volume: Vec<u8>,
        normalized: Vec<u8>,
        close: Vec<u8>,
    }

    impl Answer {
        /// Samba's shape: both name classes answered, identity too.
        fn samba(normalized: &str, all: &str) -> Self {
            Self {
                basic: build_query_info_response(basic()),
                all: build_query_info_response(file_all_information(all)),
                index: build_query_info_response(index(0x77)),
                volume: build_query_info_response(volume(0xBEEF)),
                normalized: build_query_info_response(file_name_information(normalized)),
                close: build_close_response(),
            }
        }

        fn frame(self) -> Vec<u8> {
            build_compound_response_frame(&[
                build_create_response(FILE, 1234),
                self.basic,
                build_query_info_response(standard()),
                self.all,
                self.index,
                self.volume,
                self.normalized,
                self.close,
            ])
        }
    }

    async fn resolve_with(answer: Answer, path: &str) -> (Arc<MockTransport>, Result<Resolved>) {
        resolve_on(a_share(), answer, path).await
    }

    async fn resolve_on(
        tree: Tree,
        answer: Answer,
        path: &str,
    ) -> (Arc<MockTransport>, Result<Resolved>) {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(answer.frame());
        // Room for a standalone CLOSE, should the test's answer call for one.
        mock.queue_response(build_close_response());
        let result = tree.resolve(&mut conn, path).await;
        (mock, result)
    }

    #[tokio::test]
    async fn the_normalized_name_is_the_answer_when_the_server_gives_one() {
        let (mock, result) = resolve_with(
            Answer::samba("Docs\\MixedCase Name.txt", "\\Docs\\MixedCase Name.txt"),
            "DOCS/mixedcase name.TXT",
        )
        .await;
        let resolved = result.expect("resolve must succeed");
        assert_eq!(resolved.path, "Docs/MixedCase Name.txt");
        assert_eq!(resolved.info.size, 1234);
        assert!(!resolved.info.is_directory);
        assert_eq!(
            resolved.identity,
            Some(FileIdentity {
                volume_serial: 0xBEEF,
                index_number: 0x77,
            })
        );
        assert_eq!(mock.sent_count(), 1, "one round trip, no standalone CLOSE");
    }

    #[tokio::test]
    async fn the_request_is_one_compound_asking_both_name_classes() {
        let (mock, _) = resolve_with(Answer::samba("a.txt", "\\a.txt"), "a.txt").await;
        let sent = mock.sent_message(0).unwrap();
        let mut classes = Vec::new();
        let mut offset = 0usize;
        loop {
            let mut cursor = ReadCursor::new(&sent[offset..]);
            let header = crate::msg::header::Header::unpack(&mut cursor).unwrap();
            if header.command == Command::QueryInfo {
                let req = QueryInfoRequest::unpack(&mut cursor).unwrap();
                classes.push((req.info_type, req.file_info_class));
            }
            if header.next_command == 0 {
                break;
            }
            offset += header.next_command as usize;
        }
        assert_eq!(
            classes,
            vec![
                (InfoType::File, FILE_BASIC_INFORMATION),
                (InfoType::File, FILE_STANDARD_INFORMATION),
                (InfoType::File, FILE_ALL_INFORMATION),
                (InfoType::File, 6),
                (InfoType::Filesystem, 1),
                (InfoType::File, FILE_NORMALIZED_NAME_INFORMATION),
            ]
        );
    }

    #[tokio::test]
    async fn a_leading_backslash_on_the_normalized_name_is_dropped() {
        // Windows returns class 48 as an absolute name; Samba doesn't.
        let (_, result) = resolve_with(
            Answer::samba("\\Docs\\Report.txt", "\\Docs\\Report.txt"),
            "docs/report.txt",
        )
        .await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");
    }

    #[tokio::test]
    async fn names_in_the_private_use_area_come_back_in_caller_form() {
        // AGENTS.md pitfall 23: `?` travels as U+F025, and a caller must get
        // back the path they'd write, or the next CREATE can't open it.
        let (_, result) = resolve_with(
            Answer::samba("Dir\\who\u{F025}.txt", "\\Dir\\who\u{F025}.txt"),
            "dir/WHO?.txt",
        )
        .await;
        assert_eq!(result.unwrap().path, "Dir/who?.txt");
    }

    #[tokio::test]
    async fn file_all_information_is_the_fallback_when_class_48_is_refused() {
        // Windows cascades a failure onto every related op after it (MS-SMB2
        // § 3.3.5.2.7.2), so a refused class 48 takes the CLOSE with it.
        let mut answer = Answer::samba("unused", "\\Docs\\Report.txt");
        answer.normalized = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        answer.close = build_close_error_response(NtStatus::NOT_SUPPORTED);
        let (mock, result) = resolve_with(answer, "docs/report.txt").await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");
        assert_eq!(
            mock.sent_count(),
            2,
            "the handle the cascaded CLOSE left open gets a standalone CLOSE"
        );
    }

    #[tokio::test]
    async fn file_all_information_keeps_only_the_components_the_caller_named() {
        // Windows 2008–2012 R2 fill class 18 with an absolute path, which can
        // start above the share root.
        let mut answer = Answer::samba("unused", "\\Shares\\Public\\Docs\\Report.txt");
        answer.normalized = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        let (_, result) = resolve_with(answer, "docs/report.txt").await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");
    }

    #[tokio::test]
    async fn on_a_dfs_share_the_server_and_share_prefix_never_reaches_the_caller() {
        let mut tree = a_share();
        tree.is_dfs = true;
        let mut answer = Answer::samba("unused", "\\nas\\public\\Docs\\Report.txt");
        answer.normalized = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        let (_, result) = resolve_on(tree.clone(), answer, "docs/report.txt").await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");

        // Class 48 is share-relative by definition, DFS or not.
        let (_, result) = resolve_on(
            tree,
            Answer::samba("Docs\\Report.txt", "\\nas\\public\\Docs\\Report.txt"),
            "docs/report.txt",
        )
        .await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");
    }

    #[tokio::test]
    async fn a_truncated_normalized_name_is_never_used() {
        // A name cut off by STATUS_BUFFER_OVERFLOW could name a different
        // file, which is the one thing a policy check can't afford.
        let mut answer = Answer::samba("unused", "\\Docs\\Report.txt");
        answer.normalized = build_query_info_response_with_status(
            NtStatus::BUFFER_OVERFLOW,
            file_name_information("Docs\\Rep"),
        );
        let (_, result) = resolve_with(answer, "docs/report.txt").await;
        assert_eq!(result.unwrap().path, "Docs/Report.txt");
    }

    #[tokio::test]
    async fn a_server_that_names_nothing_is_reported_as_unsupported() {
        // Current Windows below v1803: class 48 refused, class 18's name empty.
        let mut answer = Answer::samba("unused", "");
        answer.normalized = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        answer.close = build_close_error_response(NtStatus::NOT_SUPPORTED);
        let (mock, result) = resolve_with(answer, "docs/report.txt").await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Unsupported);
        assert_eq!(mock.sent_count(), 2, "the handle is still closed");
    }

    #[tokio::test]
    async fn an_empty_all_information_name_is_not_the_share_root() {
        let mut answer = Answer::samba("unused", "");
        answer.normalized = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        let (_, result) = resolve_with(answer, "").await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn the_share_root_resolves_to_the_empty_path() {
        let (_, result) = resolve_with(Answer::samba("", "\\"), "").await;
        assert_eq!(result.unwrap().path, "");
    }

    #[tokio::test]
    async fn a_server_that_will_not_identify_the_file_still_resolves_it() {
        let mut answer = Answer::samba("Docs\\Report.txt", "\\Docs\\Report.txt");
        answer.index = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        answer.volume = build_query_info_error_response(NtStatus::NOT_SUPPORTED);
        let (_, result) = resolve_with(answer, "docs/report.txt").await;
        let resolved = result.unwrap();
        assert_eq!(resolved.path, "Docs/Report.txt");
        assert_eq!(resolved.identity, None);
    }

    #[tokio::test]
    async fn a_failed_metadata_query_fails_the_call_and_closes_the_handle() {
        let mut answer = Answer::samba("Docs\\Report.txt", "\\Docs\\Report.txt");
        answer.basic = build_query_info_error_response(NtStatus::ACCESS_DENIED);
        answer.close = build_close_error_response(NtStatus::ACCESS_DENIED);
        let (mock, result) = resolve_with(answer, "docs/report.txt").await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::AccessDenied);
        assert_eq!(mock.sent_count(), 2, "compound + standalone CLOSE");
    }

    #[tokio::test]
    async fn a_path_that_does_not_open_reports_the_create_error() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let refused = || build_query_info_error_response(NtStatus::OBJECT_NAME_NOT_FOUND);
        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            refused(),
            refused(),
            refused(),
            refused(),
            refused(),
            refused(),
            build_close_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ]));
        let err = a_share()
            .resolve(&mut conn, "missing.txt")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert_eq!(mock.sent_count(), 1, "nothing opened, nothing to close");
    }
}
