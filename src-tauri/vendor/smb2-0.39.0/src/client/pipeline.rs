//! Running a heterogeneous list of SMB2 operations through one call.
//!
//! [`Pipeline`] takes a mixed batch of reads, writes, deletes, listings, and
//! stats, runs them in order, and hands back one result per operation. It is
//! a convenience over calling [`Tree`] methods one at a time, not a
//! throughput feature: operations do not overlap on the wire, so it neither
//! fills the credit window nor saves round trips. To overlap server work,
//! run operations on several connections concurrently.

use log::trace;

use crate::client::connection::Connection;
use crate::client::tree::Tree;

/// An operation to execute through the pipeline.
#[derive(Debug, Clone)]
pub enum Op {
    /// Read a file, returning its contents.
    ReadFile(String),
    /// Write data to a file (create or overwrite).
    WriteFile(String, Vec<u8>),
    /// Delete a file.
    Delete(String),
    /// List a directory.
    ListDirectory(String),
    /// Get file metadata.
    Stat(String),
}

/// Result of a pipeline operation.
#[derive(Debug)]
pub enum OpResult {
    /// File data read successfully.
    FileData {
        /// The path that was read.
        path: String,
        /// The file contents.
        data: Vec<u8>,
    },
    /// File written successfully.
    Written {
        /// The path that was written.
        path: String,
        /// Number of bytes written.
        bytes_written: u64,
    },
    /// File deleted successfully.
    Deleted {
        /// The path that was deleted.
        path: String,
    },
    /// Directory listing.
    DirEntries {
        /// The path that was listed.
        path: String,
        /// The directory entries.
        entries: Vec<crate::client::tree::DirectoryEntry>,
    },
    /// File metadata.
    Stat {
        /// The path that was queried.
        path: String,
        /// The file information.
        info: crate::client::tree::FileInfo,
    },
    /// Operation failed.
    Error {
        /// The path that failed.
        path: String,
        /// The error that occurred.
        error: crate::Error,
    },
}

/// Runs a list of SMB operations in order and collects their results.
///
/// Each operation runs to completion before the next one starts, including
/// its internal steps (a read is CREATE + READ + CLOSE). Nothing here
/// interleaves work from different operations.
///
/// **It doesn't follow DFS links.** An operation on a path behind one comes
/// back as [`OpResult::Error`] with `STATUS_PATH_NOT_COVERED`
/// ([`ErrorKind::DfsReferral`](crate::ErrorKind::DfsReferral)); this is the
/// low-level path, and a hidden referral and retry would break its one
/// operation, one exchange contract. Run that path through an
/// [`SmbClient`](crate::SmbClient) method instead, which follows the link.
pub struct Pipeline<'a> {
    conn: &'a mut Connection,
    tree: &'a Tree,
}

impl<'a> Pipeline<'a> {
    /// Create a new pipeline bound to a connection and tree.
    pub fn new(conn: &'a mut Connection, tree: &'a Tree) -> Self {
        Self { conn, tree }
    }

    /// Execute a batch of operations and return the results.
    ///
    /// Results are returned in the same order as the input operations.
    /// Each operation that fails produces an [`OpResult::Error`] rather
    /// than aborting the entire batch.
    pub async fn execute(&mut self, ops: Vec<Op>) -> Vec<OpResult> {
        let mut results = Vec::with_capacity(ops.len());

        for op in ops {
            let result = self.execute_one(op).await;
            results.push(result);
        }

        results
    }

    /// Execute a single operation.
    async fn execute_one(&mut self, op: Op) -> OpResult {
        match op {
            Op::ReadFile(path) => {
                trace!("pipeline: read_file path={path:?}");
                match self.tree.read_file(self.conn, &path).await {
                    Ok(data) => OpResult::FileData { path, data },
                    Err(e) => OpResult::Error { path, error: e },
                }
            }
            Op::WriteFile(path, data) => {
                trace!("pipeline: write_file path={path:?}");
                match self.tree.write_file(self.conn, &path, &data).await {
                    Ok(bytes_written) => OpResult::Written {
                        path,
                        bytes_written,
                    },
                    Err(e) => OpResult::Error { path, error: e },
                }
            }
            Op::Delete(path) => {
                trace!("pipeline: delete path={path:?}");
                match self.tree.delete_file(self.conn, &path).await {
                    Ok(()) => OpResult::Deleted { path },
                    Err(e) => OpResult::Error { path, error: e },
                }
            }
            Op::ListDirectory(path) => {
                trace!("pipeline: list_directory path={path:?}");
                match self.tree.list_directory(self.conn, &path).await {
                    Ok(entries) => OpResult::DirEntries { path, entries },
                    Err(e) => OpResult::Error { path, error: e },
                }
            }
            Op::Stat(path) => {
                trace!("pipeline: stat path={path:?}");
                match self.tree.stat(self.conn, &path).await {
                    Ok(info) => OpResult::Stat { path, info },
                    Err(e) => OpResult::Error { path, error: e },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::connection::pack_message;
    use crate::client::test_helpers::{
        build_close_response, build_create_response, build_set_info_response, setup_connection,
    };
    use crate::client::tree::Tree;
    use crate::msg::create::{CreateAction, CreateResponse};
    use crate::msg::header::{ErrorResponse, Header};
    use crate::msg::query_directory::QueryDirectoryResponse;
    use crate::msg::query_info::QueryInfoResponse;
    use crate::msg::read::ReadResponse;
    use crate::msg::write::WriteResponse;
    use crate::pack::FileTime;
    use crate::transport::MockTransport;
    use crate::types::status::NtStatus;
    use crate::types::{Command, FileId, OplockLevel, TreeId};
    use std::sync::Arc;

    fn test_tree() -> Tree {
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

    fn build_create_response_directory(file_id: FileId) -> Vec<u8> {
        let mut h = Header::new_request(Command::Create);
        h.flags.set_response();
        h.credits = 32;

        let body = CreateResponse {
            oplock_level: OplockLevel::None,
            flags: 0,
            create_action: CreateAction::FileOpened,
            creation_time: FileTime(132_000_000_000_000_000),
            last_access_time: FileTime(132_000_000_000_000_000),
            last_write_time: FileTime(133_000_000_000_000_000),
            change_time: FileTime(133_000_000_000_000_000),
            allocation_size: 0,
            end_of_file: 0,
            file_attributes: 0x10, // DIRECTORY
            file_id,
            create_contexts: vec![],
        };

        pack_message(&h, &body)
    }

    fn build_flush_response() -> Vec<u8> {
        let mut h = Header::new_request(Command::Flush);
        h.flags.set_response();
        h.credits = 32;

        let body = crate::msg::flush::FlushResponse;
        pack_message(&h, &body)
    }

    fn build_read_response(data: Vec<u8>) -> Vec<u8> {
        let mut h = Header::new_request(Command::Read);
        h.flags.set_response();
        h.credits = 32;

        let body = ReadResponse {
            data_offset: 0x50,
            data_remaining: 0,
            flags: 0,
            data,
        };

        pack_message(&h, &body)
    }

    fn build_write_response(count: u32) -> Vec<u8> {
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

    fn build_query_info_response(output_buffer: Vec<u8>) -> Vec<u8> {
        let mut h = Header::new_request(Command::QueryInfo);
        h.flags.set_response();
        h.credits = 32;

        let body = QueryInfoResponse { output_buffer };

        pack_message(&h, &body)
    }

    fn build_query_directory_response(status: NtStatus, entries_data: Vec<u8>) -> Vec<u8> {
        let mut h = Header::new_request(Command::QueryDirectory);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;

        if status == NtStatus::NO_MORE_FILES {
            let body = ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            };
            return pack_message(&h, &body);
        }

        let body = QueryDirectoryResponse {
            output_buffer: entries_data,
        };

        pack_message(&h, &body)
    }

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
        // Padding to 40 bytes (Reserved)
        buf.extend_from_slice(&0u32.to_le_bytes());
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

    /// Build a single FileBothDirectoryInformation entry.
    fn build_file_both_dir_info(
        name: &str,
        size: u64,
        is_directory: bool,
        next_offset: u32,
    ) -> Vec<u8> {
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes_len = name_u16.len() * 2;

        let mut buf = Vec::new();
        buf.extend_from_slice(&next_offset.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // FileIndex
        buf.extend_from_slice(&132_000_000_000_000_000u64.to_le_bytes()); // CreationTime
        buf.extend_from_slice(&132_000_000_000_000_000u64.to_le_bytes()); // LastAccessTime
        buf.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes()); // LastWriteTime
        buf.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes()); // ChangeTime
        buf.extend_from_slice(&size.to_le_bytes());
        buf.extend_from_slice(&((size + 4095) & !4095).to_le_bytes()); // AllocationSize
        let attrs: u32 = if is_directory { 0x10 } else { 0x20 };
        buf.extend_from_slice(&attrs.to_le_bytes());
        buf.extend_from_slice(&(name_bytes_len as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // EaSize
        buf.push(0); // ShortNameLength
        buf.push(0); // Reserved
        buf.extend_from_slice(&[0u8; 24]); // ShortName
        for &u in &name_u16 {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf
    }

    /// Build a compound response frame with proper NextCommand offsets and padding.
    fn build_compound_response_frame(responses: &[Vec<u8>]) -> Vec<u8> {
        let mut padded: Vec<Vec<u8>> = Vec::new();
        for (i, resp) in responses.iter().enumerate() {
            let mut r = resp.clone();
            let is_last = i == responses.len() - 1;
            if !is_last {
                // Pad to 8-byte alignment.
                let remainder = r.len() % 8;
                if remainder != 0 {
                    r.resize(r.len() + (8 - remainder), 0);
                }
                // Set NextCommand.
                let next_cmd = r.len() as u32;
                r[20..24].copy_from_slice(&next_cmd.to_le_bytes());
            }
            padded.push(r);
        }
        let mut frame = Vec::new();
        for r in &padded {
            frame.extend_from_slice(r);
        }
        frame
    }

    /// Build a compound read response frame (CREATE + READ + CLOSE) for pipeline tests.
    fn build_compound_read_response(file_id: FileId, data: Vec<u8>) -> Vec<u8> {
        let create_resp = build_create_response(file_id, data.len() as u64);
        let read_resp = build_read_response(data);
        let close_resp = build_close_response();
        build_compound_response_frame(&[create_resp, read_resp, close_resp])
    }

    #[tokio::test]
    async fn pipeline_batch_of_three_reads() {
        let mock = Arc::new(MockTransport::new());

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // Three read operations, each needs a compound CREATE + READ + CLOSE frame.
        for i in 0..3 {
            let data = format!("content_{}", i);
            mock.queue_response(build_compound_read_response(file_id, data.into_bytes()));
        }

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![
                Op::ReadFile("file1.txt".to_string()),
                Op::ReadFile("file2.txt".to_string()),
                Op::ReadFile("file3.txt".to_string()),
            ])
            .await;

        assert_eq!(results.len(), 3);
        for (i, result) in results.into_iter().enumerate() {
            match result {
                OpResult::FileData { path, data } => {
                    assert_eq!(path, format!("file{}.txt", i + 1));
                    assert_eq!(data, format!("content_{}", i).into_bytes());
                }
                other => panic!("expected FileData, got {:?}", other),
            }
        }
    }

    #[tokio::test]
    async fn pipeline_mixed_ops() {
        let mock = Arc::new(MockTransport::new());

        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // Op 1: ReadFile -- compound CREATE + READ + CLOSE
        mock.queue_response(build_compound_read_response(file_id, b"hello".to_vec()));

        // Op 2: Delete -- compound CREATE + SET_INFO(disposition) + CLOSE
        let del_create = build_create_response(file_id, 0);
        let del_close = build_close_response();
        mock.queue_response(build_compound_response_frame(&[
            del_create,
            build_set_info_response(),
            del_close,
        ]));

        // Op 3: ListDirectory -- CREATE + QUERY_DIR + QUERY_DIR(NO_MORE) + CLOSE
        mock.queue_response(build_create_response_directory(file_id));
        let entry = build_file_both_dir_info("test.txt", 100, false, 0);
        mock.queue_response(build_query_directory_response(NtStatus::SUCCESS, entry));
        mock.queue_response(build_query_directory_response(
            NtStatus::NO_MORE_FILES,
            vec![],
        ));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![
                Op::ReadFile("data.bin".to_string()),
                Op::Delete("old.txt".to_string()),
                Op::ListDirectory("docs".to_string()),
            ])
            .await;

        assert_eq!(results.len(), 3);

        match &results[0] {
            OpResult::FileData { data, .. } => assert_eq!(data, b"hello"),
            other => panic!("expected FileData, got {:?}", other),
        }

        match &results[1] {
            OpResult::Deleted { path } => assert_eq!(path, "old.txt"),
            other => panic!("expected Deleted, got {:?}", other),
        }

        match &results[2] {
            OpResult::DirEntries { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].name, "test.txt");
            }
            other => panic!("expected DirEntries, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pipeline_delete_file() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // DELETE = compound CREATE + SET_INFO(disposition) + CLOSE
        let create_resp = build_create_response(file_id, 0);
        let close_resp = build_close_response();
        let frame =
            build_compound_response_frame(&[create_resp, build_set_info_response(), close_resp]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![Op::Delete("remove_me.txt".to_string())])
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            OpResult::Deleted { path } => assert_eq!(path, "remove_me.txt"),
            other => panic!("expected Deleted, got {:?}", other),
        }

        // One compound frame sent.
        let sent = mock.sent_messages();
        assert_eq!(sent.len(), 1);
    }

    #[tokio::test]
    async fn pipeline_write_file() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // WRITE uses the compound CREATE+WRITE+FLUSH, then a standalone CLOSE.
        let create_resp = build_create_response(file_id, 0);
        let write_resp = build_write_response(11);
        let flush_resp = build_flush_response();
        mock.queue_response(build_compound_response_frame(&[
            create_resp,
            write_resp,
            flush_resp,
        ]));
        mock.queue_response(build_close_response());

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![Op::WriteFile(
                "output.txt".to_string(),
                b"hello world".to_vec(),
            )])
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            OpResult::Written {
                path,
                bytes_written,
            } => {
                assert_eq!(path, "output.txt");
                assert_eq!(*bytes_written, 11);
            }
            other => panic!("expected Written, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pipeline_stat() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // STAT = compound CREATE + QUERY_INFO(basic) + QUERY_INFO(standard) + CLOSE
        let create_resp = build_create_response(file_id, 0);

        let basic_info = build_file_basic_info(
            132_000_000_000_000_000,
            132_100_000_000_000_000,
            133_000_000_000_000_000,
            133_000_000_000_000_000,
            0x20, // ARCHIVE (not a directory)
        );
        let basic_resp = build_query_info_response(basic_info);

        let std_info = build_file_standard_info(
            4096,  // allocation_size
            2048,  // end_of_file (actual size)
            1,     // number_of_links
            false, // delete_pending
            false, // directory
        );
        let std_resp = build_query_info_response(std_info);

        // FileAttributeTagInformation: attributes, then a tag (none here).
        let tag_resp = build_query_info_response([0x20u32, 0].map(u32::to_le_bytes).concat());

        let close_resp = build_close_response();

        let frame = build_compound_response_frame(&[
            create_resp,
            basic_resp,
            std_resp,
            tag_resp,
            close_resp,
        ]);
        mock.queue_response(frame);

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![Op::Stat("info.txt".to_string())])
            .await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            OpResult::Stat { path, info } => {
                assert_eq!(path, "info.txt");
                assert_eq!(info.size, 2048);
                assert!(!info.is_directory);
                assert_eq!(info.created, FileTime(132_000_000_000_000_000));
                assert_eq!(info.modified, FileTime(133_000_000_000_000_000));
            }
            other => panic!("expected Stat, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn pipeline_error_does_not_abort_batch() {
        let mock = Arc::new(MockTransport::new());
        let file_id = FileId {
            persistent: 1,
            volatile: 2,
        };

        // Op 1: ReadFile that fails at CREATE -- compound frame with cascaded errors.
        let error_body = ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        let mut h1 = Header::new_request(Command::Create);
        h1.flags.set_response();
        h1.credits = 32;
        h1.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let create_err = pack_message(&h1, &error_body);

        let mut h2 = Header::new_request(Command::Read);
        h2.flags.set_response();
        h2.credits = 32;
        h2.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let read_err = pack_message(&h2, &error_body);

        let mut h3 = Header::new_request(Command::Close);
        h3.flags.set_response();
        h3.credits = 32;
        h3.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let close_err = pack_message(&h3, &error_body);

        mock.queue_response(build_compound_response_frame(&[
            create_err, read_err, close_err,
        ]));

        // Op 2: ReadFile that succeeds -- compound frame.
        mock.queue_response(build_compound_read_response(file_id, b"abc".to_vec()));

        let mut conn = setup_connection(&mock);
        let tree = test_tree();
        let mut pipeline = Pipeline::new(&mut conn, &tree);

        let results = pipeline
            .execute(vec![
                Op::ReadFile("missing.txt".to_string()),
                Op::ReadFile("exists.txt".to_string()),
            ])
            .await;

        assert_eq!(results.len(), 2);
        match &results[0] {
            OpResult::Error { path, .. } => assert_eq!(path, "missing.txt"),
            other => panic!("expected Error, got {:?}", other),
        }
        match &results[1] {
            OpResult::FileData { path, data } => {
                assert_eq!(path, "exists.txt");
                assert_eq!(data, b"abc");
            }
            other => panic!("expected FileData, got {:?}", other),
        }
    }
}
