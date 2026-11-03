//! Server-side copy: copy byte ranges between two files entirely on the server.
//!
//! When source and destination live on the same SMB share, the server can copy
//! data between them directly — the bytes never travel over the wire. This is
//! the mechanism Windows Explorer uses for same-share copies, and it turns a
//! multi-gigabyte copy into a handful of small control messages. See MS-SMB2
//! sections 2.2.31.1 / 2.2.32.1 (the wire structures live in
//! [`msg::copychunk`](crate::msg::copychunk)).
//!
//! # Two layers
//!
//! - **Convenience** ([`Tree::server_side_copy_file`], [`SmbClient::server_side_copy_file`]):
//!   copy a whole file to another path on the same share in one call. Opens both
//!   handles, batches the copy within the server's limits, and closes up.
//! - **Primitives** ([`Tree::request_resume_key`], [`Tree::copy_chunks`], and the
//!   range-level [`Tree::server_side_copy_range`]): build any server-side copy
//!   flow yourself — copy a sub-range, copy into a specific destination offset,
//!   or reuse a handle you already hold.
//!
//! # Not every server supports it
//!
//! Older Samba builds and some NAS firmware don't implement the copychunk
//! FSCTLs. Both layers surface that as an error classified
//! [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported); branch on it to
//! fall back to a client-side read-then-write copy:
//!
//! ```no_run
//! # async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
//! use smb2::ErrorKind;
//! match client.server_side_copy_file(share, "big.iso", "big-copy.iso").await {
//!     Ok(bytes) => println!("copied {bytes} bytes server-side"),
//!     Err(e) if e.kind() == ErrorKind::Unsupported => {
//!         // Server has no server-side copy — read the file and write it back.
//!         let data = client.read_file(share, "big.iso").await?;
//!         client.write_file(share, "big-copy.iso", &data).await?;
//!     }
//!     Err(e) => return Err(e),
//! }
//! # Ok(())
//! # }
//! ```

use std::fmt;

use log::debug;

use crate::client::connection::Connection;
use crate::client::tree::Tree;
use crate::client::SmbClient;
use crate::error::{Error, Result};
use crate::msg::copychunk::{
    SrvCopychunk, SrvCopychunkCopy, SrvCopychunkResponse, SrvRequestResumeKeyResponse,
    RESUME_KEY_LEN,
};
use crate::msg::ioctl::{
    IoctlRequest, IoctlResponse, FSCTL_SRV_COPYCHUNK, FSCTL_SRV_REQUEST_RESUME_KEY,
    SMB2_0_IOCTL_IS_FSCTL,
};
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::status::NtStatus;
use crate::types::{Command, FileId};

/// An opaque 24-byte key that names an open source file for a server-side copy.
///
/// Obtained from [`Tree::request_resume_key`] against an open source handle, and
/// passed to [`Tree::copy_chunks`] as the copy source. Per MS-SMB2 2.2.32.3 the
/// key is opaque: don't interpret its bytes, just hand it back to the server.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ResumeKey([u8; RESUME_KEY_LEN]);

impl ResumeKey {
    /// Wrap raw resume-key bytes (for example, one you serialized elsewhere).
    #[must_use]
    pub fn from_bytes(bytes: [u8; RESUME_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw 24 opaque bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; RESUME_KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for ResumeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show only a short prefix for correlation; the key is opaque and
        // dumping all 24 bytes is noise.
        write!(
            f,
            "ResumeKey({:02x}{:02x}{:02x}{:02x}..)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

/// One source->destination byte range to copy server-side.
///
/// `length` is a `u32`: a single chunk is capped at the server's per-chunk
/// limit (see [`ServerSideCopyLimits`]), well under 4 GiB. To copy a larger
/// range, use [`Tree::server_side_copy_range`], which splits it for you.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyChunk {
    /// Offset in the source file to copy from.
    pub source_offset: u64,
    /// Offset in the destination file to copy to.
    pub target_offset: u64,
    /// Number of bytes to copy.
    pub length: u32,
}

impl CopyChunk {
    /// A chunk copying `length` bytes from `source_offset` in the source to
    /// `target_offset` in the destination.
    #[must_use]
    pub fn new(source_offset: u64, target_offset: u64, length: u32) -> Self {
        Self {
            source_offset,
            target_offset,
            length,
        }
    }
}

impl From<CopyChunk> for SrvCopychunk {
    fn from(c: CopyChunk) -> Self {
        SrvCopychunk {
            source_offset: c.source_offset,
            target_offset: c.target_offset,
            length: c.length,
        }
    }
}

/// What a single [`Tree::copy_chunks`] request accomplished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyChunkResult {
    /// Number of chunks the server successfully wrote.
    pub chunks_written: u32,
    /// Total bytes written to the destination across all chunks.
    pub total_bytes_written: u64,
}

/// The per-request limits a server enforces on server-side copy.
///
/// A server that receives a copy request exceeding its limits doesn't copy
/// anything; it rejects the request and advertises these values so the client
/// can re-batch and retry (MS-SMB2 3.3.5.15.6.2). The batched convenience
/// methods do that automatically; [`Tree::copy_chunks`] hands the limits back
/// to you via [`CopyChunkOutcome::Rejected`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerSideCopyLimits {
    /// Maximum number of chunks in one request.
    pub max_chunks: u32,
    /// Maximum bytes in a single chunk.
    pub max_chunk_size: u32,
    /// Maximum total bytes across all chunks in one request.
    pub max_data_size: u32,
}

impl ServerSideCopyLimits {
    /// Conservative limits that essentially every server implementing
    /// server-side copy accepts (they match the common Windows/Samba minimums):
    /// 16 chunks, 1 MiB per chunk, 16 MiB per request. The batched convenience
    /// methods start here and shrink to the server's advertised limits only if
    /// a first request is rejected — so the common case is a single round of
    /// correctly-sized requests with no renegotiation.
    pub const CONSERVATIVE: Self = Self {
        max_chunks: 16,
        max_chunk_size: 1024 * 1024,
        max_data_size: 16 * 1024 * 1024,
    };

    /// Clamp advertised limits into a usable shape: at least one chunk, at least
    /// one byte per chunk, and a per-request budget that fits at least one
    /// full-size chunk. Guards against a malformed or degenerate advertisement
    /// that would otherwise stall batching.
    fn sanitized(self) -> Self {
        let max_chunk_size = self.max_chunk_size.max(1);
        Self {
            max_chunks: self.max_chunks.max(1),
            max_chunk_size,
            max_data_size: self.max_data_size.max(max_chunk_size),
        }
    }
}

/// The outcome of one [`Tree::copy_chunks`] request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyChunkOutcome {
    /// The server copied the requested chunks.
    Copied(CopyChunkResult),
    /// The server rejected the request as exceeding its limits and advertised
    /// them (MS-SMB2 3.3.5.15.6.2). Re-batch within `limits` and retry — the
    /// batched convenience methods ([`Tree::server_side_copy_range`] and up) do
    /// this for you.
    Rejected {
        /// The server's per-request limits.
        limits: ServerSideCopyLimits,
    },
}

impl Tree {
    /// Ask the server for an opaque [`ResumeKey`] identifying an open file, for
    /// use as the *source* of a server-side copy
    /// (`FSCTL_SRV_REQUEST_RESUME_KEY`, MS-SMB2 3.2.4.20.2.1).
    ///
    /// `source` must be an open handle on this tree with read access (open it
    /// with [`Tree::open_file`]). The returned key is passed to
    /// [`copy_chunks`](Self::copy_chunks).
    ///
    /// Returns an error classified [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported)
    /// if the server doesn't implement server-side copy.
    pub async fn request_resume_key(
        &self,
        conn: &mut Connection,
        source: FileId,
    ) -> Result<ResumeKey> {
        let conn = &mut self.pinned_conn(conn);
        let req = IoctlRequest {
            ctl_code: FSCTL_SRV_REQUEST_RESUME_KEY,
            file_id: source,
            max_input_response: 0,
            // A resume-key response is 24 bytes of key + 4 of (unused) context
            // length; 32 is what the spec prescribes (MS-SMB2 3.2.4.20.2.1).
            max_output_response: 32,
            flags: SMB2_0_IOCTL_IS_FSCTL,
            input_data: Vec::new(),
        };

        let frame = conn
            .execute(Command::Ioctl, &req, Some(self.tree_id))
            .await?;
        if frame.header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: frame.header.status,
                command: Command::Ioctl,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let ioctl_resp = IoctlResponse::unpack(&mut cursor)?;
        let mut key_cursor = ReadCursor::new(&ioctl_resp.output_data);
        let resume = SrvRequestResumeKeyResponse::unpack(&mut key_cursor)?;
        Ok(ResumeKey(resume.resume_key))
    }

    /// Issue one server-side copy IOCTL (`FSCTL_SRV_COPYCHUNK`) against an open
    /// destination handle: copy `chunks` from the source named by `source_key`
    /// into `dest`, in a single round-trip.
    ///
    /// This is the low-level primitive. It does not batch or respect limits —
    /// pass chunks that fit the server's per-request limits, or handle a
    /// [`CopyChunkOutcome::Rejected`] (the server advertises its limits) by
    /// re-batching. Most callers want [`server_side_copy_range`](Self::server_side_copy_range)
    /// or [`server_side_copy_file`](Self::server_side_copy_file), which do the
    /// batching and renegotiation.
    ///
    /// `dest` must be open with read *and* write access (`FSCTL_SRV_COPYCHUNK`
    /// requires both on the destination, MS-SMB2 3.3.5.15.6) — the convenience
    /// methods open it that way. An empty `chunks` slice copies nothing and
    /// returns `Copied` with zeroed counts.
    ///
    /// Returns an error classified [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported)
    /// if the server doesn't implement server-side copy, or a protocol error
    /// (for example [`ErrorKind::NotFound`](crate::ErrorKind::NotFound) when the
    /// resume key names no open) otherwise.
    pub async fn copy_chunks(
        &self,
        conn: &mut Connection,
        dest: FileId,
        source_key: &ResumeKey,
        chunks: &[CopyChunk],
    ) -> Result<CopyChunkOutcome> {
        let conn = &mut self.pinned_conn(conn);
        let copy = SrvCopychunkCopy {
            source_key: source_key.0,
            chunks: chunks.iter().copied().map(SrvCopychunk::from).collect(),
        };
        let mut w = WriteCursor::new();
        copy.pack(&mut w);
        let input_data = w.into_inner();

        let req = IoctlRequest {
            ctl_code: FSCTL_SRV_COPYCHUNK,
            file_id: dest,
            max_input_response: 0,
            max_output_response: SrvCopychunkResponse::SIZE as u32,
            flags: SMB2_0_IOCTL_IS_FSCTL,
            input_data,
        };

        let frame = conn
            .execute(Command::Ioctl, &req, Some(self.tree_id))
            .await?;
        let status = frame.header.status;

        // The limits-negotiation path: STATUS_INVALID_PARAMETER carrying a
        // 12-byte SRV_COPYCHUNK_RESPONSE means "too big, here are my limits"
        // rather than a hard failure (MS-SMB2 3.2.5.14.3).
        if status == NtStatus::INVALID_PARAMETER {
            if let Ok(resp) = parse_copychunk_response(&frame.body) {
                let limits = ServerSideCopyLimits {
                    max_chunks: resp.chunks_written,
                    max_chunk_size: resp.chunk_bytes_written,
                    max_data_size: resp.total_bytes_written,
                };
                return Ok(CopyChunkOutcome::Rejected { limits });
            }
            // No limits payload: a genuine invalid-parameter failure.
            return Err(Error::Protocol {
                status,
                command: Command::Ioctl,
            });
        }

        if status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status,
                command: Command::Ioctl,
            });
        }

        let resp = parse_copychunk_response(&frame.body)?;
        Ok(CopyChunkOutcome::Copied(CopyChunkResult {
            chunks_written: resp.chunks_written,
            total_bytes_written: u64::from(resp.total_bytes_written),
        }))
    }

    /// Copy a contiguous byte range from a source file to a destination file
    /// entirely on the server, batching into as many `FSCTL_SRV_COPYCHUNK`
    /// requests as the server's limits require. Returns the total bytes copied.
    ///
    /// Both handles must already be open on this tree: `source_key` from
    /// [`request_resume_key`](Self::request_resume_key) on the source, and
    /// `dest` opened with read+write access. `length` bytes are copied from
    /// `source_offset` in the source to `dest_offset` in the destination. A
    /// `length` of 0 copies nothing.
    ///
    /// This is the general building block: copy a file prefix into a temp, copy
    /// a mid-file region, or place data at an arbitrary destination offset. The
    /// whole-file [`server_side_copy_file`](Self::server_side_copy_file) is a
    /// thin wrapper over it.
    ///
    /// The first request uses [`ServerSideCopyLimits::CONSERVATIVE`]; if the
    /// server rejects it and advertises tighter limits, this re-batches within
    /// them transparently and continues.
    pub async fn server_side_copy_range(
        &self,
        conn: &mut Connection,
        dest: FileId,
        source_key: &ResumeKey,
        source_offset: u64,
        dest_offset: u64,
        length: u64,
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        if length == 0 {
            return Ok(0);
        }

        let mut limits = ServerSideCopyLimits::CONSERVATIVE.sanitized();
        let mut copied: u64 = 0;

        while copied < length {
            let remaining = length - copied;
            let base_src = source_offset + copied;
            let base_dst = dest_offset + copied;
            let (chunks, batch_len) = build_batch(base_src, base_dst, remaining, limits);

            // sanitized() guarantees a non-empty batch whenever remaining > 0.
            debug_assert!(!chunks.is_empty() && batch_len > 0);

            match self.copy_chunks(conn, dest, source_key, &chunks).await? {
                CopyChunkOutcome::Copied(result) => {
                    if result.total_bytes_written == 0 {
                        // A successful response that copied nothing would spin
                        // the loop forever; treat it as a protocol violation.
                        return Err(Error::invalid_data(
                            "server-side copy reported success but wrote 0 bytes",
                        ));
                    }
                    copied += result.total_bytes_written.min(batch_len);
                }
                CopyChunkOutcome::Rejected { limits: advertised } => {
                    let sane = advertised.sanitized();
                    if sane == limits {
                        // We already batched within these limits and were still
                        // rejected — the server isn't giving us usable limits.
                        return Err(Error::invalid_data(
                            "server rejected server-side copy without advertising smaller limits",
                        ));
                    }
                    debug!(
                        "copy: server-side copy limits renegotiated to \
                         max_chunks={} max_chunk_size={} max_data_size={}",
                        sane.max_chunks, sane.max_chunk_size, sane.max_data_size
                    );
                    limits = sane;
                    // Retry this position with the tighter limits.
                }
            }
        }

        Ok(copied)
    }

    /// Copy a whole file to another path on the same share entirely on the
    /// server — no file bytes cross the wire.
    ///
    /// Opens `source_path` for reading and `dest_path` for read+write (creating
    /// it, or truncating an existing file), copies every byte with
    /// [`server_side_copy_range`](Self::server_side_copy_range), flushes and
    /// closes both handles, and returns the number of bytes copied.
    ///
    /// Returns an error classified [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported)
    /// if the server doesn't implement server-side copy; fall back to a
    /// read-then-write copy (see the [module docs](crate::client::copy)).
    ///
    /// [`SmbClient::server_side_copy_file`] is the convenience wrapper when you
    /// hold an `&SmbClient`.
    pub async fn server_side_copy_file(
        &self,
        conn: &mut Connection,
        source_path: &str,
        dest_path: &str,
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        // Whole-file copy: truncate the destination and copy the entire source.
        self.copy_paths(conn, source_path, dest_path, true, 0, 0, None)
            .await
    }

    /// Copy a byte range from one file to another on the same share entirely on
    /// the server, placing it at `dest_offset` in the destination.
    ///
    /// Opens `source_path` for reading and `dest_path` for read+write
    /// **without truncating** (existing destination content outside the written
    /// region is preserved), copies `length` bytes from `source_offset` to
    /// `dest_offset` with [`server_side_copy_range`](Self::server_side_copy_range),
    /// flushes and closes both handles, and returns the bytes copied.
    ///
    /// This is the building block for partial-file flows without touching raw
    /// handles. For example, to rewrite the tail of an archive: server-side copy
    /// the retained head into a temp, then append new data with a positioned
    /// writer ([`create_file_writer_at`](Self::create_file_writer_at)):
    ///
    /// ```no_run
    /// # async fn example(share: &std::sync::Arc<smb2::Tree>, mut conn: smb2::client::Connection) -> Result<(), smb2::Error> {
    /// let head_len = 4096;
    /// // Copy the first `head_len` bytes of the archive into a fresh temp file.
    /// share
    ///     .server_side_copy_file_range(&mut conn, "archive.zip", 0, "archive.tmp", 0, head_len)
    ///     .await?;
    /// // Append new entries right after the retained head.
    /// let mut writer = share.create_file_writer_at(conn, "archive.tmp", head_len).await?;
    /// writer.write_chunk(b"...new zip entries...").await?;
    /// writer.finish().await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Returns an error classified [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported)
    /// if the server doesn't implement server-side copy.
    pub async fn server_side_copy_file_range(
        &self,
        conn: &mut Connection,
        source_path: &str,
        source_offset: u64,
        dest_path: &str,
        dest_offset: u64,
        length: u64,
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        self.copy_paths(
            conn,
            source_path,
            dest_path,
            false,
            source_offset,
            dest_offset,
            Some(length),
        )
        .await
    }

    /// Shared open → resume-key → copy → flush → close choreography for the
    /// path-based convenience methods. Opens the source read-only and the
    /// destination read+write (truncating iff `truncate_dest`), copies `length`
    /// bytes (or the whole source when `length` is `None`), and always closes
    /// both handles. Never leaks a handle on an error path.
    async fn copy_paths(
        &self,
        conn: &mut Connection,
        source_path: &str,
        dest_path: &str,
        truncate_dest: bool,
        source_offset: u64,
        dest_offset: u64,
        length: Option<u64>,
    ) -> Result<u64> {
        let conn = &mut self.pinned_conn(conn);
        debug!(
            "copy: server-side copy from={source_path:?} to={dest_path:?} truncate_dest={truncate_dest}"
        );
        // Both handles are used across many round trips, so every request
        // goes out pinned to the session that opened them.
        let conn = &mut conn.pinned_to_session();

        let (src_id, src_size) = self.open_file(conn, source_path).await?;

        let dst_open = if truncate_dest {
            self.open_readwrite_overwrite(conn, dest_path).await
        } else {
            self.open_file_readwrite(conn, dest_path).await
        };
        let dst_id = match dst_open {
            Ok((id, _)) => id,
            Err(e) => {
                let _ = self.close_handle(conn, src_id).await;
                return Err(e);
            }
        };

        let len = length.unwrap_or(src_size);

        // Do the copy, then always close both handles regardless of outcome.
        let result = async {
            let key = self.request_resume_key(conn, src_id).await?;
            let copied = self
                .server_side_copy_range(conn, dst_id, &key, source_offset, dest_offset, len)
                .await?;
            self.flush_handle(conn, dst_id).await?;
            Ok::<u64, Error>(copied)
        }
        .await;

        let _ = self.close_handle(conn, dst_id).await;
        let _ = self.close_handle(conn, src_id).await;
        result
    }
}

impl SmbClient {
    /// Copy a whole file to another path on the same share entirely on the
    /// server. Convenience wrapper over
    /// [`Tree::server_side_copy_file`](crate::Tree::server_side_copy_file) that
    /// routes through the client's connection.
    ///
    /// Returns an error classified [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported)
    /// if the server lacks server-side copy; fall back to read-then-write.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    /// let bytes = client.server_side_copy_file(share, "movie.mkv", "movie-backup.mkv").await?;
    /// println!("copied {bytes} bytes without touching the network");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn server_side_copy_file(
        &mut self,
        tree: &Tree,
        source_path: &str,
        dest_path: &str,
    ) -> Result<u64> {
        let t = tree.clone();
        let conn = self.connections.for_tree(&t)?;
        t.server_side_copy_file(conn, source_path, dest_path).await
    }

    /// Copy a byte range from one file to another on the same share entirely on
    /// the server. Convenience wrapper over
    /// [`Tree::server_side_copy_file_range`](crate::Tree::server_side_copy_file_range).
    ///
    /// Copies `length` bytes from `source_offset` in `source_path` to
    /// `dest_offset` in `dest_path`, without truncating the destination.
    pub async fn server_side_copy_file_range(
        &mut self,
        tree: &Tree,
        source_path: &str,
        source_offset: u64,
        dest_path: &str,
        dest_offset: u64,
        length: u64,
    ) -> Result<u64> {
        let t = tree.clone();
        let conn = self.connections.for_tree(&t)?;
        t.server_side_copy_file_range(
            conn,
            source_path,
            source_offset,
            dest_path,
            dest_offset,
            length,
        )
        .await
    }
}

/// Parse a 12-byte `SRV_COPYCHUNK_RESPONSE` out of an IOCTL response body.
fn parse_copychunk_response(frame_body: &[u8]) -> Result<SrvCopychunkResponse> {
    let mut cursor = ReadCursor::new(frame_body);
    let ioctl_resp = IoctlResponse::unpack(&mut cursor)?;
    let mut out = ReadCursor::new(&ioctl_resp.output_data);
    SrvCopychunkResponse::unpack(&mut out)
}

/// Build one request's worth of chunks covering up to `remaining` bytes from
/// `base_src`/`base_dst`, staying within `limits`. Returns the chunks and their
/// total byte length. Assumes `limits` is sanitized (non-degenerate).
fn build_batch(
    base_src: u64,
    base_dst: u64,
    remaining: u64,
    limits: ServerSideCopyLimits,
) -> (Vec<CopyChunk>, u64) {
    let max_data = u64::from(limits.max_data_size);
    let max_chunk = u64::from(limits.max_chunk_size);
    let mut chunks = Vec::new();
    let mut off: u64 = 0;

    while (chunks.len() as u32) < limits.max_chunks && off < remaining && off < max_data {
        let this = max_chunk.min(remaining - off).min(max_data - off);
        if this == 0 {
            break;
        }
        chunks.push(CopyChunk {
            source_offset: base_src + off,
            target_offset: base_dst + off,
            length: this as u32,
        });
        off += this;
    }

    (chunks, off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_helpers::{
        build_close_response, build_create_response, build_flush_response,
        build_ioctl_error_response, build_ioctl_response, build_ioctl_response_status,
        setup_connection,
    };
    use crate::msg::copychunk::SrvCopychunkResponse;
    use crate::pack::{Pack, WriteCursor};
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

    fn dst_id() -> FileId {
        FileId {
            persistent: 0xDD,
            volatile: 0xEE,
        }
    }

    fn copychunk_response_bytes(resp: SrvCopychunkResponse) -> Vec<u8> {
        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        w.into_inner()
    }

    fn resume_key_response_bytes(key: [u8; RESUME_KEY_LEN]) -> Vec<u8> {
        let mut w = WriteCursor::new();
        SrvRequestResumeKeyResponse { resume_key: key }.pack(&mut w);
        w.into_inner()
    }

    // -- request_resume_key -------------------------------------------

    #[tokio::test]
    async fn request_resume_key_parses_key() {
        let mock = Arc::new(MockTransport::new());
        let key = [0x42u8; RESUME_KEY_LEN];
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_REQUEST_RESUME_KEY,
            resume_key_response_bytes(key),
        ));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let src = FileId {
            persistent: 1,
            volatile: 2,
        };
        let got = tree.request_resume_key(&mut conn, src).await.unwrap();
        assert_eq!(got.as_bytes(), &key);
    }

    #[tokio::test]
    async fn request_resume_key_unsupported_server_classifies_unsupported() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_ioctl_error_response(NtStatus::NOT_SUPPORTED));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let err = tree
            .request_resume_key(
                &mut conn,
                FileId {
                    persistent: 1,
                    volatile: 2,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::Unsupported);
    }

    // -- copy_chunks --------------------------------------------------

    #[tokio::test]
    async fn copy_chunks_success_reports_bytes() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_COPYCHUNK,
            copychunk_response_bytes(SrvCopychunkResponse {
                chunks_written: 2,
                chunk_bytes_written: 0,
                total_bytes_written: 3000,
            }),
        ));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([0; RESUME_KEY_LEN]);
        let chunks = [CopyChunk::new(0, 0, 1000), CopyChunk::new(1000, 1000, 2000)];
        let outcome = tree
            .copy_chunks(&mut conn, dst_id(), &key, &chunks)
            .await
            .unwrap();
        assert_eq!(
            outcome,
            CopyChunkOutcome::Copied(CopyChunkResult {
                chunks_written: 2,
                total_bytes_written: 3000,
            })
        );
    }

    #[tokio::test]
    async fn copy_chunks_invalid_parameter_with_payload_is_rejected_with_limits() {
        let mock = Arc::new(MockTransport::new());
        // STATUS_INVALID_PARAMETER carrying limits: 4 chunks / 64 KiB / 256 KiB.
        mock.queue_response(build_ioctl_response_status(
            FSCTL_SRV_COPYCHUNK,
            NtStatus::INVALID_PARAMETER,
            copychunk_response_bytes(SrvCopychunkResponse {
                chunks_written: 4,
                chunk_bytes_written: 65536,
                total_bytes_written: 262144,
            }),
        ));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([0; RESUME_KEY_LEN]);
        let outcome = tree
            .copy_chunks(&mut conn, dst_id(), &key, &[CopyChunk::new(0, 0, 4096)])
            .await
            .unwrap();
        assert_eq!(
            outcome,
            CopyChunkOutcome::Rejected {
                limits: ServerSideCopyLimits {
                    max_chunks: 4,
                    max_chunk_size: 65536,
                    max_data_size: 262144,
                }
            }
        );
    }

    #[tokio::test]
    async fn copy_chunks_invalid_parameter_without_payload_is_error() {
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_ioctl_error_response(NtStatus::INVALID_PARAMETER));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([0; RESUME_KEY_LEN]);
        let err = tree
            .copy_chunks(&mut conn, dst_id(), &key, &[CopyChunk::new(0, 0, 4096)])
            .await
            .unwrap_err();
        assert_eq!(err.status(), Some(NtStatus::INVALID_PARAMETER));
    }

    // -- server_side_copy_range: batching + negotiation ---------------

    #[tokio::test]
    async fn copy_range_batches_across_chunk_and_request_limits() {
        // Conservative limits: 16 chunks x 1 MiB, 16 MiB/request. Copy 40 MiB:
        // that's 40 chunks of 1 MiB -> requests of 16 + 16 + 8 chunks.
        let mib = 1024 * 1024u64;
        let total = 40 * mib;
        let mock = Arc::new(MockTransport::new());
        for bytes in [16 * mib, 16 * mib, 8 * mib] {
            mock.queue_response(build_ioctl_response(
                FSCTL_SRV_COPYCHUNK,
                copychunk_response_bytes(SrvCopychunkResponse {
                    chunks_written: (bytes / mib) as u32,
                    chunk_bytes_written: 0,
                    total_bytes_written: bytes as u32,
                }),
            ));
        }

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([7; RESUME_KEY_LEN]);
        let copied = tree
            .server_side_copy_range(&mut conn, dst_id(), &key, 0, 0, total)
            .await
            .unwrap();
        assert_eq!(copied, total);
    }

    #[tokio::test]
    async fn copy_range_renegotiates_then_succeeds() {
        // First request (conservative) is rejected with smaller limits; the
        // retry within them succeeds.
        let mock = Arc::new(MockTransport::new());
        mock.queue_response(build_ioctl_response_status(
            FSCTL_SRV_COPYCHUNK,
            NtStatus::INVALID_PARAMETER,
            copychunk_response_bytes(SrvCopychunkResponse {
                chunks_written: 1,
                chunk_bytes_written: 65536,
                total_bytes_written: 65536,
            }),
        ));
        // 200 KiB with 64 KiB chunks and 1 chunk/request -> 4 requests
        // (64 + 64 + 64 + 8 KiB).
        for bytes in [65536u32, 65536, 65536, 8192] {
            mock.queue_response(build_ioctl_response(
                FSCTL_SRV_COPYCHUNK,
                copychunk_response_bytes(SrvCopychunkResponse {
                    chunks_written: 1,
                    chunk_bytes_written: 0,
                    total_bytes_written: bytes,
                }),
            ));
        }

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([0; RESUME_KEY_LEN]);
        let copied = tree
            .server_side_copy_range(&mut conn, dst_id(), &key, 0, 0, 200 * 1024)
            .await
            .unwrap();
        assert_eq!(copied, 200 * 1024);
    }

    #[tokio::test]
    async fn copy_range_zero_length_is_noop() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let key = ResumeKey::from_bytes([0; RESUME_KEY_LEN]);
        let copied = tree
            .server_side_copy_range(&mut conn, dst_id(), &key, 0, 0, 0)
            .await
            .unwrap();
        assert_eq!(copied, 0);
    }

    // -- server_side_copy_file: full open/copy/close choreography -----

    #[tokio::test]
    async fn copy_file_range_places_chunk_at_requested_offsets() {
        use crate::msg::copychunk::SrvCopychunkCopy;
        use crate::msg::header::Header;
        use crate::msg::ioctl::{IoctlRequest, FSCTL_SRV_COPYCHUNK};

        let mock = Arc::new(MockTransport::new());
        let src_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        let dst = dst_id();
        let len = 1000u64;

        // CREATE(source) -> CREATE(dest) -> RESUME_KEY -> COPYCHUNK -> FLUSH ->
        // CLOSE(dest) -> CLOSE(source).
        mock.queue_response(build_create_response(src_id, 8192));
        mock.queue_response(build_create_response(dst, 8192));
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_REQUEST_RESUME_KEY,
            resume_key_response_bytes([3; RESUME_KEY_LEN]),
        ));
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_COPYCHUNK,
            copychunk_response_bytes(SrvCopychunkResponse {
                chunks_written: 1,
                chunk_bytes_written: 0,
                total_bytes_written: len as u32,
            }),
        ));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let copied = tree
            .server_side_copy_file_range(&mut conn, "src.bin", 100, "dst.bin", 4096, len)
            .await
            .unwrap();
        assert_eq!(copied, len);

        // The COPYCHUNK request must carry the caller's source/target offsets.
        let sent = mock.sent_messages();
        let copy = sent
            .iter()
            .find_map(|bytes| {
                let mut c = ReadCursor::new(&bytes[Header::SIZE..]);
                let req = IoctlRequest::unpack(&mut c).ok()?;
                if req.ctl_code != FSCTL_SRV_COPYCHUNK {
                    return None;
                }
                let mut b = ReadCursor::new(&req.input_data);
                SrvCopychunkCopy::unpack(&mut b).ok()
            })
            .expect("a COPYCHUNK request was sent");
        assert_eq!(copy.chunks.len(), 1);
        assert_eq!(copy.chunks[0].source_offset, 100);
        assert_eq!(copy.chunks[0].target_offset, 4096);
        assert_eq!(copy.chunks[0].length, len as u32);
    }

    #[tokio::test]
    async fn copy_file_opens_copies_and_closes_both_handles() {
        let mock = Arc::new(MockTransport::new());
        let src_id = FileId {
            persistent: 0xAA,
            volatile: 0xBB,
        };
        let dst = dst_id();
        let size = 5000u64;

        // CREATE(source, read) -> CREATE(dest, read+write) -> RESUME_KEY ->
        // COPYCHUNK -> FLUSH(dest) -> CLOSE(dest) -> CLOSE(source).
        mock.queue_response(build_create_response(src_id, size));
        mock.queue_response(build_create_response(dst, 0));
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_REQUEST_RESUME_KEY,
            resume_key_response_bytes([9; RESUME_KEY_LEN]),
        ));
        mock.queue_response(build_ioctl_response(
            FSCTL_SRV_COPYCHUNK,
            copychunk_response_bytes(SrvCopychunkResponse {
                chunks_written: 1,
                chunk_bytes_written: 0,
                total_bytes_written: size as u32,
            }),
        ));
        mock.queue_response(build_flush_response());
        mock.queue_response(build_close_response());
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let copied = tree
            .server_side_copy_file(&mut conn, "src.bin", "dst.bin")
            .await
            .unwrap();
        assert_eq!(copied, size);
    }
}
