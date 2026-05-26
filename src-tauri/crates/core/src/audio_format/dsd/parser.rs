//! DSF + DFF container parsers.
//!
//! Both formats wrap a raw 1-bit DSD bitstream with a tiny header
//! describing the bitrate (DSD64 = 2_822_400 Hz, DSD128 = 5_644_800,
//! …), the channel count, and the total sample count. We just need
//! enough of the layout to (a) know how many bits to feed the PCM
//! converter and (b) seek into the bitstream for play position.
//!
//! References:
//!   - DSF (Sony):
//!     <https://dsd-guide.com/sites/default/files/white-papers/DSFFileFormatSpec_E.pdf>
//!   - DFF (Philips, "DSD Interchange File Format"):
//!     <https://dsd-guide.com/sites/default/files/white-papers/DSDIFF_1.5_Spec.pdf>

use std::io::{Read, Seek, SeekFrom};

/// Errors surfaced from the parsers. Kept narrow — callers only care
/// about "is this a valid DSD container, and where's the bitstream?"
#[derive(Debug, thiserror::Error)]
pub enum DsdError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a DSD container (bad magic {0:?})")]
    BadMagic([u8; 4]),
    #[error("malformed: {0}")]
    Malformed(&'static str),
    #[error("unsupported sample rate {0} Hz")]
    UnsupportedRate(u32),
    #[error("unsupported channel count {0}")]
    UnsupportedChannels(u32),
}

/// Channel layout reported by the container. We map both DSF and DFF
/// channel-type codes onto the same enum so the PCM converter doesn't
/// have to know which container produced the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdChannels {
    Mono,
    Stereo,
    /// Anything beyond stereo (3.0, 5.0, 5.1, 7.1). We carry the raw
    /// count for the rare multichannel SACD rip — the engine will
    /// downmix to stereo using the same path it already uses for FLAC
    /// 5.1 sources.
    Multi(u8),
}

impl DsdChannels {
    pub fn count(&self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Multi(n) => *n,
        }
    }
}

/// Layout extracted from a container header. `data_offset` /
/// `data_len_bytes` give the byte slice the PCM decoder needs to
/// stream from the file; `sample_rate_hz` is the **DSD bit rate**
/// (i.e. DSD64 = 2_822_400), not the resulting PCM rate.
#[derive(Debug, Clone)]
pub struct DsdLayout {
    pub container: DsdContainer,
    pub channels: DsdChannels,
    pub sample_rate_hz: u32,
    /// Total DSD samples per channel (i.e. bits-per-channel divided
    /// by 1). Used to compute duration and seek.
    pub samples_per_channel: u64,
    pub data_offset: u64,
    pub data_len_bytes: u64,
    /// True when the data chunk is laid out interleaved per-block:
    ///   DSF stores `block_size_per_channel` bytes for ch0, then
    ///   ch1, etc., looping. DFF interleaves byte-per-byte.
    /// The PCM converter reads bits in different orders depending on
    /// this flag, so it's persisted here rather than re-derived.
    pub block_interleave: Option<u32>,
    /// Bit order inside each byte. DSF is little-endian (LSB first
    /// in time), DFF is big-endian (MSB first).
    pub lsb_first: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdContainer {
    Dsf,
    Dff,
}

impl DsdLayout {
    /// Convenience: the multiple of 44_100 Hz that the underlying
    /// rate represents (DSD64 → 64, DSD128 → 128, …). Returns `None`
    /// for unrecognised rates — the parser already validates so this
    /// is purely a label.
    pub fn dsd_rate_multiple(&self) -> Option<u32> {
        match self.sample_rate_hz {
            2_822_400 => Some(64),
            5_644_800 => Some(128),
            11_289_600 => Some(256),
            22_579_200 => Some(512),
            45_158_400 => Some(1024),
            _ => None,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        // ms = samples * 1000 / rate. u128 to avoid overflow on long
        // multichannel SACD rips at DSD512 (rare but legal).
        ((self.samples_per_channel as u128) * 1000 / (self.sample_rate_hz as u128)) as u64
    }
}

/// Parse a DSF container header (Sony format). The file starts with
/// a `DSD ` chunk pointing at metadata + the `fmt ` + `data` chunks.
pub fn parse_dsf<R: Read + Seek>(reader: &mut R) -> Result<DsdLayout, DsdError> {
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != b"DSD " {
        return Err(DsdError::BadMagic(magic));
    }

    // DSD chunk: 4 magic + 8 chunk size + 8 file size + 8 metadata
    // pointer. We skip everything but the metadata pointer (used by
    // the metadata reader landing in a follow-up commit).
    skip(reader, 8)?; // chunk size
    skip(reader, 8)?; // file size
    skip(reader, 8)?; // metadata offset (0 = none)

    // fmt chunk: 4 magic + 8 chunk size + payload
    let mut fmt_magic = [0u8; 4];
    reader.read_exact(&mut fmt_magic)?;
    if &fmt_magic != b"fmt " {
        return Err(DsdError::Malformed("missing fmt chunk"));
    }
    skip(reader, 8)?; // fmt chunk size (always 52 for v1)

    let _format_version = read_u32_le(reader)?;
    let _format_id = read_u32_le(reader)?;
    let channel_type = read_u32_le(reader)?;
    let channel_count = read_u32_le(reader)?;
    let sample_rate_hz = read_u32_le(reader)?;
    let _bits_per_sample = read_u32_le(reader)?; // always 1
    let total_samples_per_channel = read_u64_le(reader)?;
    let block_size_per_channel = read_u32_le(reader)?;
    let _reserved = read_u32_le(reader)?;

    validate_rate(sample_rate_hz)?;
    let channels = match channel_count {
        1 => DsdChannels::Mono,
        2 => DsdChannels::Stereo,
        n @ 3..=8 => DsdChannels::Multi(n as u8),
        n => return Err(DsdError::UnsupportedChannels(n)),
    };
    // DSF channel_type 1=mono, 2=stereo, 3=3.0, 4=quad, 5=4.0, 6=5.0,
    // 7=5.1. We don't surface the layout (front/rear) — only the
    // count is used downstream for downmixing.
    let _ = channel_type;

    // data chunk: 4 magic + 8 chunk size + payload
    let mut data_magic = [0u8; 4];
    reader.read_exact(&mut data_magic)?;
    if &data_magic != b"data" {
        return Err(DsdError::Malformed("missing data chunk"));
    }
    let data_chunk_size = read_u64_le(reader)?;
    let data_offset = reader.stream_position()?;
    // Chunk size includes the 12-byte chunk header per the spec, so
    // the payload is `chunk_size - 12`.
    let data_len_bytes = data_chunk_size.saturating_sub(12);

    Ok(DsdLayout {
        container: DsdContainer::Dsf,
        channels,
        sample_rate_hz,
        samples_per_channel: total_samples_per_channel,
        data_offset,
        data_len_bytes,
        block_interleave: Some(block_size_per_channel),
        lsb_first: true,
    })
}

/// Parse a DFF container header (Philips DSDIFF format). The file
/// starts with a `FRM8` chunk wrapping a `DSD ` form-type, followed
/// by `PROP/SND ` (properties) and `DSD ` (the bitstream).
pub fn parse_dff<R: Read + Seek>(reader: &mut R) -> Result<DsdLayout, DsdError> {
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != b"FRM8" {
        return Err(DsdError::BadMagic(magic));
    }
    let _form_size = read_u64_be(reader)?;
    let mut form_type = [0u8; 4];
    reader.read_exact(&mut form_type)?;
    if &form_type != b"DSD " {
        return Err(DsdError::Malformed("FRM8 form type != DSD"));
    }

    let mut sample_rate_hz: Option<u32> = None;
    let mut channel_count: Option<u32> = None;
    let mut data_offset: Option<u64> = None;
    let mut data_len_bytes: Option<u64> = None;

    // Walk the top-level chunks. We're only interested in `PROP` and
    // `DSD ` (the bitstream); everything else (DIIN, COMT, ID3 ) is
    // metadata handled elsewhere.
    loop {
        let mut chunk_id = [0u8; 4];
        if reader.read_exact(&mut chunk_id).is_err() {
            break;
        }
        let chunk_size = read_u64_be(reader)?;
        let chunk_start = reader.stream_position()?;
        match &chunk_id {
            b"PROP" => {
                let mut prop_type = [0u8; 4];
                reader.read_exact(&mut prop_type)?;
                if &prop_type != b"SND " {
                    return Err(DsdError::Malformed("PROP type != SND"));
                }
                // Sub-chunks inside PROP, until we've consumed the
                // payload.
                let prop_end = chunk_start + chunk_size;
                while reader.stream_position()? < prop_end {
                    let mut sub_id = [0u8; 4];
                    reader.read_exact(&mut sub_id)?;
                    let sub_size = read_u64_be(reader)?;
                    let sub_start = reader.stream_position()?;
                    match &sub_id {
                        b"FS  " => sample_rate_hz = Some(read_u32_be(reader)?),
                        b"CHNL" => {
                            let n = read_u16_be(reader)? as u32;
                            channel_count = Some(n);
                        }
                        _ => {}
                    }
                    // Always seek to the end of the sub-chunk so we
                    // tolerate fields we don't yet read.
                    let next = sub_start + sub_size;
                    reader.seek(SeekFrom::Start(pad_even(next)))?;
                }
            }
            b"DSD " => {
                data_offset = Some(chunk_start);
                data_len_bytes = Some(chunk_size);
            }
            _ => {}
        }
        let next = chunk_start + chunk_size;
        // IFF chunks are word-aligned: an odd byte count is followed
        // by a single padding byte.
        reader.seek(SeekFrom::Start(pad_even(next)))?;
    }

    let sample_rate_hz = sample_rate_hz.ok_or(DsdError::Malformed("DFF missing FS sub-chunk"))?;
    let channel_count = channel_count.ok_or(DsdError::Malformed("DFF missing CHNL sub-chunk"))?;
    let data_offset = data_offset.ok_or(DsdError::Malformed("DFF missing DSD chunk"))?;
    let data_len_bytes = data_len_bytes.ok_or(DsdError::Malformed("DFF missing DSD chunk"))?;

    validate_rate(sample_rate_hz)?;
    let channels = match channel_count {
        1 => DsdChannels::Mono,
        2 => DsdChannels::Stereo,
        n @ 3..=8 => DsdChannels::Multi(n as u8),
        n => return Err(DsdError::UnsupportedChannels(n)),
    };
    // Each byte holds 8 DSD samples; data_len is bytes for ALL
    // channels interleaved → samples per channel.
    let samples_per_channel = (data_len_bytes / channels.count() as u64) * 8;

    Ok(DsdLayout {
        container: DsdContainer::Dff,
        channels,
        sample_rate_hz,
        samples_per_channel,
        data_offset,
        data_len_bytes,
        block_interleave: None,
        lsb_first: false,
    })
}

fn validate_rate(rate: u32) -> Result<(), DsdError> {
    match rate {
        2_822_400 | 5_644_800 | 11_289_600 | 22_579_200 | 45_158_400 => Ok(()),
        n => Err(DsdError::UnsupportedRate(n)),
    }
}

fn skip<R: Read + Seek>(r: &mut R, n: i64) -> std::io::Result<()> {
    r.seek(SeekFrom::Current(n)).map(|_| ())
}

fn read_u16_be<R: Read>(r: &mut R) -> std::io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}
fn read_u32_le<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u32_be<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}
fn read_u64_le<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn read_u64_be<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}
fn pad_even(n: u64) -> u64 {
    if n & 1 == 1 {
        n + 1
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
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
}
