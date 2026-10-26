use super::*;

/// A 188-byte packet: header, optional adaptation stuffing, payload,
/// padded with 0xff the way a muxer stuffs (here through the payload,
/// which only the PSI parsers bound).
fn packet(pid: u16, start: bool, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![
        SYNC,
        (if start { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1f),
        pid as u8,
        0x10,
    ];
    p.extend_from_slice(payload);
    p.resize(PACKET, 0xff);
    p
}

/// A packet whose payload is exactly `payload`, the rest of the packet
/// taken by an adaptation field of stuffing — how a muxer ends a PES.
fn padded(pid: u16, start: bool, payload: &[u8]) -> Vec<u8> {
    let stuffing = PACKET - 4 - payload.len();
    let mut p = vec![
        SYNC,
        (if start { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1f),
        pid as u8,
        0x30,
        (stuffing - 1) as u8,
    ];
    if stuffing > 1 {
        p.push(0x00);
        p.resize(4 + stuffing, 0xff);
    }
    p.extend_from_slice(payload);
    assert_eq!(p.len(), PACKET);
    p
}

fn pat(pmt_pid: u16) -> Vec<u8> {
    // pointer, table 0, length 13, ts id, version, section numbers,
    // program 1 → pmt_pid, CRC (unchecked).
    vec![
        0,
        0x00,
        0xb0,
        13,
        0,
        1,
        0xc1,
        0,
        0,
        0,
        1,
        0xe0 | (pmt_pid >> 8) as u8,
        pmt_pid as u8,
        0,
        0,
        0,
        0,
    ]
}

fn pmt(streams: &[(u8, u16)]) -> Vec<u8> {
    let mut body = vec![0, 1, 0xc1, 0, 0, 0xe1, 0x00, 0xf0, 0x00];
    for (stream_type, pid) in streams {
        body.extend_from_slice(&[
            *stream_type,
            0xe0 | (pid >> 8) as u8,
            *pid as u8,
            0xf0,
            0x00,
        ]);
    }
    body.extend_from_slice(&[0, 0, 0, 0]);
    let mut s = vec![0, 0x02, 0xb0, body.len() as u8];
    s.extend_from_slice(&body);
    s
}

fn pes(es: &[u8]) -> Vec<u8> {
    let mut p = vec![0, 0, 1, 0xc0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
    p.extend_from_slice(es);
    p
}

#[test]
fn the_audio_stream_comes_out_whole_across_packets_and_calls() {
    let frame_a: Vec<u8> = (0..150u8).collect();
    let frame_b: Vec<u8> = (0..100u8).rev().collect();
    let mut ts = Vec::new();
    ts.extend(packet(0, true, &pat(0x1000)));
    ts.extend(packet(0x1000, true, &pmt(&[(0x1b, 0x100), (0x0f, 0x101)])));
    // A video packet, ignored.
    ts.extend(padded(
        0x100,
        true,
        &[0, 0, 1, 0xe0, 0, 0, 0x80, 0, 0, 9, 9],
    ));
    let first = pes(&frame_a);
    ts.extend(padded(0x101, true, &first));
    ts.extend(padded(0x101, false, &frame_b));

    let mut demuxer = TsDemuxer::new();
    let mut out = Vec::new();
    // Cut in the middle of a packet: the next call completes it.
    let cut = PACKET * 3 + 77;
    demuxer.feed(&ts[..cut], &mut out);
    demuxer.feed(&ts[cut..], &mut out);

    assert_eq!(demuxer.audio_kind(), Some(AudioKind::Adts));
    let mut want = frame_a.clone();
    want.extend(&frame_b);
    assert_eq!(out, want);
}

#[test]
fn a_program_map_that_renumbers_its_audio_is_followed() {
    let mut demuxer = TsDemuxer::new();
    let mut out = Vec::new();
    let mut first = Vec::new();
    first.extend(packet(0, true, &pat(0x1000)));
    first.extend(packet(0x1000, true, &pmt(&[(0x0f, 0x101)])));
    first.extend(padded(0x101, true, &pes(&[1, 1])));
    demuxer.feed(&first, &mut out);
    assert_eq!(out, [1, 1]);

    // The same map again keeps the stream, whatever else it lists.
    let mut again = Vec::new();
    again.extend(packet(0x1000, true, &pmt(&[(0x0f, 0x102), (0x0f, 0x101)])));
    again.extend(padded(0x101, true, &pes(&[2])));
    demuxer.feed(&again, &mut out);
    assert_eq!(out, [1, 1, 2]);

    // An encoder that restarted: new program map PID, new audio PID.
    // The old PID is gone, so the new one is taken.
    let mut restarted = Vec::new();
    restarted.extend(packet(0, true, &pat(0x1100)));
    restarted.extend(packet(0x1100, true, &pmt(&[(0x1b, 0x200), (0x03, 0x201)])));
    restarted.extend(padded(0x101, true, &pes(&[9])));
    restarted.extend(padded(0x201, true, &pes(&[3, 3])));
    demuxer.feed(&restarted, &mut out);
    assert_eq!(out, [1, 1, 2, 3, 3]);
    // And it says so: the decoder was opened for ADTS.
    assert_eq!(demuxer.audio_kind(), Some(AudioKind::Mpeg));

    // A map with no audio left chooses none.
    demuxer.feed(&packet(0x1100, true, &pmt(&[(0x1b, 0x200)])), &mut out);
    assert_eq!(demuxer.audio_kind(), None);
}

#[test]
fn mpeg_audio_is_named_and_a_lost_sync_is_found_again() {
    let mut ts = vec![0x00, 0x13];
    ts.extend(packet(0, true, &pat(0x20)));
    ts.extend(packet(0x20, true, &pmt(&[(0x03, 0x21)])));
    ts.extend(padded(0x21, true, &pes(&[1, 2, 3])));
    let mut demuxer = TsDemuxer::new();
    let mut out = Vec::new();
    demuxer.feed(&ts, &mut out);
    assert_eq!(demuxer.audio_kind(), Some(AudioKind::Mpeg));
    assert_eq!(out, [1, 2, 3]);
}

#[test]
fn packed_audio_loses_its_id3_tag() {
    let mut segment = b"ID3\x04\x00\x00\x00\x00\x00\x05".to_vec();
    segment.extend_from_slice(b"PRIV!");
    segment.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80]);
    let audio = strip_id3(&segment);
    assert_eq!(audio, [0xff, 0xf1, 0x50, 0x80]);
    assert_eq!(packed_kind(audio), Some(AudioKind::Adts));
    assert_eq!(packed_kind(&[0xff, 0xfb, 0x90]), Some(AudioKind::Mpeg));
    assert_eq!(strip_id3(&[1, 2, 3]), [1, 2, 3]);
    assert!(is_transport_stream(&packet(0, true, &[])));
    assert!(!is_transport_stream(audio));
}
