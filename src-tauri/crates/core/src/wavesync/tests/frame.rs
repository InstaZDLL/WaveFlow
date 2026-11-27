use super::*;
use crate::wavesync::frame::{Frame, FrameError, ReadFailure, ReadRequest, MAX_READ_LEN};

fn read(offset: u64, len: u32) -> ReadRequest {
    ReadRequest {
        req: 7,
        entry: id(3),
        offset,
        len,
    }
}

#[test]
fn every_frame_reads_back_as_itself() {
    let frames = [
        Frame::Read(read(u64::MAX, MAX_READ_LEN)),
        Frame::Data {
            req: u32::MAX,
            bytes: vec![1, 2, 3],
        },
        Frame::Data {
            req: 0,
            bytes: Vec::new(),
        },
        Frame::Data {
            req: 1,
            bytes: vec![0; MAX_READ_LEN as usize],
        },
        Frame::Fail {
            req: 2,
            failure: ReadFailure::Gone,
        },
        Frame::Fail {
            req: 2,
            failure: ReadFailure::Changed,
        },
        Frame::Fail {
            req: 2,
            failure: ReadFailure::Unavailable,
        },
        Frame::Fail {
            req: 2,
            failure: ReadFailure::OutOfRange,
        },
    ];
    for frame in frames {
        assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
    }
}

#[test]
fn the_byte_layout_is_pinned() {
    let bytes = Frame::Read(ReadRequest {
        req: 0x0102_0304,
        entry: EntryId::from_bytes([0xee; 16]),
        offset: 0x0a0b,
        len: 0x10,
    })
    .encode();
    let mut expected = vec![1, 1, 2, 3, 4];
    expected.extend([0xee; 16]);
    expected.extend([0, 0, 0, 0, 0, 0, 0x0a, 0x0b]);
    expected.extend([0, 0, 0, 0x10]);
    assert_eq!(bytes, expected);
    assert_eq!(
        Frame::Fail {
            req: 1,
            failure: ReadFailure::Changed
        }
        .encode(),
        vec![3, 0, 0, 0, 1, 2]
    );
}

#[test]
fn a_frame_of_the_wrong_shape_is_refused_whole() {
    let good = Frame::Read(read(0, 10)).encode();
    assert_eq!(Frame::decode(&[]), Err(FrameError::Empty));
    assert_eq!(
        Frame::decode(&[9, 0, 0, 0, 0]),
        Err(FrameError::UnknownKind)
    );
    assert_eq!(
        Frame::decode(&good[..good.len() - 1]),
        Err(FrameError::BadLength)
    );
    let mut longer = good.clone();
    longer.push(0);
    assert_eq!(Frame::decode(&longer), Err(FrameError::BadLength));
    assert_eq!(
        Frame::decode(&Frame::Read(read(0, 0)).encode()),
        Err(FrameError::BadReadLength)
    );
    assert_eq!(
        Frame::decode(&Frame::Read(read(0, MAX_READ_LEN + 1)).encode()),
        Err(FrameError::BadReadLength)
    );
    assert_eq!(Frame::decode(&[2, 0, 0]), Err(FrameError::BadLength));
    let mut oversized = vec![2, 0, 0, 0, 0];
    oversized.resize(5 + MAX_READ_LEN as usize + 1, 0);
    assert_eq!(Frame::decode(&oversized), Err(FrameError::BadLength));
    assert_eq!(Frame::decode(&[3, 0, 0, 0, 0]), Err(FrameError::BadLength));
    assert_eq!(
        Frame::decode(&[3, 0, 0, 0, 0, 0]),
        Err(FrameError::UnknownFailure)
    );
    assert_eq!(
        Frame::decode(&[3, 0, 0, 0, 0, 9]),
        Err(FrameError::UnknownFailure)
    );
}

#[test]
fn a_read_stops_at_the_end_of_the_file() {
    let e = Entry {
        size: 1_000,
        ..entry(3)
    };
    assert_eq!(read(0, 10).span(&e), Ok(10));
    assert_eq!(read(995, 10).span(&e), Ok(5));
    assert_eq!(read(1_000, 10).span(&e), Ok(0));
    assert_eq!(read(1_001, 10).span(&e), Err(ReadFailure::OutOfRange));
    assert_eq!(read(u64::MAX, 10).span(&e), Err(ReadFailure::OutOfRange));
}
