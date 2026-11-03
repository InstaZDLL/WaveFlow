//! Durable handles: an open that outlives the connection carrying it.
//!
//! A 768-file copy to a NAS used to end at the first blip. M2 made the blip
//! visible and M3.1 made the connection come back, but a write halfway through
//! a 4 GB file still started that file over. A durable handle is what turns
//! "start again" into "carry on": the server keeps the open alive for a while
//! after the connection dies, and the client claims it back.
//!
//! ## The data-safety rule this module exists to enforce
//!
//! Reclaiming the wrong handle writes bytes into the wrong file, which is far
//! worse than a failed transfer. So a reclaim here is not "the server said
//! yes"; it is **two independent proofs**, and anything less closes the handle
//! and fails:
//!
//! 1. **The server matched our open.** The `DH2C` reconnect context carries
//!    the 16-byte `CreateGuid` this client chose at open time and the server
//!    stored with the open. MS-SMB2 § 3.3.5.9.12 makes the server compare it,
//!    so a grant means it found *this client's* open rather than something
//!    else living at the same `FileId`. This is why only SMB 3.x v2 handles
//!    are ever reclaimed — see [`crate::msg::create_context`] for why v1's
//!    `FileId`-only shape is not good enough.
//! 2. **The open still points at the same bytes.** `QUERY_INFO` for
//!    `FileInternalInformation` (MS-FSCC § 2.4.22) returns the file's index
//!    number — its inode, on a POSIX server — and `FileFsVolumeInformation`
//!    (MS-FSCC § 2.5.9) says which volume that inode belongs to, which matters
//!    because a share can span mount points and inode numbers only have to be
//!    unique within a filesystem. Both are read at open and read again after
//!    the reclaim, and both must match. This proof does not depend on the
//!    server implementing the `CreateGuid` comparison correctly, which is the
//!    point of having two.
//!
//!    The index number is required; the volume serial is best-effort. A server
//!    that will not answer it reports 0 on both sides, so the comparison
//!    degrades to the index number alone and never to a weaker match than
//!    that — while a server that answers on one side and not the other fails
//!    the comparison, which is the safe direction.
//!
//! All of it is **compounded onto the CREATE**, so neither proof costs a round
//! trip.
//!
//! Gotcha/Why — the identity comes from `QUERY_INFO`, not from the `QFid`
//! create context, even though `QFid` answers exactly this question in one
//! field and would ride along for free. MS-SMB2 § 3.3.5.9.12 lets a server
//! reject a CREATE that carries any context *besides* the durable-reconnect
//! one, and Samba 4.20 does exactly that: `DH2C` + `QFid` comes back
//! `STATUS_OBJECT_NAME_NOT_FOUND` from `smb2_create.c` before the open is even
//! looked up. A proof that only works on one side of the reconnect is no
//! proof, so both sides use the same `QUERY_INFO` instead.
//!
//! Gotcha/Why — `FileInternalInformation` (class 6) and not the tidier
//! `FileIdInformation` (class 59), which returns both halves at once: Samba
//! 4.20 answers class 59 with `STATUS_INVALID_INFO_CLASS`. Class 6 has existed
//! since SMB 2.0 and every server answers it. (Both findings verified
//! 2026-08-02 against the `smb-guest` fixture, Samba 4.20.6, `log level = 10`.)
//!
//! A server that will not answer `FileIdInformation` gets no resume from this
//! crate. That is deliberately strict, and it costs a rare restarted transfer
//! to buy the guarantee that a resumed write never lands in a stranger's file.
//!
//! ## Getting one at all
//!
//! ⚠️ The server only grants a durable handle when the CREATE also asks for a
//! batch oplock or a handle-caching lease (MS-SMB2 § 3.3.5.9.10). This module
//! asks for a batch oplock, which has a cost worth knowing about: while we
//! hold one, another client opening the same file makes the server send us an
//! oplock break and wait for our acknowledgment before letting them proceed.
//! That is why durable opens are a separate, opt-in method rather than the
//! default for every write.
//!
//! Everything here degrades quietly. A server on SMB 2.1, a server with
//! durable handles switched off, a share that declines the oplock: all of them
//! produce an ordinary working handle with no durability, never an error.

use log::{debug, error, info, warn};

use crate::client::connection::{CompoundOp, Connection, Frame};
use crate::error::{DurableLoss, Error, Result};
use crate::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use crate::msg::create_context::{self, DurableGrant, DurableReconnectV2, DurableRequestV2};
use crate::msg::query_info::{InfoType, QueryInfoRequest, QueryInfoResponse};
use crate::pack::{Guid, ReadCursor, Unpack};
use crate::types::flags::{FileAccessMask, FileAttributes};
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, Dialect, FileId, OplockLevel};

use super::tree::Tree;

/// `FILE_NON_DIRECTORY_FILE` (MS-SMB2 § 2.2.13).
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
/// `FileInternalInformation` (MS-FSCC § 2.4.22): the file's index number.
const FILE_INTERNAL_INFORMATION: u8 = 6;
/// `FileFsVolumeInformation` (MS-FSCC § 2.5.9): which volume it lives on.
const FS_VOLUME_INFORMATION: u8 = 1;
/// Offset of `VolumeSerialNumber` inside `FileFsVolumeInformation`, past the
/// 8-byte `VolumeCreationTime`.
const FS_VOLUME_SERIAL_OFFSET: usize = 8;

/// Which file, exactly, a handle points at.
///
/// Stable across opens of the same file and different for any other file on
/// the server, which is what makes it usable as the client-side half of
/// proving a reclaimed handle has not moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileIdentity {
    /// The serial number of the volume the file lives on
    /// (`FileFsVolumeInformation`), or `0` on a server that will not say.
    pub volume_serial: u32,
    /// The file's index number within that volume
    /// (`FileInternalInformation`) — its inode, on a POSIX server.
    pub index_number: u64,
}

impl FileIdentity {
    /// The `QUERY_INFO` that reads the index number, for compounding onto a
    /// CREATE. `FileId::SENTINEL` makes the server substitute the handle the
    /// CREATE ahead of it just produced.
    pub(super) fn index_query() -> QueryInfoRequest {
        QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: FILE_INTERNAL_INFORMATION,
            output_buffer_length: 8,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        }
    }

    /// The `QUERY_INFO` that reads the volume serial.
    pub(super) fn volume_query() -> QueryInfoRequest {
        QueryInfoRequest {
            info_type: InfoType::Filesystem,
            file_info_class: FS_VOLUME_INFORMATION,
            // Generous: the label follows the serial and its length varies.
            output_buffer_length: 128,
            additional_information: 0,
            flags: 0,
            file_id: FileId::SENTINEL,
            input_buffer: vec![],
        }
    }

    /// Whether both halves were answered, which is what a resume across
    /// sessions needs before it may call two files the same one (see
    /// [`WriteCheckpoint`](crate::client::stream::WriteCheckpoint)). A `0` in
    /// either half is a server that answered without saying: `from_frames`
    /// reports an unanswered volume query as `0`, and an index number of `0`
    /// is no file in particular.
    ///
    /// Stricter than a durable reclaim, which accepts a missing volume serial
    /// because its `CreateGuid` is a second proof that the server matched our
    /// own open. A resume has nothing else, and a share can span mount
    /// points whose inode numbers overlap.
    pub(crate) fn is_proof(&self) -> bool {
        self.volume_serial != 0 && self.index_number != 0
    }

    fn payload(frame: &Frame) -> Option<Vec<u8>> {
        if frame.header.status != NtStatus::SUCCESS {
            return None;
        }
        Some(
            QueryInfoResponse::unpack(&mut ReadCursor::new(&frame.body))
                .ok()?
                .output_buffer,
        )
    }

    /// Assemble one from the two compounded answers. `None` when the index
    /// number — the part that actually discriminates — is missing.
    pub(super) fn from_frames(index: Option<&Frame>, volume: Option<&Frame>) -> Option<Self> {
        let index_number = {
            let body = Self::payload(index?)?;
            ReadCursor::new(&body).read_u64_le().ok()?
        };
        let volume_serial = volume
            .and_then(Self::payload)
            .and_then(|body| {
                let slice = body.get(FS_VOLUME_SERIAL_OFFSET..FS_VOLUME_SERIAL_OFFSET + 4)?;
                Some(u32::from_le_bytes(slice.try_into().ok()?))
            })
            .unwrap_or(0);
        Some(Self {
            volume_serial,
            index_number,
        })
    }
}

/// A write handle the server has promised to keep alive across a disconnect,
/// plus everything needed to prove a reclaimed one is the same file.
///
/// Obtained from [`Tree::open_file_durable`] and spent by
/// [`Tree::reclaim_durable_handle`]. Cheap to copy and safe to hold across a
/// reconnect — that is its whole job — but ❌ never construct one by hand or
/// copy the `create_guid` between files: it is the token the server matches
/// on, and reusing it is how a reclaim finds the wrong open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableHandle {
    /// The handle as the current session knows it. Replaced on every
    /// successful reclaim.
    pub file_id: FileId,
    /// What the server promised.
    pub grant: DurableGrant,
    /// Which file this was, at open. The client-side half of the proof.
    identity: FileIdentity,
    /// Our proof of ownership, echoed back in the reconnect context.
    create_guid: Guid,
    /// The connection generation this handle was last valid on.
    generation: u64,
}

impl DurableHandle {
    /// Which file the server said this handle points at.
    pub fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// Whether this handle belongs to the session `conn` currently has.
    ///
    /// `false` means a reconnect has happened since the handle was obtained,
    /// so it must be reclaimed before use.
    pub fn is_current(&self, conn: &Connection) -> bool {
        self.generation == conn.generation()
    }
}

/// What an open produced: the handle, its size, and durability if the server
/// granted any.
#[derive(Debug, Clone, Copy)]
pub struct DurableOpen {
    /// The handle to write through.
    pub file_id: FileId,
    /// The file's size at open.
    pub size: u64,
    /// `Some` when the server promised to keep this open across a disconnect.
    /// `None` is normal and not an error: an SMB 2.1 server, a server with
    /// durable handles off, or one that declined the batch oplock all land
    /// here, and the handle works fine — it just cannot be resumed.
    pub durable: Option<DurableHandle>,
}

impl Tree {
    /// Open (or create) a file for writing, asking for a handle that survives
    /// a disconnect.
    ///
    /// Same as [`open_file_readwrite`](Tree::open_file_readwrite) plus a batch
    /// oplock, the durable-handle create context, and a compounded
    /// `QUERY_INFO` that records which file this is. Read the module docs
    /// before using it: the batch oplock is not free for other clients, and a
    /// `None` in [`DurableOpen::durable`] is the normal outcome on servers that
    /// do not support this.
    ///
    /// Opened non-truncating (`FileOpenIf`), because the point of resuming is
    /// to keep the bytes already written.
    pub async fn open_file_durable(
        &self,
        conn: &mut Connection,
        path: &str,
    ) -> Result<DurableOpen> {
        let conn = &mut self.pinned_conn(conn);
        let create_guid = crate::client::connection::random_guid();
        // A server below SMB 3.0 has only the v1 contexts, whose reclaim this
        // crate refuses to perform. Asking anyway would buy a batch oplock and
        // its costs in exchange for nothing.
        let durable_possible = conn.params().is_some_and(|p| p.dialect >= Dialect::Smb3_0);

        let contexts = if durable_possible {
            create_context::pack_contexts(&[DurableRequestV2 {
                // The server knows its own resource limits and clamps whatever
                // we ask for, so naming a window only invites a smaller one.
                timeout_ms: 0,
                persistent: false,
                create_guid,
            }
            .context()])
        } else {
            Vec::new()
        };

        let create_req = CreateRequest {
            requested_oplock_level: if durable_possible {
                // The price of durability: without a batch oplock (or a
                // handle-caching lease) the server ignores the DH2Q context
                // entirely (MS-SMB2 § 3.3.5.9.10).
                OplockLevel::Batch
            } else {
                OplockLevel::None
            },
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(ShareAccess::FILE_SHARE_READ | ShareAccess::FILE_SHARE_WRITE),
            create_disposition: CreateDisposition::FileOpenIf,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: self.format_path(path),
            create_contexts: contexts,
        };
        let (create, identity) = self.create_and_identify(conn, &create_req).await?;
        let resp = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?;
        let answered = create_context::parse_contexts(&resp.create_contexts)?;
        let grant = create_context::find(&answered, create_context::NAME_DH2Q)
            .map(|c| DurableGrant::from_bytes(&c.data))
            .transpose()?;

        // Both proofs or nothing. A handle we could not later prove is the
        // same file must not be presented as resumable: the caller would build
        // a resume on it and we would have to refuse at the worst possible
        // moment, when the honest answer was available right here.
        let durable = match (grant, identity) {
            (Some(grant), Some(identity)) => {
                debug!(
                    "durable: opened with a durable handle, server holds it {} ms{} path={:?}",
                    grant.timeout_ms,
                    if grant.persistent {
                        " (persistent)"
                    } else {
                        ""
                    },
                    path
                );
                // So an oplock break, which arrives with no usable tree id of
                // its own, can be answered on the right tree.
                conn.register_oplock(resp.file_id, self.tree_id);
                Some(DurableHandle {
                    file_id: resp.file_id,
                    grant,
                    identity,
                    create_guid,
                    generation: conn.generation(),
                })
            }
            (Some(_), None) => {
                warn!(
                    "durable: got a durable handle but the server would not say which file \
                     it is, so a reclaim could never be proven to be the same one; treating \
                     the handle as non-resumable path={path:?}"
                );
                None
            }
            (None, _) => {
                debug!(
                    "durable: opened without durability (server declined or does not \
                     support it); an interrupted write will restart path={path:?}"
                );
                None
            }
        };

        Ok(DurableOpen {
            file_id: resp.file_id,
            size: resp.end_of_file,
            durable,
        })
    }

    /// Claim a durable open back after a reconnect, or fail rather than guess.
    ///
    /// `conn` must be a live, authenticated connection to the same server, and
    /// `self` a tree connect to the same share, under the same credentials —
    /// that is what the server checks before it will even look at the handle.
    /// `path` must be the path the handle was opened at.
    ///
    /// On success the returned handle carries the new `FileId` and the caller
    /// can resume writing at whatever offset it had reached. On failure
    /// **nothing is left open on the server**: a handle that came back
    /// unprovable is closed before the error is returned.
    ///
    /// Errors are [`Error::DurableHandleLost`], whose [`DurableLoss`] says
    /// which guarantee did not hold. All of them mean the same thing to a
    /// caller: reopen and rewrite the file from the start.
    pub async fn reclaim_durable_handle(
        &self,
        conn: &mut Connection,
        handle: &DurableHandle,
        path: &str,
    ) -> Result<DurableHandle> {
        let conn = &mut self.pinned_conn(conn);
        let lost = |reason| Error::DurableHandleLost {
            path: path.to_string(),
            reason,
        };

        let create_req = CreateRequest {
            requested_oplock_level: OplockLevel::Batch,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::FILE_READ_DATA
                    | FileAccessMask::FILE_WRITE_DATA
                    | FileAccessMask::FILE_READ_ATTRIBUTES
                    | FileAccessMask::FILE_WRITE_ATTRIBUTES
                    | FileAccessMask::SYNCHRONIZE,
            ),
            file_attributes: FileAttributes::NORMAL,
            share_access: ShareAccess(ShareAccess::FILE_SHARE_READ | ShareAccess::FILE_SHARE_WRITE),
            create_disposition: CreateDisposition::FileOpen,
            create_options: FILE_NON_DIRECTORY_FILE,
            name: self.format_path(path),
            // ❌ The reconnect context must travel ALONE. A server may reject a
            // reclaim carrying any other create context (MS-SMB2 § 3.3.5.9.12)
            // and Samba does; the identity check rides on the compounded
            // QUERY_INFO instead. See the module docs.
            create_contexts: create_context::pack_contexts(&[DurableReconnectV2 {
                file_id: handle.file_id,
                create_guid: handle.create_guid,
                persistent: handle.grant.persistent,
            }
            .context()]),
        };
        let (create, identity) = match self.create_and_identify(conn, &create_req).await {
            Ok(pair) => pair,
            Err(Error::Protocol { status, .. }) => {
                debug!(
                    "durable: the server would not give the handle back ({status}); the \
                     open expired or the server restarted path={path:?}"
                );
                return Err(lost(DurableLoss::Expired));
            }
            Err(e) => return Err(e),
        };
        let resp = CreateResponse::unpack(&mut ReadCursor::new(&create.body))?;

        let Some(identity) = identity else {
            // It answered the reclaim but not the question. We hold a handle we
            // cannot vouch for, so put it back.
            warn!(
                "durable: the handle came back but the server would not say which file \
                 it is, so nothing about it can be proven; closing it and starting over \
                 path={path:?}"
            );
            let _ = self.close_handle(conn, resp.file_id).await;
            return Err(lost(DurableLoss::IdentityUnavailable));
        };

        if identity != handle.identity {
            // The one outcome this whole module exists to make impossible.
            // Loud, because a server reaching this is doing something the
            // protocol says it must not.
            error!(
                "durable: REFUSING a reclaimed handle -- the server matched our \
                 CreateGuid but handed back a different file (asked for volume {:#x} \
                 file {:#x}, got volume {:#x} file {:#x}). Closing it; the transfer \
                 restarts rather than writing into the wrong file. path={path:?}",
                handle.identity.volume_serial,
                handle.identity.index_number,
                identity.volume_serial,
                identity.index_number,
            );
            let _ = self.close_handle(conn, resp.file_id).await;
            return Err(lost(DurableLoss::IdentityMismatch));
        }

        info!(
            "durable: reclaimed on the new session; the transfer resumes rather than \
             restarting path={path:?}"
        );
        conn.register_oplock(resp.file_id, self.tree_id);
        Ok(DurableHandle {
            file_id: resp.file_id,
            generation: conn.generation(),
            ..*handle
        })
    }

    /// One round trip: open the file and ask which file it is.
    ///
    /// Either `QUERY_INFO` is allowed to fail — a server that will not answer
    /// them still opened the file, it just cannot offer a resume — so their
    /// failure comes back as `None` rather than sinking the CREATE.
    async fn create_and_identify(
        &self,
        conn: &mut Connection,
        create_req: &CreateRequest,
    ) -> Result<(Frame, Option<FileIdentity>)> {
        let conn = &mut self.pinned_conn(conn);
        let index_req = FileIdentity::index_query();
        let volume_req = FileIdentity::volume_query();
        let results = conn
            .execute_compound(&[
                CompoundOp {
                    command: Command::Create,
                    body: create_req,
                    tree_id: Some(self.tree_id),
                    credit_charge: CreditCharge(1),
                },
                CompoundOp {
                    command: Command::QueryInfo,
                    body: &index_req,
                    tree_id: Some(self.tree_id),
                    credit_charge: CreditCharge(1),
                },
                CompoundOp {
                    command: Command::QueryInfo,
                    body: &volume_req,
                    tree_id: Some(self.tree_id),
                    credit_charge: CreditCharge(1),
                },
            ])
            .await?;
        let frames: Vec<Option<Frame>> = results.into_iter().map(|r| r.ok()).collect();
        let create = frames
            .first()
            .and_then(|f| f.as_ref())
            .ok_or_else(|| Error::invalid_data("compound CREATE produced no response"))?;
        if create.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: create.header.status,
                command: Command::Create,
            });
        }
        let identity = FileIdentity::from_frames(
            frames.get(1).and_then(|f| f.as_ref()),
            frames.get(2).and_then(|f| f.as_ref()),
        );
        let create = frames.into_iter().next().flatten().expect("checked above");
        Ok((create, identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_helpers::{
        build_close_response, build_compound_response_frame, build_create_error_response,
        build_create_response_with_contexts, build_query_info_error_response,
        build_query_info_response, setup_connection,
    };
    use crate::msg::create_context::CreateContext;
    use crate::transport::MockTransport;
    use crate::types::TreeId;
    use std::sync::Arc;

    const OURS: FileIdentity = FileIdentity {
        volume_serial: 0xABCD,
        index_number: 0x1234_5678,
    };
    /// Same volume, different inode: the shape a recycled `FileId` would take.
    const SOMEONE_ELSES: FileIdentity = FileIdentity {
        volume_serial: 0xABCD,
        index_number: 0x9999_9999,
    };
    /// Same inode, different volume: what a share spanning two mount points
    /// can produce, and the reason the volume is part of the identity at all.
    const ANOTHER_VOLUME: FileIdentity = FileIdentity {
        volume_serial: 0x1234,
        index_number: 0x1234_5678,
    };

    pub(super) fn a_share() -> Tree {
        Tree {
            pin: None,
            tree_id: TreeId(20),
            share_name: "test".to_string(),
            server: "test-server".to_string(),
            is_dfs: false,
            encrypt_data: false,
            dfs_origin: None,
        }
    }

    fn smb3(conn: &mut Connection) {
        let mut params = conn.params().unwrap();
        params.dialect = Dialect::Smb3_1_1;
        conn.set_test_params(params);
    }

    /// `FileInternalInformation`: the index number, and nothing else.
    fn index_body(id: FileIdentity) -> Vec<u8> {
        id.index_number.to_le_bytes().to_vec()
    }

    /// `FileFsVolumeInformation`: creation time, serial, then a label.
    fn volume_body(id: FileIdentity) -> Vec<u8> {
        let mut out = vec![0u8; FS_VOLUME_SERIAL_OFFSET];
        out.extend_from_slice(&id.volume_serial.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // VolumeLabelLength
        out.extend_from_slice(&[0, 0]); // SupportsObjects + Reserved
        out
    }

    fn granted(timeout_ms: u32) -> CreateContext {
        let mut body = Vec::new();
        body.extend_from_slice(&timeout_ms.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        CreateContext::new(create_context::NAME_DH2Q, body)
    }

    fn a_file_id(n: u64) -> FileId {
        FileId {
            persistent: n,
            volatile: n,
        }
    }

    /// The compounded CREATE + QUERY_INFO ×2 answer both paths expect.
    fn answer(
        file_id: FileId,
        contexts: &[CreateContext],
        identity: Option<FileIdentity>,
    ) -> Vec<u8> {
        answer_with(file_id, contexts, identity, true)
    }

    /// Same, but `with_volume` false models a server that answers the index
    /// number and declines `FileFsVolumeInformation`.
    fn answer_with(
        file_id: FileId,
        contexts: &[CreateContext],
        identity: Option<FileIdentity>,
        with_volume: bool,
    ) -> Vec<u8> {
        let (index, volume) = match identity {
            Some(id) => (
                build_query_info_response(index_body(id)),
                if with_volume {
                    build_query_info_response(volume_body(id))
                } else {
                    build_query_info_error_response(NtStatus::NOT_SUPPORTED)
                },
            ),
            None => (
                build_query_info_error_response(NtStatus::NOT_SUPPORTED),
                build_query_info_error_response(NtStatus::NOT_SUPPORTED),
            ),
        };
        build_compound_response_frame(&[
            build_create_response_with_contexts(file_id, 0, contexts),
            index,
            volume,
        ])
    }

    /// The contexts the client actually put on the wire for the CREATE in
    /// request `n`.
    fn sent_contexts(mock: &MockTransport, n: usize) -> Vec<CreateContext> {
        let sent = mock.sent_message(n).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = crate::msg::header::Header::unpack(&mut cursor).unwrap();
        let req = CreateRequest::unpack(&mut cursor).unwrap();
        create_context::parse_contexts(&req.create_contexts).unwrap()
    }

    fn sent_oplock(mock: &MockTransport, n: usize) -> OplockLevel {
        let sent = mock.sent_message(n).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = crate::msg::header::Header::unpack(&mut cursor).unwrap();
        CreateRequest::unpack(&mut cursor)
            .unwrap()
            .requested_oplock_level
    }

    /// The compounded answer to a durable open of `file_id`, granted with an
    /// identity the reclaim can be checked against.
    pub(super) fn durable_answer(file_id: FileId) -> Vec<u8> {
        answer(file_id, &[granted(180_000)], Some(OURS))
    }

    async fn open_durably(mock: &Arc<MockTransport>, conn: &mut Connection) -> DurableOpen {
        mock.queue_response(answer(a_file_id(1), &[granted(180_000)], Some(OURS)));
        a_share()
            .open_file_durable(conn, "big.iso")
            .await
            .expect("the open must succeed")
    }

    #[tokio::test]
    async fn an_open_the_server_backs_with_both_proofs_is_resumable() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);

        let open = open_durably(&mock, &mut conn).await;

        let durable = open.durable.expect("both proofs were answered");
        assert_eq!(durable.file_id, a_file_id(1));
        assert_eq!(durable.grant.timeout_ms, 180_000);
        assert_eq!(durable.identity(), OURS);
        assert!(durable.is_current(&conn));

        assert_eq!(
            sent_oplock(&mock, 0),
            OplockLevel::Batch,
            "without a batch oplock the server ignores the durable request \
             entirely (MS-SMB2 3.3.5.9.10)"
        );
        assert!(
            create_context::find(&sent_contexts(&mock, 0), create_context::NAME_DH2Q).is_some()
        );
    }

    /// A server that will not do durable handles is normal, not an error. The
    /// caller gets a working handle that simply cannot be resumed.
    #[tokio::test]
    async fn an_open_the_server_declines_durability_on_still_returns_a_working_handle() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        mock.queue_response(answer(a_file_id(1), &[], Some(OURS)));

        let open = a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap();

        assert_eq!(open.file_id, a_file_id(1));
        assert!(
            open.durable.is_none(),
            "no grant means no resume, and that must not be an error"
        );
    }

    /// A grant with no way to prove identity is not offered as resumable.
    ///
    /// Offering it would push the failure to the worst possible moment: the
    /// caller builds a resume on the handle and we refuse mid-transfer, when
    /// the honest answer was available at open time.
    #[tokio::test]
    async fn a_grant_the_server_will_not_back_with_an_identity_is_not_resumable() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        mock.queue_response(answer(a_file_id(1), &[granted(180_000)], None));

        let open = a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap();
        assert!(open.durable.is_none());
    }

    /// SMB 2.1 has only the v1 contexts, whose reclaim carries no client-chosen
    /// proof. We don't ask, so we don't take the batch oplock's costs for
    /// something we would refuse to use.
    #[tokio::test]
    async fn a_pre_smb3_server_is_never_asked_for_a_durable_handle() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock); // negotiates SMB 2.0.2
        mock.queue_response(answer(a_file_id(1), &[], Some(OURS)));

        let open = a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap();

        assert!(open.durable.is_none());
        assert_eq!(sent_oplock(&mock, 0), OplockLevel::None);
        assert!(
            create_context::find(&sent_contexts(&mock, 0), create_context::NAME_DH2Q).is_none()
        );
    }

    // ── Reclaiming ────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_reclaim_of_the_same_file_succeeds_and_carries_the_new_handle() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        // The server gives it back under a new FileId, same file.
        mock.queue_response(answer(a_file_id(2), &[], Some(OURS)));
        let reclaimed = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await
            .expect("same file, both proofs held");

        assert_eq!(
            reclaimed.file_id,
            a_file_id(2),
            "the caller must write through the new handle, not the dead one"
        );
        assert_eq!(reclaimed.identity(), OURS);
    }

    /// The reconnect context has to carry the guid from the *original* open —
    /// it is the token the server matches on — and it has to travel ALONE.
    ///
    /// MS-SMB2 § 3.3.5.9.12 lets a server reject a reclaim carrying any other
    /// create context, and Samba 4.20 does: `DH2C` + `QFid` comes back
    /// `STATUS_OBJECT_NAME_NOT_FOUND` before the open is even looked up. A
    /// proof that only works on one side of the reconnect is no proof, which
    /// is why the identity check rides on a compounded QUERY_INFO instead.
    #[tokio::test]
    async fn the_reconnect_context_replays_the_create_guid_and_travels_alone() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        let opened = sent_contexts(&mock, 0);
        let dh2q = create_context::find(&opened, create_context::NAME_DH2Q).unwrap();
        let guid_at_open = dh2q.data[16..32].to_vec();

        mock.queue_response(answer(a_file_id(2), &[], Some(OURS)));
        a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await
            .unwrap();

        let reclaimed = sent_contexts(&mock, 1);
        assert_eq!(
            reclaimed.len(),
            1,
            "the reconnect context must be the only one, or servers reject the \
             reclaim outright: {reclaimed:?}"
        );
        let dh2c = create_context::find(&reclaimed, create_context::NAME_DH2C)
            .expect("the reclaim must carry a DH2C context");
        assert_eq!(
            dh2c.data[16..32],
            guid_at_open[..],
            "the guid the server stored with the open is what proves the \
             handle is ours"
        );
        assert_eq!(
            dh2c.data[0..8],
            durable.file_id.persistent.to_le_bytes()[..],
            "and the old FileId is what it looks up"
        );
    }

    /// The one this whole module exists for.
    ///
    /// The server matched the `CreateGuid` and still handed back a different
    /// file. Writing through that handle would put the user's bytes in a
    /// stranger's file, which is far worse than a failed transfer, so the
    /// handle is closed and the reclaim refused.
    #[tokio::test]
    async fn a_reclaim_that_comes_back_as_a_different_file_is_refused_and_closed() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        mock.queue_response(answer(a_file_id(2), &[], Some(SOMEONE_ELSES)));
        mock.queue_response(build_close_response());

        let outcome = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await;

        assert!(
            matches!(
                outcome,
                Err(Error::DurableHandleLost {
                    reason: DurableLoss::IdentityMismatch,
                    ..
                })
            ),
            "expected a refusal naming the mismatch, got {outcome:?}"
        );
        assert_eq!(
            mock.sent_count(),
            3,
            "the handle we refused must be closed, not leaked on the server: \
             open, reclaim, close"
        );
        let closed = mock.sent_message(2).unwrap();
        let mut cursor = ReadCursor::new(&closed);
        let header = crate::msg::header::Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::Close);
    }

    /// A reclaim the server will not vouch for is refused too. "It gave the
    /// handle back" is not one of the two proofs.
    #[tokio::test]
    async fn a_reclaim_with_no_identity_is_refused_and_closed() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        mock.queue_response(answer(a_file_id(2), &[], None));
        mock.queue_response(build_close_response());

        let outcome = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await;

        assert!(
            matches!(
                outcome,
                Err(Error::DurableHandleLost {
                    reason: DurableLoss::IdentityUnavailable,
                    ..
                })
            ),
            "got {outcome:?}"
        );
        assert_eq!(mock.sent_count(), 3, "the unprovable handle was closed");
    }

    /// The routine failure: the open timed out, or the server restarted. A
    /// durable handle survives a dead connection, not a dead server.
    #[tokio::test]
    async fn a_reclaim_the_server_rejects_reports_the_open_as_expired() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        mock.queue_response(build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_query_info_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_query_info_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ]));

        let outcome = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await;

        assert!(
            matches!(
                outcome,
                Err(Error::DurableHandleLost {
                    reason: DurableLoss::Expired,
                    ..
                })
            ),
            "got {outcome:?}"
        );
        assert_eq!(
            mock.sent_count(),
            2,
            "nothing was opened, so there is nothing to close"
        );
    }

    /// A handle knows which session it belongs to, so a caller can tell
    /// "reconnect happened, reclaim first" from "still fine, keep writing"
    /// without tracking it separately.
    #[tokio::test]
    async fn a_handle_knows_it_has_outlived_its_session() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        assert!(durable.is_current(&conn), "generation 0 is what conn is on");
        let after_a_reconnect = DurableHandle {
            generation: 1,
            ..durable
        };
        assert!(!after_a_reconnect.is_current(&conn));
    }

    /// A share can span mount points, so two different files can share an
    /// index number. The volume is what tells them apart.
    #[tokio::test]
    async fn a_reclaim_on_a_different_volume_is_refused_even_at_the_same_index() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();

        mock.queue_response(answer(a_file_id(2), &[], Some(ANOTHER_VOLUME)));
        mock.queue_response(build_close_response());

        let outcome = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await;
        assert!(
            matches!(
                outcome,
                Err(Error::DurableHandleLost {
                    reason: DurableLoss::IdentityMismatch,
                    ..
                })
            ),
            "got {outcome:?}"
        );
    }

    /// A server that answers the index number but not the volume still gets a
    /// resume: the comparison degrades to the index alone, which is never
    /// weaker than having no proof, and is symmetric so it cannot produce a
    /// false match.
    #[tokio::test]
    async fn a_server_that_will_not_name_the_volume_still_gets_a_resume() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        mock.queue_response(answer_with(
            a_file_id(1),
            &[granted(180_000)],
            Some(OURS),
            false,
        ));
        let durable = a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap()
            .durable
            .expect("the index number alone is enough to identify the file");
        assert_eq!(durable.identity().volume_serial, 0, "not answered");

        mock.queue_response(answer_with(a_file_id(2), &[], Some(OURS), false));
        a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await
            .expect("same index number, same file");
    }

    /// A server that named the volume at open and won't at reclaim fails the
    /// comparison, which is the safe direction.
    #[tokio::test]
    async fn a_volume_that_goes_missing_between_open_and_reclaim_is_refused() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        smb3(&mut conn);
        let durable = open_durably(&mock, &mut conn).await.durable.unwrap();
        assert_eq!(durable.identity().volume_serial, OURS.volume_serial);

        mock.queue_response(answer_with(a_file_id(2), &[], Some(OURS), false));
        mock.queue_response(build_close_response());

        let outcome = a_share()
            .reclaim_durable_handle(&mut conn, &durable, "big.iso")
            .await;
        assert!(
            matches!(
                outcome,
                Err(Error::DurableHandleLost {
                    reason: DurableLoss::IdentityMismatch,
                    ..
                })
            ),
            "got {outcome:?}"
        );
    }
}

/// The batch oplock's bill, and who pays it.
#[cfg(test)]
mod oplock_break_tests {
    use super::tests::*;
    use super::*;
    use crate::client::connection::{pack_message, NegotiatedParams};
    use crate::msg::header::Header;
    use crate::msg::oplock_break::OplockBreak;
    use crate::pack::Unpack;
    use crate::transport::MockTransport;
    use crate::types::flags::Capabilities;
    use crate::types::{MessageId, SessionId, TreeId};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const THE_TREE: TreeId = TreeId(20);

    /// A connection whose responses carry real message ids, so an unsolicited
    /// frame can be delivered without the mock's auto-rewrite pairing it
    /// against a send that never happened.
    fn plain_connection(mock: &Arc<MockTransport>) -> Connection {
        let mut conn =
            Connection::from_transport(Box::new(mock.clone()), Box::new(mock.clone()), "test");
        conn.set_test_params(NegotiatedParams {
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
        conn.set_session_id(SessionId(1));
        conn.set_credits(512);
        conn
    }

    /// Stamp consecutive message ids into a canned compound frame.
    ///
    /// The `build_*_response` helpers all hardcode message id 0, which the
    /// mock's auto-rewrite normally patches. This connection deliberately runs
    /// without it (see [`plain_connection`]), so the ids are stamped here
    /// instead — a 3-op compound occupies ids `first..first+3`.
    fn with_msg_ids(mut frame: Vec<u8>, first: u64) -> Vec<u8> {
        let mut offset = 0usize;
        let mut id = first;
        loop {
            frame[offset + 24..offset + 32].copy_from_slice(&id.to_le_bytes());
            let next =
                u32::from_le_bytes(frame[offset + 20..offset + 24].try_into().unwrap()) as usize;
            if next == 0 {
                return frame;
            }
            offset += next;
            id += 1;
        }
    }

    fn a_break(file_id: FileId) -> Vec<u8> {
        let mut h = Header::new_request(Command::OplockBreak);
        h.flags.set_response();
        h.message_id = MessageId::UNSOLICITED;
        h.credits = 1;
        pack_message(
            &h,
            &OplockBreak {
                oplock_level: OplockLevel::LevelII,
                file_id,
            },
        )
    }

    async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Holding a batch oplock costs *other* clients: until we answer a break,
    /// whoever tried to open the file waits out the server's break timeout,
    /// which is around 35 s on both Samba and Windows. So we answer.
    #[tokio::test]
    async fn an_oplock_break_is_acknowledged_so_the_other_client_is_not_left_waiting() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = plain_connection(&mock);
        let handle_id = FileId {
            persistent: 5,
            volatile: 5,
        };
        mock.queue_response(with_msg_ids(durable_answer(handle_id), 0));
        let open = a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap();
        assert!(open.durable.is_some());
        let sent_after_open = mock.sent_count();

        mock.queue_response(a_break(handle_id));
        wait_for("the acknowledgment to be sent", || {
            mock.sent_count() > sent_after_open
        })
        .await;

        let sent = mock.sent_message(sent_after_open).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let header = Header::unpack(&mut cursor).unwrap();
        assert_eq!(header.command, Command::OplockBreak);
        assert_eq!(
            header.tree_id,
            Some(THE_TREE),
            "the acknowledgment has to name the tree the open belongs to \
             (MS-SMB2 2.2.24.1); on the wrong one the server rejects it and the \
             other client waits anyway"
        );
        let ack = OplockBreak::unpack(&mut cursor).unwrap();
        assert_eq!(ack.file_id, handle_id);
        assert_eq!(
            ack.oplock_level,
            OplockLevel::None,
            "we only ever took the oplock to get durability, and durability is \
             already gone by the time a break arrives"
        );
    }

    /// A break for a handle we hold no oplock on is not ours to answer, and
    /// guessing a tree id would produce an acknowledgment the server rejects.
    #[tokio::test]
    async fn a_break_for_a_handle_we_never_oplocked_is_left_alone() {
        let mock = Arc::new(MockTransport::new());
        let conn = plain_connection(&mock);

        mock.queue_response(a_break(FileId {
            persistent: 999,
            volatile: 999,
        }));
        // Long enough that an acknowledgment would certainly have been sent.
        tokio::time::sleep(Duration::from_millis(150)).await;

        assert_eq!(mock.sent_count(), 0, "nothing should have been sent");
        assert_eq!(conn.metrics().unsolicited_notifications_received, 1);
    }

    /// Closing a handle retires its oplock bookkeeping, so a break arriving
    /// afterwards is not acknowledged on a handle that is already gone.
    #[tokio::test]
    async fn closing_a_handle_retires_its_oplock_bookkeeping() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = plain_connection(&mock);
        let handle_id = FileId {
            persistent: 5,
            volatile: 5,
        };
        mock.queue_response(with_msg_ids(durable_answer(handle_id), 0));
        a_share()
            .open_file_durable(&mut conn, "big.iso")
            .await
            .unwrap();

        // The canned responses all carry message id 0, and this connection has
        // no auto-rewrite, so rewind the sequence for the second exchange.
        conn.set_next_message_id(0);
        mock.queue_response(with_msg_ids(
            crate::client::test_helpers::build_close_response(),
            0,
        ));
        a_share().close_handle(&mut conn, handle_id).await.unwrap();
        let sent_after_close = mock.sent_count();

        mock.queue_response(a_break(handle_id));
        tokio::time::sleep(Duration::from_millis(150)).await;

        assert_eq!(
            mock.sent_count(),
            sent_after_close,
            "a break for a closed handle must not produce an acknowledgment"
        );
    }
}
