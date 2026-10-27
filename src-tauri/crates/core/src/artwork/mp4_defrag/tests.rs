use super::*;

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = version_flags.to_be_bytes().to_vec();
    b.extend_from_slice(body);
    bx(kind, &b)
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// mvhd / mdhd version 0: created, modified, timescale, duration, tail.
fn hd(kind: &[u8; 4], timescale: u32, tail_len: usize) -> Vec<u8> {
    let mut body = words(&[0, 0, timescale, 0]);
    body.extend(std::iter::repeat(0u8).take(tail_len));
    full(kind, 0, &body)
}

fn moov(fragmented: bool) -> Vec<u8> {
    let tkhd = full(b"tkhd", 3, &cat(&[words(&[0, 0, 1, 0, 0]), vec![0; 60]]));
    let stsd = full(b"stsd", 0, &cat(&[words(&[1]), bx(b"avc1", &[7; 20])]));
    let empty = |kind: &[u8; 4], n: usize| full(kind, 0, &words(&vec![0; n]));
    let stbl = bx(
        b"stbl",
        &cat(&[
            stsd,
            empty(b"stts", 1),
            empty(b"stsc", 1),
            empty(b"stsz", 2),
            empty(b"stco", 1),
        ]),
    );
    let minf = bx(b"minf", &cat(&[full(b"vmhd", 1, &[0; 8]), stbl]));
    let hdlr = full(
        b"hdlr",
        0,
        &cat(&[words(&[0]), b"vide".to_vec(), vec![0; 13]]),
    );
    let mdia = bx(b"mdia", &cat(&[hd(b"mdhd", 1000, 4), hdlr, minf]));
    let trak = bx(b"trak", &cat(&[tkhd, mdia]));
    // trex: track 1, description 1, duration 40, size 0, non-sync.
    let mvex = bx(
        b"mvex",
        &full(b"trex", 0, &words(&[1, 1, 40, 0, SAMPLE_IS_NON_SYNC])),
    );
    let mut parts = vec![hd(b"mvhd", 1000, 80), trak];
    if fragmented {
        parts.push(mvex);
    }
    bx(b"moov", &cat(&parts))
}

/// A moof + mdat pair: samples of `sizes` bytes, each filled with its
/// own marker byte, the first one sync, all with composition offset 80.
fn fragment(sequence: u32, base_dts: u64, sizes: &[u32], marker: u8) -> Vec<u8> {
    let tfhd = full(b"tfhd", 0x02_0000, &words(&[1]));
    let tfdt = full(b"tfdt", 1 << 24, &base_dts.to_be_bytes());
    // data offset + first-sample flags + size + cts offset.
    let mut entries = Vec::new();
    for &size in sizes {
        entries.extend(words(&[size, 80]));
    }
    let trun_len = 8 + 4 + 4 + 4 + 4 + entries.len();
    let traf_len = 8 + tfhd.len() + tfdt.len() + trun_len;
    let moof_len = 8 + 16 + traf_len;
    let data_offset = (moof_len + 8) as u32;
    let trun = full(
        b"trun",
        0x01 | 0x04 | 0x200 | 0x800,
        &cat(&[words(&[sizes.len() as u32, data_offset, 0]), entries]),
    );
    let traf = bx(b"traf", &cat(&[tfhd, tfdt, trun]));
    let moof = bx(
        b"moof",
        &cat(&[full(b"mfhd", 0, &words(&[sequence])), traf]),
    );
    assert_eq!(moof.len(), moof_len);
    let mut media = Vec::new();
    for (i, &size) in sizes.iter().enumerate() {
        media.extend(std::iter::repeat(marker + i as u8).take(size as usize));
    }
    cat(&[moof, bx(b"mdat", &media)])
}

fn find_box<'a>(boxes: &[BoxRef<'a>], path: &[&[u8; 4]]) -> BoxRef<'a> {
    let found = child(boxes, path[0]).unwrap_or_else(|| panic!("no {:?}", path[0]));
    if path.len() == 1 {
        found
    } else {
        find_box(&children(&found).unwrap(), &path[1..])
    }
}

fn table(b: &BoxRef, skip: usize) -> Vec<u32> {
    b.body[4 + skip..]
        .chunks(4)
        .map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

#[test]
fn an_ordinary_mp4_is_left_alone() {
    let file = cat(&[
        bx(b"ftyp", b"isom\0\0\0\0"),
        moov(false),
        bx(b"mdat", &[1, 2, 3]),
    ]);
    assert!(defragment(&file).unwrap().is_none());
    assert!(!is_fragmented(&mut std::io::Cursor::new(&file)).unwrap());
}

#[test]
fn a_large_box_before_the_index_does_not_hide_it() {
    // Two megabytes of `free` ahead of `moov`: the walk seeks over it.
    let file = cat(&[
        bx(b"ftyp", b"iso6\0\0\0\0"),
        bx(b"free", &vec![0; 2 * 1024 * 1024]),
        moov(true),
        fragment(1, 0, &[5, 3], 0x10),
    ]);
    assert!(is_fragmented(&mut std::io::Cursor::new(&file)).unwrap());
    // A moof after a moov without mvex still counts, as in defragment.
    let loose = cat(&[moov(false), fragment(1, 0, &[5], 0x10)]);
    assert!(is_fragmented(&mut std::io::Cursor::new(&loose)).unwrap());
    // A truncated header is an answer too, not an error.
    assert!(!is_fragmented(&mut std::io::Cursor::new(&file[..12])).unwrap());
}

#[test]
fn fragments_become_one_indexed_mdat() {
    let file = cat(&[
        bx(b"ftyp", b"iso6\0\0\0\0"),
        moov(true),
        // Starts ten seconds into its own timeline, like Apple's HEVC.
        fragment(1, 10_000, &[5, 3, 4], 0x10),
        fragment(2, 10_120, &[6, 2], 0x20),
    ]);
    assert!(is_fragmented(&mut std::io::Cursor::new(&file)).unwrap());
    let out = defragment(&file).unwrap().expect("fragmented input");
    assert!(!is_fragmented(&mut std::io::Cursor::new(&out)).unwrap());
    assert!(defragment(&out).unwrap().is_none());

    let top = parse_boxes(&out, 0).unwrap();
    let kinds: Vec<_> = top.iter().map(|b| b.kind).collect();
    assert_eq!(kinds, [*b"ftyp", *b"moov", *b"mdat"]);
    let moov = children(&top[1]).unwrap();
    assert!(child(&moov, b"mvex").is_none());

    let stbl = [b"trak", b"mdia", b"minf", b"stbl"];
    let at = |kind: &[u8; 4]| {
        let mut path = stbl.to_vec();
        path.push(kind);
        find_box(&moov, &path)
    };
    let sizes = table(&at(b"stsz"), 0);
    assert_eq!(sizes, [0, 5, 5, 3, 4, 6, 2]);
    assert_eq!(table(&at(b"stts"), 0), [1, 5, 40]);
    assert_eq!(table(&at(b"stss"), 0), [2, 1, 4]);
    assert_eq!(table(&at(b"ctts"), 0), [1, 5, 80]);
    assert_eq!(table(&at(b"stsc"), 0), [2, 1, 3, 1, 2, 2, 1]);

    // Every sample's bytes sit where the index says.
    let offsets = table(&at(b"stco"), 0);
    assert_eq!(offsets[0], 2);
    let mut sample = 0;
    for (chunk, marker, count) in [(0, 0x10u8, 3), (1, 0x20u8, 2)] {
        let mut at = offsets[1 + chunk] as usize;
        for i in 0..count {
            let size = sizes[2 + sample] as usize;
            assert!(out[at..at + size].iter().all(|&b| b == marker + i as u8));
            at += size;
            sample += 1;
        }
    }

    // Moved to zero; the composition offset delays the first frame by
    // 80, which the edit list skips.
    let elst = find_box(&moov, &[b"trak", b"edts", b"elst"]);
    let media_time = i64::from_be_bytes(elst.body[16..24].try_into().unwrap());
    assert_eq!(media_time, 80);
    let mdhd = find_box(&moov, &[b"trak", b"mdia", b"mdhd"]);
    let duration = u64::from_be_bytes(mdhd.body[24..32].try_into().unwrap());
    assert_eq!(duration, 200);
}

#[test]
fn an_impossible_sample_count_is_refused() {
    // Data offset only: no per-sample field bounds the count by the box,
    // so it has to be bounded by the file.
    let tfhd = full(b"tfhd", 0x02_0000 | 0x10, &words(&[1, 4]));
    let trun = full(b"trun", 0x01, &words(&[u32::MAX, 0]));
    let traf = bx(b"traf", &cat(&[tfhd, trun]));
    let moof = bx(b"moof", &cat(&[full(b"mfhd", 0, &words(&[1])), traf]));
    let file = cat(&[bx(b"ftyp", b"iso6\0\0\0\0"), moov(true), moof]);
    assert!(defragment(&file).is_err());
}

#[test]
fn runs_sharing_their_data_are_refused() {
    // Each run fits in the file on its own; three of them over the same
    // bytes would index far more samples than the file holds.
    let tfhd = full(b"tfhd", 0x02_0000 | 0x10, &words(&[1, 1]));
    let run = || full(b"trun", 0x01, &words(&[2000, 0]));
    let traf = bx(b"traf", &cat(&[tfhd, run(), run(), run()]));
    let moof = bx(b"moof", &cat(&[full(b"mfhd", 0, &words(&[1])), traf]));
    let file = cat(&[
        bx(b"ftyp", b"iso6\0\0\0\0"),
        moov(true),
        moof,
        bx(b"mdat", &[0; 2400]),
    ]);
    assert!(file.len() < 2 * 2000);
    assert!(defragment(&file).is_err());
}

#[test]
fn a_truncated_fragment_is_refused() {
    let mut file = cat(&[
        bx(b"ftyp", b"iso6\0\0\0\0"),
        moov(true),
        fragment(1, 0, &[50], 1),
    ]);
    file.truncate(file.len() - 10);
    assert!(defragment(&file).is_err());
}

/// Point at a real file to check a conversion by hand:
/// `WF_DEFRAG_IN=in.mp4 WF_DEFRAG_OUT=out.mp4 cargo test … -- --ignored`.
#[test]
#[ignore]
fn convert_a_real_file() {
    let input = std::fs::read(std::env::var("WF_DEFRAG_IN").unwrap()).unwrap();
    let out = defragment(&input).unwrap().expect("fragmented input");
    std::fs::write(std::env::var("WF_DEFRAG_OUT").unwrap(), out).unwrap();
}
