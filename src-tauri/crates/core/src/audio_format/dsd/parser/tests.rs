use super::*;
use std::io::Cursor;

/// Minimal valid DSF: DSD chunk + fmt chunk + data chunk. Stereo
/// DSD64, 1 second of audio = 2_822_400 samples per channel.
fn synth_dsf(samples_per_channel: u64, channels: u32, rate: u32) -> Vec<u8> {
    let mut out = Vec::new();
    // DSD chunk
    out.extend_from_slice(b"DSD ");
    out.extend_from_slice(&28u64.to_le_bytes()); // chunk size
    out.extend_from_slice(&0u64.to_le_bytes()); // file size (unused in test)
    out.extend_from_slice(&0u64.to_le_bytes()); // metadata offset
                                                // fmt chunk (52 bytes total payload)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&52u64.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // format version
    out.extend_from_slice(&0u32.to_le_bytes()); // format id (raw DSD)
    out.extend_from_slice(&2u32.to_le_bytes()); // channel type (stereo)
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // bits per sample
    out.extend_from_slice(&samples_per_channel.to_le_bytes());
    out.extend_from_slice(&4096u32.to_le_bytes()); // block size per channel
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
                                                // data chunk: chunk_size includes 12-byte header per spec
    let payload_bytes = (samples_per_channel / 8) * channels as u64;
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(payload_bytes + 12).to_le_bytes());
    out.extend(std::iter::repeat(0xAA).take(payload_bytes as usize));
    out
}

#[test]
fn dsf_parses_stereo_dsd64() {
    let bytes = synth_dsf(2_822_400, 2, 2_822_400);
    let layout = parse_dsf(&mut Cursor::new(bytes)).expect("parse");
    assert_eq!(layout.container, DsdContainer::Dsf);
    assert_eq!(layout.channels, DsdChannels::Stereo);
    assert_eq!(layout.sample_rate_hz, 2_822_400);
    assert_eq!(layout.dsd_rate_multiple(), Some(64));
    assert_eq!(layout.samples_per_channel, 2_822_400);
    assert_eq!(layout.duration_ms(), 1000);
    assert!(layout.lsb_first);
    assert_eq!(layout.block_interleave, Some(4096));
}

#[test]
fn dsf_parses_dsd128() {
    let bytes = synth_dsf(5_644_800, 2, 5_644_800);
    let layout = parse_dsf(&mut Cursor::new(bytes)).expect("parse");
    assert_eq!(layout.dsd_rate_multiple(), Some(128));
    assert_eq!(layout.duration_ms(), 1000);
}

#[test]
fn dsf_rejects_bad_magic() {
    let bytes = b"NOPE\0\0\0\0".to_vec();
    let err = parse_dsf(&mut Cursor::new(bytes)).unwrap_err();
    assert!(matches!(err, DsdError::BadMagic(_)));
}

#[test]
fn dsf_rejects_unsupported_rate() {
    let bytes = synth_dsf(1000, 2, 1_411_200);
    let err = parse_dsf(&mut Cursor::new(bytes)).unwrap_err();
    assert!(matches!(err, DsdError::UnsupportedRate(1_411_200)));
}

/// Minimal DFF: FRM8 + DSD form + PROP/SND with FS + CHNL +
/// DSD bitstream chunk.
fn synth_dff(samples_per_channel: u64, channels: u16, rate: u32) -> Vec<u8> {
    let mut prop_payload = Vec::new();
    prop_payload.extend_from_slice(b"SND ");
    // FS sub-chunk (4 byte payload)
    prop_payload.extend_from_slice(b"FS  ");
    prop_payload.extend_from_slice(&4u64.to_be_bytes());
    prop_payload.extend_from_slice(&rate.to_be_bytes());
    // CHNL sub-chunk (2 byte payload, padded to 8 for word
    // alignment)
    prop_payload.extend_from_slice(b"CHNL");
    prop_payload.extend_from_slice(&2u64.to_be_bytes());
    prop_payload.extend_from_slice(&channels.to_be_bytes());

    let data_bytes = (samples_per_channel / 8) * channels as u64;
    let mut data_chunk = Vec::new();
    data_chunk.extend_from_slice(b"DSD ");
    data_chunk.extend_from_slice(&data_bytes.to_be_bytes());
    data_chunk.extend(std::iter::repeat(0xAA).take(data_bytes as usize));

    let mut prop_chunk = Vec::new();
    prop_chunk.extend_from_slice(b"PROP");
    prop_chunk.extend_from_slice(&(prop_payload.len() as u64).to_be_bytes());
    prop_chunk.extend_from_slice(&prop_payload);

    let mut form_payload = Vec::new();
    form_payload.extend_from_slice(b"DSD ");
    form_payload.extend_from_slice(&prop_chunk);
    form_payload.extend_from_slice(&data_chunk);

    let mut out = Vec::new();
    out.extend_from_slice(b"FRM8");
    out.extend_from_slice(&(form_payload.len() as u64).to_be_bytes());
    out.extend_from_slice(&form_payload);
    out
}

#[test]
fn dff_parses_stereo_dsd64() {
    let bytes = synth_dff(2_822_400, 2, 2_822_400);
    let layout = parse_dff(&mut Cursor::new(bytes)).expect("parse");
    assert_eq!(layout.container, DsdContainer::Dff);
    assert_eq!(layout.channels, DsdChannels::Stereo);
    assert_eq!(layout.sample_rate_hz, 2_822_400);
    assert_eq!(layout.duration_ms(), 1000);
    assert!(!layout.lsb_first);
    assert!(layout.block_interleave.is_none());
}

#[test]
fn dff_rejects_bad_magic() {
    let bytes = b"NOPE\0\0\0\0\0\0\0\0\0\0\0\0".to_vec();
    let err = parse_dff(&mut Cursor::new(bytes)).unwrap_err();
    assert!(matches!(err, DsdError::BadMagic(_)));
}

#[test]
fn pad_even_aligns_odd_to_next_word_boundary() {
    assert_eq!(pad_even(0), 0);
    assert_eq!(pad_even(7), 8);
    assert_eq!(pad_even(8), 8);
    assert_eq!(pad_even(9), 10);
}
