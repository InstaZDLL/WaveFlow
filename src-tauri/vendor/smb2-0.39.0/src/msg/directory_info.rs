//! Directory-listing entries: what a QUERY_DIRECTORY response's output buffer
//! holds, for the two information classes this crate asks for.
//!
//! - `FileBothDirectoryInformation` (class 3, MS-FSCC § 2.4.8): what every
//!   listing asks for by default.
//! - `FileIdBothDirectoryInformation` (class 37, MS-FSCC § 2.4.22): the same
//!   entry plus `Reserved2` and the 64-bit `FileId` after the short name, the
//!   value `FileInternalInformation` calls `IndexNumber` (§ 2.4.27). Asked for
//!   only when a caller opts in.
//!
//! Entries are chained by `NextEntryOffset` and 8-byte aligned. Names come out
//! exactly as the wire carries them; mapping the private-use characters back
//! (`crate::name`) is the client's job, so both classes go through the one
//! decode site.

use crate::msg::query_directory::FileInformationClass;
use crate::pack::{FileTime, Pack, ReadCursor, Unpack, WriteCursor};
use crate::Result;

/// Size of the fixed part of a class 3 entry, up to `FileName`.
const BOTH_FIXED: usize = 94;
/// Size of the fixed part of a class 37 entry: class 3's plus `Reserved2` (2)
/// and `FileId` (8).
const ID_BOTH_FIXED: usize = 104;
/// `ShortName` is always 24 bytes on the wire, however long the name in it.
const SHORT_NAME_BYTES: usize = 24;

/// One directory entry, in either class.
///
/// `ShortName` isn't kept: nothing here uses 8.3 names, and the packer writes
/// it empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryInfoEntry {
    pub file_index: u32,
    pub creation_time: FileTime,
    pub last_access_time: FileTime,
    pub last_write_time: FileTime,
    pub change_time: FileTime,
    pub end_of_file: u64,
    pub allocation_size: u64,
    pub file_attributes: u32,
    pub ea_size: u32,
    /// The 64-bit file ID: `Some` for a class 37 entry, whatever its value
    /// (0 means the file system has none, MS-FSCC § 2.4.22), `None` for
    /// class 3, which doesn't carry one.
    pub file_id: Option<u64>,
    /// The name as the wire carries it, private-use mapping and all.
    pub file_name: String,
}

/// The classes [`parse_entries`] and [`pack_entries`] speak.
fn fixed_len(class: FileInformationClass) -> Result<usize> {
    match class {
        FileInformationClass::FileBothDirectoryInformation => Ok(BOTH_FIXED),
        FileInformationClass::FileIdBothDirectoryInformation => Ok(ID_BOTH_FIXED),
        other => Err(crate::Error::invalid_data(format!(
            "no directory entry parser for {other:?}"
        ))),
    }
}

/// Every entry in a QUERY_DIRECTORY output buffer of `class`.
///
/// A tail too short for a whole fixed part ends the walk rather than failing
/// it, as does a `NextEntryOffset` of 0. A name running past the buffer is an
/// error.
pub fn parse_entries(class: FileInformationClass, data: &[u8]) -> Result<Vec<DirectoryInfoEntry>> {
    let fixed = fixed_len(class)?;
    let with_id = fixed == ID_BOTH_FIXED;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while data.len().saturating_sub(offset) >= fixed {
        let mut cursor = ReadCursor::new(&data[offset..]);
        let next_entry_offset = cursor.read_u32_le()? as usize;
        let file_index = cursor.read_u32_le()?;
        let creation_time = FileTime::unpack(&mut cursor)?;
        let last_access_time = FileTime::unpack(&mut cursor)?;
        let last_write_time = FileTime::unpack(&mut cursor)?;
        let change_time = FileTime::unpack(&mut cursor)?;
        let end_of_file = cursor.read_u64_le()?;
        let allocation_size = cursor.read_u64_le()?;
        let file_attributes = cursor.read_u32_le()?;
        let file_name_length = cursor.read_u32_le()? as usize;
        let ea_size = cursor.read_u32_le()?;
        let _short_name_length = cursor.read_u8()?;
        let _reserved1 = cursor.read_u8()?;
        cursor.skip(SHORT_NAME_BYTES)?;
        let file_id = if with_id {
            let _reserved2 = cursor.read_u16_le()?;
            Some(cursor.read_u64_le()?)
        } else {
            None
        };
        let file_name = cursor.read_utf16_le(file_name_length)?;
        entries.push(DirectoryInfoEntry {
            file_index,
            creation_time,
            last_access_time,
            last_write_time,
            change_time,
            end_of_file,
            allocation_size,
            file_attributes,
            ea_size,
            file_id,
            file_name,
        });
        if next_entry_offset == 0 {
            break;
        }
        offset += next_entry_offset;
    }
    Ok(entries)
}

/// The output buffer a server would send for `entries`: chained, each but the
/// last padded to 8 bytes. A class 37 entry with no `file_id` packs a 0.
pub fn pack_entries(
    class: FileInformationClass,
    entries: &[DirectoryInfoEntry],
) -> Result<Vec<u8>> {
    let with_id = fixed_len(class)? == ID_BOTH_FIXED;
    let mut w = WriteCursor::new();
    for (i, entry) in entries.iter().enumerate() {
        let start = w.position();
        let name_len = entry.file_name.encode_utf16().count() * 2;
        w.write_u32_le(0); // NextEntryOffset, set once the entry's length is known.
        w.write_u32_le(entry.file_index);
        entry.creation_time.pack(&mut w);
        entry.last_access_time.pack(&mut w);
        entry.last_write_time.pack(&mut w);
        entry.change_time.pack(&mut w);
        w.write_u64_le(entry.end_of_file);
        w.write_u64_le(entry.allocation_size);
        w.write_u32_le(entry.file_attributes);
        w.write_u32_le(name_len as u32);
        w.write_u32_le(entry.ea_size);
        w.write_u8(0); // ShortNameLength
        w.write_u8(0); // Reserved1
        w.write_zeros(SHORT_NAME_BYTES);
        if with_id {
            w.write_u16_le(0); // Reserved2
            w.write_u64_le(entry.file_id.unwrap_or(0));
        }
        w.write_utf16_le(&entry.file_name);
        if i + 1 < entries.len() {
            w.align_to(8);
            w.set_u32_le_at(start, (w.position() - start) as u32);
        }
    }
    Ok(w.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: FileInformationClass = FileInformationClass::FileBothDirectoryInformation;
    const ID_BOTH: FileInformationClass = FileInformationClass::FileIdBothDirectoryInformation;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A class 37 entry assembled field by field from MS-FSCC § 2.4.22, so the
    /// codec is checked against the spec rather than against itself.
    fn spec_id_both_entry(next: u32, file_id: u64, name: &str) -> Vec<u8> {
        let name = utf16(name);
        let mut b = Vec::new();
        b.extend_from_slice(&next.to_le_bytes()); // NextEntryOffset
        b.extend_from_slice(&7u32.to_le_bytes()); // FileIndex
        b.extend_from_slice(&1u64.to_le_bytes()); // CreationTime
        b.extend_from_slice(&2u64.to_le_bytes()); // LastAccessTime
        b.extend_from_slice(&3u64.to_le_bytes()); // LastWriteTime
        b.extend_from_slice(&4u64.to_le_bytes()); // ChangeTime
        b.extend_from_slice(&100u64.to_le_bytes()); // EndOfFile
        b.extend_from_slice(&4096u64.to_le_bytes()); // AllocationSize
        b.extend_from_slice(&0x20u32.to_le_bytes()); // FileAttributes
        b.extend_from_slice(&(name.len() as u32).to_le_bytes()); // FileNameLength
        b.extend_from_slice(&9u32.to_le_bytes()); // EaSize
        b.push(4); // ShortNameLength
        b.push(0); // Reserved1
        let mut short = utf16("AB");
        short.resize(24, 0);
        b.extend_from_slice(&short); // ShortName
        b.extend_from_slice(&[0, 0]); // Reserved2
        b.extend_from_slice(&file_id.to_le_bytes()); // FileId
        assert_eq!(b.len(), ID_BOTH_FIXED);
        b.extend_from_slice(&name); // FileName
        b
    }

    #[test]
    fn a_class_37_entry_carries_its_file_id_after_the_short_name() {
        let mut data = spec_id_both_entry(0, 0x0123_4567_89AB_CDEF, "a.txt");
        let first_len = data.len().next_multiple_of(8);
        data[0..4].copy_from_slice(&(first_len as u32).to_le_bytes());
        data.resize(first_len, 0);
        data.extend_from_slice(&spec_id_both_entry(0, 0, ".."));

        let entries = parse_entries(ID_BOTH, &data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            DirectoryInfoEntry {
                file_index: 7,
                creation_time: FileTime(1),
                last_access_time: FileTime(2),
                last_write_time: FileTime(3),
                change_time: FileTime(4),
                end_of_file: 100,
                allocation_size: 4096,
                file_attributes: 0x20,
                ea_size: 9,
                file_id: Some(0x0123_4567_89AB_CDEF),
                file_name: "a.txt".into(),
            }
        );
        // A zero is reported as sent; what it means is the client's call.
        assert_eq!(entries[1].file_id, Some(0));
        assert_eq!(entries[1].file_name, "..");
    }

    #[test]
    fn a_class_3_entry_has_no_file_id_and_its_name_starts_ten_bytes_earlier() {
        // The same bytes read as class 3 put the name where class 37 keeps
        // Reserved2 + FileId: the one difference between the two layouts.
        let entry = DirectoryInfoEntry {
            file_name: "same".into(),
            ..Default::default()
        };
        let both = pack_entries(BOTH, std::slice::from_ref(&entry)).unwrap();
        let id_both = pack_entries(
            ID_BOTH,
            &[DirectoryInfoEntry {
                file_id: Some(5),
                ..entry.clone()
            }],
        )
        .unwrap();
        assert_eq!(both.len() + 10, id_both.len());
        assert_eq!(&both[..BOTH_FIXED], &id_both[..BOTH_FIXED]);
        assert_eq!(parse_entries(BOTH, &both).unwrap(), [entry]);
    }

    #[test]
    fn a_short_tail_ends_the_walk_and_a_long_name_is_an_error() {
        assert!(parse_entries(ID_BOTH, &[]).unwrap().is_empty());
        assert!(parse_entries(ID_BOTH, &[0; ID_BOTH_FIXED - 1])
            .unwrap()
            .is_empty());

        let mut data = spec_id_both_entry(0, 1, "abc");
        data.truncate(data.len() - 1);
        assert!(parse_entries(ID_BOTH, &data).is_err());
    }

    #[test]
    fn packed_entries_are_chained_and_8_byte_aligned() {
        let entries: Vec<_> = ["a", "bb", "ccc"]
            .iter()
            .map(|name| DirectoryInfoEntry {
                file_id: Some(1),
                file_name: (*name).into(),
                ..Default::default()
            })
            .collect();
        let data = pack_entries(ID_BOTH, &entries).unwrap();
        let mut offset = 0;
        for _ in 0..2 {
            let next = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
            assert_eq!(next % 8, 0);
            offset += next;
        }
        assert_eq!(
            &data[offset..offset + 4],
            &[0; 4],
            "the last one ends the chain"
        );
    }

    #[test]
    fn only_the_two_listing_classes_are_spoken() {
        let class = FileInformationClass::FileNamesInformation;
        assert!(parse_entries(class, &[]).is_err());
        assert!(pack_entries(class, &[]).is_err());
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_file_time, arb_utf16_string};
    use proptest::prelude::*;

    fn arb_entry(with_id: bool) -> impl Strategy<Value = DirectoryInfoEntry> {
        (
            (
                any::<u32>(),
                arb_file_time(),
                arb_file_time(),
                arb_file_time(),
            ),
            (arb_file_time(), any::<u64>(), any::<u64>(), any::<u32>()),
            (any::<u32>(), any::<u64>(), arb_utf16_string(64)),
        )
            .prop_map(
                move |(
                    (index, created, accessed, written),
                    (changed, eof, alloc, attrs),
                    (ea, id, name),
                )| {
                    DirectoryInfoEntry {
                        file_index: index,
                        creation_time: created,
                        last_access_time: accessed,
                        last_write_time: written,
                        change_time: changed,
                        end_of_file: eof,
                        allocation_size: alloc,
                        file_attributes: attrs,
                        ea_size: ea,
                        file_id: with_id.then_some(id),
                        file_name: name,
                    }
                },
            )
    }

    proptest! {
        #[test]
        fn file_both_directory_information_pack_unpack(
            entries in prop::collection::vec(arb_entry(false), 1..8),
        ) {
            let class = FileInformationClass::FileBothDirectoryInformation;
            let data = pack_entries(class, &entries).unwrap();
            prop_assert_eq!(parse_entries(class, &data).unwrap(), entries);
        }

        #[test]
        fn file_id_both_directory_information_pack_unpack(
            entries in prop::collection::vec(arb_entry(true), 1..8),
        ) {
            let class = FileInformationClass::FileIdBothDirectoryInformation;
            let data = pack_entries(class, &entries).unwrap();
            prop_assert_eq!(parse_entries(class, &data).unwrap(), entries);
        }
    }
}
