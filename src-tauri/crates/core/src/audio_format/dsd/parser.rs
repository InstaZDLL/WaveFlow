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
mod tests;
