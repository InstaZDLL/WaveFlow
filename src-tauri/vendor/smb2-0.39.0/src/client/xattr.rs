//! macOS extended attributes, stored the way macOS's own SMB client stores them.
//!
//! A mount of the same share sees what these calls write, and the other way
//! round, byte for byte. The library stays at the byte level: it doesn't parse
//! plists, tag colors, or Finder flags.
//!
//! - **Default mapping**: an xattr is a named stream of the same name, whose
//!   inner `:` is mapped to U+F022 like any other (`streams.rs`). So
//!   `com.apple.metadata:_kMDItemUserTags` travels as
//!   `f.txt:com.apple.metadata<U+F022>_kMDItemUserTags`.
//! - **`com.apple.ResourceFork`** is the `AFP_Resource` stream, as is.
//! - **`com.apple.FinderInfo`** is the 32 bytes at offset 16 of the 60-byte
//!   `AFP_AfpInfo` stream. Writing it keeps the rest of that stream.
//!
//! Evidence, and what each server does: `docs/notes/named-streams-and-xattrs.md`.

use log::trace;

use crate::client::batch::{self, BatchOptions, BatchResults};
use crate::client::connection::Connection;
use crate::client::streams::is_missing_name;
use crate::error::Result;
use crate::types::Command;
use crate::{Error, Tree};

/// The xattr holding Finder's flags, label color, and creator codes.
pub const XATTR_FINDER_INFO: &str = "com.apple.FinderInfo";
/// The xattr holding a classic Mac resource fork.
pub const XATTR_RESOURCE_FORK: &str = "com.apple.ResourceFork";

const STREAM_AFP_INFO: &str = "AFP_AfpInfo";
const STREAM_AFP_RESOURCE: &str = "AFP_Resource";

/// The size of `com.apple.FinderInfo`.
const FINDER_INFO_LEN: usize = 32;
/// The size of an `AFP_AfpInfo` stream.
const AFP_INFO_LEN: usize = 60;
/// Where FinderInfo starts inside it.
const AFP_INFO_FINDER_OFFSET: usize = 16;
/// `AFP\0`, then version `0x00000100`, both big-endian, which Samba's `vfs_fruit`
/// checks on every write: it refuses anything else, with `STATUS_NOT_FOUND` on the
/// WRITE (Samba 4.20.6, 2026-10-31).
const AFP_INFO_HEADER: [u8; 8] = [b'A', b'F', b'P', 0, 0, 0, 1, 0];
/// "No backup time" (`AD_DATE_START`), what a server reports for a new stream.
const AFP_INFO_NO_BACKUP: [u8; 4] = [0x80, 0, 0, 0];
const AFP_INFO_BACKUP_OFFSET: usize = 12;

/// What one READ asks for in a batch: one credit's worth, far above a tag plist.
/// A bigger attribute costs that item a second round trip.
const BATCH_READ: u64 = 65536;

/// What one item charges: CREATE, a READ or WRITE of up to one credit, CLOSE.
const BATCH_ITEM_CHARGE: u16 = 3;

/// The stream that holds xattr `name`.
///
/// ```
/// use smb2::xattr_stream_name;
/// assert_eq!(xattr_stream_name("com.apple.FinderInfo"), "AFP_AfpInfo");
/// assert_eq!(xattr_stream_name("user.note"), "user.note");
/// ```
pub fn xattr_stream_name(name: &str) -> &str {
    match name {
        XATTR_FINDER_INFO => STREAM_AFP_INFO,
        XATTR_RESOURCE_FORK => STREAM_AFP_RESOURCE,
        other => other,
    }
}

/// The xattr that stream `stream` holds, as [`Tree::list_xattrs`] reports it.
pub fn stream_xattr_name(stream: &str) -> &str {
    match stream {
        STREAM_AFP_INFO => XATTR_FINDER_INFO,
        STREAM_AFP_RESOURCE => XATTR_RESOURCE_FORK,
        other => other,
    }
}

/// FinderInfo out of an `AFP_AfpInfo` stream. An unrecognized stream is an
/// error: guessing at it could hand out bytes that aren't FinderInfo.
pub(crate) fn finder_info_from_afp_info(stream: &[u8]) -> Result<Vec<u8>> {
    if stream.len() < AFP_INFO_LEN {
        return Err(Error::invalid_data(format!(
            "AFP_AfpInfo is {} bytes, expected {AFP_INFO_LEN}",
            stream.len()
        )));
    }
    if stream[..4] != AFP_INFO_HEADER[..4] {
        return Err(Error::invalid_data("AFP_AfpInfo has the wrong signature"));
    }
    Ok(stream[AFP_INFO_FINDER_OFFSET..AFP_INFO_FINDER_OFFSET + FINDER_INFO_LEN].to_vec())
}

/// `existing`'s `AFP_AfpInfo` with its FinderInfo replaced, or a new one. The
/// backup time and ProDOS info stay what they were.
fn afp_info_with_finder_info(existing: Option<&[u8]>, finder_info: &[u8]) -> Result<Vec<u8>> {
    if finder_info.len() != FINDER_INFO_LEN {
        return Err(Error::invalid_data(format!(
            "com.apple.FinderInfo is {FINDER_INFO_LEN} bytes, got {}",
            finder_info.len()
        )));
    }
    let mut stream = match existing {
        Some(existing) => {
            finder_info_from_afp_info(existing)?;
            existing[..AFP_INFO_LEN].to_vec()
        }
        None => {
            let mut fresh = vec![0u8; AFP_INFO_LEN];
            fresh[AFP_INFO_BACKUP_OFFSET..AFP_INFO_BACKUP_OFFSET + 4]
                .copy_from_slice(&AFP_INFO_NO_BACKUP);
            fresh
        }
    };
    stream[..AFP_INFO_HEADER.len()].copy_from_slice(&AFP_INFO_HEADER);
    stream[AFP_INFO_FINDER_OFFSET..AFP_INFO_FINDER_OFFSET + FINDER_INFO_LEN]
        .copy_from_slice(finder_info);
    Ok(stream)
}

fn check_xattr_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid_data("an xattr name can't be empty"));
    }
    Ok(())
}

impl Tree {
    /// One xattr read. `assume_exists` is [`BatchOptions::assume_paths_exist`].
    ///
    /// Without it, the list of the file's streams comes first: most files in a
    /// listing have no such attribute, and that answers them in one round trip,
    /// tells a missing file apart, and gives the READ its exact size. Reading
    /// first and asking about the file only on "not found" measured about a third
    /// slower where 5% of files carried the attribute (and as fast where all of
    /// them did): `docs/notes/named-streams-and-xattrs.md`.
    async fn get_xattr_item(
        &self,
        conn: &mut Connection,
        path: &str,
        name: &str,
        assume_exists: bool,
    ) -> Result<Option<Vec<u8>>> {
        check_xattr_name(name)?;
        let stream = xattr_stream_name(name);
        let expected = if assume_exists {
            BATCH_READ
        } else {
            let listed = self.list_streams(conn, path).await?;
            // Case-insensitively, as NTFS names them. A false match only costs
            // a READ that answers "not found".
            match listed.iter().find(|s| s.name.eq_ignore_ascii_case(stream)) {
                Some(found) => found.size.max(1),
                None => return Ok(None),
            }
        };
        match self.read_stream_sized(conn, path, stream, expected).await {
            Ok(data) if name == XATTR_FINDER_INFO => finder_info_from_afp_info(&data).map(Some),
            Ok(data) => Ok(Some(data)),
            // Removed since the listing, or never there, or (with
            // `assume_exists`) the file itself gone, which that option accepts.
            Err(Error::Protocol {
                status,
                command: Command::Create,
            }) if is_missing_name(status) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read an xattr. `Ok(None)` when the file is there and has no such
    /// attribute; [`ErrorKind::NotFound`](crate::ErrorKind::NotFound) when the
    /// file isn't.
    ///
    /// One round trip when the file has no such attribute, two when it has. For
    /// many files, use [`get_xattr_many`](Self::get_xattr_many).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let tags = tree
    ///     .get_xattr(conn, "notes.txt", "com.apple.metadata:_kMDItemUserTags")
    ///     .await?;
    /// println!("{:?}", tags.map(|plist| plist.len()));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_xattr(
        &self,
        conn: &mut Connection,
        path: &str,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        trace!("xattr: get path={path:?} name={name:?}");
        self.get_xattr_item(conn, path, name, false).await
    }

    /// Write an xattr, creating or replacing it. A missing file is
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound) and is never created
    /// (see [`write_stream`](Self::write_stream)).
    ///
    /// `com.apple.FinderInfo` must be exactly 32 bytes; it reads the stream
    /// around it and writes the whole stream back, so it takes one more round
    /// trip than the others.
    pub async fn set_xattr(
        &self,
        conn: &mut Connection,
        path: &str,
        name: &str,
        value: &[u8],
    ) -> Result<()> {
        check_xattr_name(name)?;
        if name != XATTR_FINDER_INFO {
            return self
                .write_stream(conn, path, xattr_stream_name(name), value)
                .await;
        }
        let existing = match self.read_stream(conn, path, STREAM_AFP_INFO).await {
            Ok(existing) => Some(existing),
            Err(Error::Protocol {
                status,
                command: Command::Create,
            }) if is_missing_name(status) => None,
            Err(e) => return Err(e),
        };
        let stream = afp_info_with_finder_info(existing.as_deref(), value)?;
        self.write_stream(conn, path, STREAM_AFP_INFO, &stream)
            .await
    }

    /// Remove an xattr. Removing one that isn't there succeeds, since what the
    /// caller asked for is true. A missing file is
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound).
    ///
    /// Removing `com.apple.FinderInfo` removes the `AFP_AfpInfo` stream, the way
    /// a mount does.
    ///
    /// A server can keep what it reported deleted, which is
    /// [`Error::StreamDeleteIgnored`]: a QNAP does that with
    /// `com.apple.metadata:_kMDItemUserTags` and `com.apple.ResourceFork`. To clear
    /// Finder tags anywhere, write what macOS writes: a plist with an empty array.
    pub async fn remove_xattr(&self, conn: &mut Connection, path: &str, name: &str) -> Result<()> {
        check_xattr_name(name)?;
        match self
            .delete_stream(conn, path, xattr_stream_name(name))
            .await
        {
            Err(Error::Protocol {
                status,
                command: Command::Create,
            }) if is_missing_name(status) => self.probe_path(conn, path).await,
            other => other,
        }
    }

    /// List a file's xattr names, in the form the other calls take them.
    pub async fn list_xattrs(&self, conn: &mut Connection, path: &str) -> Result<Vec<String>> {
        let streams = self.list_streams(conn, path).await?;
        Ok(streams
            .into_iter()
            .map(|stream| stream_xattr_name(&stream.name).to_string())
            .collect())
    }

    /// Read one xattr from many files, overlapped on this connection. The
    /// results are in input order, one per path, each independent of the others.
    ///
    /// Each item means what [`get_xattr`](Self::get_xattr) means. When the
    /// caller just listed these paths and many of them may carry the attribute,
    /// set [`assume_paths_exist`](BatchOptions::assume_paths_exist): every file
    /// then costs one round trip.
    ///
    /// To show results as they arrive, use
    /// [`get_xattr_each`](Self::get_xattr_each). A path behind a DFS link answers
    /// [`ErrorKind::DfsReferral`](crate::ErrorKind::DfsReferral) here;
    /// [`SmbClient::get_xattr_many`](crate::SmbClient::get_xattr_many) follows it.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// use smb2::BatchOptions;
    ///
    /// let paths = ["a.txt", "b.txt"];
    /// let options = BatchOptions::new().assume_paths_exist(true);
    /// let tags = tree
    ///     .get_xattr_many(conn, &paths, "com.apple.metadata:_kMDItemUserTags", options)
    ///     .await;
    /// assert_eq!(tags.len(), paths.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_xattr_many(
        &self,
        conn: &mut Connection,
        paths: &[&str],
        name: &str,
        options: BatchOptions,
    ) -> Vec<Result<Option<Vec<u8>>>> {
        self.get_xattr_each(conn, paths, name, options)
            .collect_in_order()
            .await
    }

    /// [`get_xattr_many`](Self::get_xattr_many), handing each result over as it
    /// finishes, with its index in `paths`.
    ///
    /// A busy NAS sometimes blocks for seconds on one open, so what a listing
    /// wants is the fast majority now. Nothing goes out until the returned
    /// [`BatchResults`] is polled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let paths = ["a.txt", "b.txt"];
    /// let mut results = tree.get_xattr_each(conn, &paths, "user.note", Default::default());
    /// while let Some((index, value)) = results.next().await {
    ///     println!("{}: {:?}", paths[index], value?.map(|v| v.len()));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_xattr_each(
        &self,
        conn: &mut Connection,
        paths: &[&str],
        name: &str,
        options: BatchOptions,
    ) -> BatchResults<Option<Vec<u8>>> {
        let window = batch::window_for(conn, BATCH_ITEM_CHARGE, options.window);
        let assume_exists = options.assume_paths_exist;
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_string()).collect();
        let name = name.to_string();
        let (tree, conn) = (self.clone(), conn.clone());
        batch::run(paths.len(), window, move |index| {
            let (tree, mut conn) = (tree.clone(), conn.clone());
            let (path, name) = (paths[index].clone(), name.clone());
            async move {
                tree.get_xattr_item(&mut conn, &path, &name, assume_exists)
                    .await
            }
        })
    }

    /// Write one xattr on many files, overlapped on this connection, each with
    /// its own value. The results are in input order, each independent of the
    /// others. Each item means what [`set_xattr`](Self::set_xattr) means.
    pub async fn set_xattr_many(
        &self,
        conn: &mut Connection,
        items: &[(&str, &[u8])],
        name: &str,
        options: BatchOptions,
    ) -> Vec<Result<()>> {
        let window = batch::window_for(conn, BATCH_ITEM_CHARGE, options.window);
        let items: Vec<(String, Vec<u8>)> = items
            .iter()
            .map(|(path, value)| ((*path).to_string(), value.to_vec()))
            .collect();
        let name = name.to_string();
        let (tree, conn) = (self.clone(), conn.clone());
        batch::run(items.len(), window, move |index| {
            let (tree, mut conn) = (tree.clone(), conn.clone());
            let (path, value) = items[index].clone();
            let name = name.clone();
            async move { tree.set_xattr(&mut conn, &path, &name, &value).await }
        })
        .collect_in_order()
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_mapping_is_symmetric() {
        for name in [XATTR_FINDER_INFO, XATTR_RESOURCE_FORK, "user.note"] {
            assert_eq!(stream_xattr_name(xattr_stream_name(name)), name);
        }
    }

    #[test]
    fn an_afp_info_stream_gives_up_the_32_bytes_at_offset_16() {
        let mut stream = vec![0u8; 60];
        stream[..8].copy_from_slice(&AFP_INFO_HEADER);
        for (i, byte) in stream[16..48].iter_mut().enumerate() {
            *byte = i as u8 + 1;
        }
        let finder_info = finder_info_from_afp_info(&stream).unwrap();
        assert_eq!(finder_info.len(), 32);
        assert_eq!(finder_info[0], 1);
        assert_eq!(finder_info[31], 32);
    }

    #[test]
    fn a_stream_that_isnt_afp_info_is_refused() {
        assert!(finder_info_from_afp_info(&[0u8; 59]).is_err());
        assert!(finder_info_from_afp_info(&[0u8; 60]).is_err());
    }

    #[test]
    fn a_new_afp_info_carries_the_signature_vfs_fruit_checks() {
        let stream = afp_info_with_finder_info(None, &[7u8; 32]).unwrap();
        assert_eq!(stream.len(), 60);
        assert_eq!(&stream[..8], b"AFP\0\0\0\x01\0");
        assert_eq!(&stream[12..16], &[0x80, 0, 0, 0]);
        assert_eq!(&stream[16..48], &[7u8; 32]);
    }

    #[test]
    fn replacing_finder_info_keeps_the_rest_of_the_stream() {
        let mut existing = afp_info_with_finder_info(None, &[1u8; 32]).unwrap();
        existing[12..16].copy_from_slice(&[9, 9, 9, 9]); // backup time
        existing[48..54].copy_from_slice(&[8u8; 6]); // ProDOS
        let replaced = afp_info_with_finder_info(Some(&existing), &[2u8; 32]).unwrap();
        assert_eq!(&replaced[12..16], &[9, 9, 9, 9]);
        assert_eq!(&replaced[16..48], &[2u8; 32]);
        assert_eq!(&replaced[48..54], &[8u8; 6]);
    }

    #[test]
    fn finder_info_of_the_wrong_size_is_refused() {
        assert!(afp_info_with_finder_info(None, &[0u8; 31]).is_err());
    }

    // ── Against a mock server ─────────────────────────────────────────

    use std::sync::Arc;

    use crate::client::test_helpers::{
        build_close_error_response, build_close_response, build_compound_response_frame,
        build_create_error_response, build_create_response, build_query_info_response,
        build_read_response, build_tree_connect_response, setup_connection,
    };
    use crate::msg::tree_connect::ShareType;
    use crate::pack::Unpack;
    use crate::transport::MockTransport;
    use crate::types::status::NtStatus;
    use crate::types::{FileId, TreeId};
    use crate::{BatchOptions, ErrorKind};

    const FILE: FileId = FileId {
        persistent: 1,
        volatile: 2,
    };

    async fn connected() -> (Arc<MockTransport>, Connection, Tree) {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);
        mock.queue_response(build_tree_connect_response(TreeId(7), ShareType::Disk));
        let tree = Tree::connect(&mut conn, "share").await.unwrap();
        (mock, conn, tree)
    }

    fn missing_stream() -> Vec<u8> {
        build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            crate::client::test_helpers::build_read_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_close_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ])
    }

    fn probe(status: NtStatus) -> Vec<u8> {
        if status == NtStatus::SUCCESS {
            build_compound_response_frame(&[build_create_response(FILE, 0), build_close_response()])
        } else {
            build_compound_response_frame(&[
                build_create_error_response(status),
                build_close_error_response(status),
            ])
        }
    }

    /// A stream list holding `names`.
    fn listing(names: &[&str]) -> Vec<u8> {
        let mut data = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let units: Vec<u8> = format!(":{name}:$DATA")
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let start = data.len();
            let entry_len = (24 + units.len()).next_multiple_of(8);
            let next = if index + 1 < names.len() {
                entry_len as u32
            } else {
                0
            };
            data.extend_from_slice(&next.to_le_bytes());
            data.extend_from_slice(&(units.len() as u32).to_le_bytes());
            data.extend_from_slice(&60u64.to_le_bytes());
            data.extend_from_slice(&60u64.to_le_bytes());
            data.extend_from_slice(&units);
            data.resize(start + entry_len, 0);
        }
        build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(data),
            build_close_response(),
        ])
    }

    fn file_gone() -> Vec<u8> {
        build_compound_response_frame(&[
            build_create_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_close_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
            build_close_error_response(NtStatus::OBJECT_NAME_NOT_FOUND),
        ])
    }

    fn stream_read(data: Vec<u8>) -> Vec<u8> {
        build_compound_response_frame(&[
            build_create_response(FILE, data.len() as u64),
            build_read_response(data),
            build_close_response(),
        ])
    }

    #[tokio::test]
    async fn get_xattr_tells_no_attribute_from_no_file() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(listing(&["user.other"]));
        assert_eq!(
            tree.get_xattr(&mut conn, "f", "user.a").await.unwrap(),
            None
        );
        // One round trip: no READ for an attribute the list doesn't have.
        assert_eq!(mock.sent_count(), 2);

        mock.queue_response(file_gone());
        let err = tree
            .get_xattr(&mut conn, "gone", "user.a")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn an_attribute_removed_after_the_list_reads_as_none() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(listing(&["user.a"]));
        mock.queue_response(missing_stream());
        assert_eq!(
            tree.get_xattr(&mut conn, "f", "user.a").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn get_xattr_unwraps_finder_info() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(listing(&["AFP_AfpInfo"]));
        mock.queue_response(stream_read(
            afp_info_with_finder_info(None, &[5u8; 32]).unwrap(),
        ));
        let value = tree
            .get_xattr(&mut conn, "f", XATTR_FINDER_INFO)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value, [5u8; 32]);
    }

    #[tokio::test]
    async fn the_fast_path_goes_straight_to_the_stream() {
        let (mock, mut conn, tree) = connected().await;
        for _ in 0..3 {
            mock.queue_response(missing_stream());
        }
        let results = tree
            .get_xattr_many(
                &mut conn,
                &["a", "b", "c"],
                "user.a",
                BatchOptions::new().assume_paths_exist(true),
            )
            .await;
        assert!(results.iter().all(|r| matches!(r, Ok(None))));
        // TreeConnect and one compound per file.
        assert_eq!(mock.sent_count(), 4);
    }

    #[tokio::test]
    async fn the_exact_path_reports_a_missing_file_for_that_item_only() {
        let (mock, mut conn, tree) = connected().await;
        // One at a time, so the canned answers line up with the requests.
        let options = BatchOptions::new().window(1);
        mock.queue_response(listing(&["user.a"]));
        mock.queue_response(stream_read(vec![9]));
        mock.queue_response(file_gone());
        mock.queue_response(listing(&[]));
        let results = tree
            .get_xattr_many(
                &mut conn,
                &["tagged", "gone", "untagged"],
                "user.a",
                options,
            )
            .await;
        assert_eq!(results[0].as_ref().unwrap().as_deref(), Some(&[9u8][..]));
        assert_eq!(results[1].as_ref().unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(*results[2].as_ref().unwrap(), None);
    }

    #[tokio::test]
    async fn results_also_arrive_one_at_a_time_with_their_index() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(listing(&[]));
        mock.queue_response(listing(&[]));
        let mut results = tree.get_xattr_each(
            &mut conn,
            &["a", "b"],
            "user.a",
            BatchOptions::new().window(1),
        );
        assert_eq!(results.next().await.unwrap().0, 0);
        assert_eq!(results.next().await.unwrap().0, 1);
        assert!(results.next().await.is_none());
    }

    #[tokio::test]
    async fn a_batch_asks_for_one_credit_per_read() {
        let (mock, mut conn, tree) = connected().await;
        mock.queue_response(stream_read(vec![1, 2]));
        tree.get_xattr_many(
            &mut conn,
            &["a"],
            "user.a",
            BatchOptions::new().assume_paths_exist(true),
        )
        .await
        .remove(0)
        .unwrap();
        let bytes = mock.sent_message(1).unwrap();
        let first =
            crate::msg::header::Header::unpack(&mut crate::pack::ReadCursor::new(&bytes)).unwrap();
        let second = crate::msg::header::Header::unpack(&mut crate::pack::ReadCursor::new(
            &bytes[first.next_command as usize..],
        ))
        .unwrap();
        assert_eq!(second.credit_charge.0, 1);
    }

    #[tokio::test]
    async fn the_window_stays_inside_half_a_small_credit_ceiling() {
        let (_, conn, _) = connected().await;
        assert_eq!(batch::window_for(&conn, 3, None), 16);
        assert_eq!(batch::window_for(&conn, 3, Some(4)), 4);
        conn.set_credit_ceiling(64);
        assert_eq!(batch::window_for(&conn, 3, None), 10);
        assert_eq!(batch::window_for(&conn, 3, Some(4)), 4);
        conn.set_credit_ceiling(2);
        assert_eq!(batch::window_for(&conn, 3, None), 1);
    }

    #[tokio::test]
    async fn removing_an_attribute_that_isnt_there_succeeds_on_a_file_that_is() {
        let (mock, mut conn, tree) = connected().await;
        let missing = file_gone;
        mock.queue_response(missing());
        mock.queue_response(probe(NtStatus::SUCCESS));
        tree.remove_xattr(&mut conn, "f", "user.a").await.unwrap();

        mock.queue_response(missing());
        mock.queue_response(probe(NtStatus::OBJECT_NAME_NOT_FOUND));
        let err = tree
            .remove_xattr(&mut conn, "gone", "user.a")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn list_xattrs_gives_the_names_the_other_calls_take() {
        let (mock, mut conn, tree) = connected().await;
        let entry = |name: &str, next: u32| {
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let mut entry = Vec::new();
            entry.extend_from_slice(&next.to_le_bytes());
            entry.extend_from_slice(&(units.len() as u32).to_le_bytes());
            entry.extend_from_slice(&[0u8; 16]);
            entry.extend_from_slice(&units);
            if next > 0 {
                entry.resize(next as usize, 0);
            }
            entry
        };
        let mut data = entry(":AFP_AfpInfo:$DATA", 64);
        data.extend(entry(":AFP_Resource:$DATA", 64));
        data.extend(entry(
            ":com.apple.metadata\u{F022}_kMDItemUserTags:$DATA",
            0,
        ));
        mock.queue_response(build_compound_response_frame(&[
            build_create_response(FILE, 0),
            build_query_info_response(data),
            build_close_response(),
        ]));
        assert_eq!(
            tree.list_xattrs(&mut conn, "f").await.unwrap(),
            [
                XATTR_FINDER_INFO,
                XATTR_RESOURCE_FORK,
                "com.apple.metadata:_kMDItemUserTags",
            ]
        );
    }

    #[tokio::test]
    async fn an_empty_xattr_name_is_refused_before_anything_is_sent() {
        let (mock, mut conn, tree) = connected().await;
        assert!(tree.get_xattr(&mut conn, "f", "").await.is_err());
        assert!(tree.set_xattr(&mut conn, "f", "", b"x").await.is_err());
        assert!(tree.remove_xattr(&mut conn, "f", "").await.is_err());
        assert_eq!(mock.sent_count(), 1);
    }
}
