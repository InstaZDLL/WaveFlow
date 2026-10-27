//! The DSD side of a stream: opening a DSF / DFF file, the DoP blocks a
//! native DAC takes, and the byte arithmetic both share.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::audio_format::dsd::dop::DsdToDop;
use crate::audio_format::dsd::parser::{parse_dff, parse_dsf, DsdLayout};
use crate::audio_format::dsd::pcm::DsdToPcm;

use crate::playback::replay_gain::TrackGain;
use crate::playback::resampler::Resampler;

use super::{ActiveStream, StreamBackend, DSD_READ_CHUNK};

impl ActiveStream {
    /// Build an ActiveStream wrapping the DSD pipeline. The container
    /// is parsed up-front so we know the bit rate, channel count and
    /// where the bitstream starts; the DsdToPcm converter is built
    /// from the layout so its FIR ring buffers match the channel
    /// count. `dop` selects the native DoP passthrough backend over the
    /// DSD → PCM one (#495).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_dsd(
        path: &Path,
        ext: &str,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
        dsd_taps: usize,
        dop: bool,
    ) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let layout = match ext {
            "dsf" => parse_dsf(&mut file).map_err(|e| format!("dsf parse: {e}"))?,
            "dff" => parse_dff(&mut file).map_err(|e| format!("dff parse: {e}"))?,
            _ => return Err(format!("unexpected DSD extension: {ext}")),
        };
        // Position the file cursor at the start of the bitstream so
        // the first decode call streams from the right offset.
        file.seek(SeekFrom::Start(layout.data_offset))
            .map_err(|e| format!("dsd seek to data: {e}"))?;
        let src_channels = layout.channels.count() as usize;
        let container_duration_ms = Some(layout.duration_ms());
        let codec = Some(
            layout
                .dsd_rate_multiple()
                .map_or_else(|| "DSD".to_string(), |m| format!("DSD{m}")),
        );
        // DoP path (#495): repackage the raw bitstream instead of
        // decoding it. The engine has already re-opened the output at
        // this stream's DoP rate; here we just build the encoder and
        // let `decode_dop_block` feed 24-bit words straight to the ring.
        if dop {
            let encoder = Box::new(DsdToDop::new(&layout));
            return Ok(Self {
                backend: StreamBackend::Dop {
                    file,
                    layout,
                    encoder,
                    bytes_read: 0,
                    dsd_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                },
                // DoP is bit-exact: no resampler ever engages.
                resampler: Resampler::Passthrough,
                src_channels,
                track_id,
                duration_ms,
                source_type,
                source_id,
                // DoP is bit-exact — no gain stage runs on it at all.
                replay_gain: crate::playback::replay_gain::TrackGain::default(),
                src_sample_rate: 0,
                // DoP has already pinned the output's rate: asking for
                // another one on top would fight it.
                declared_sample_rate: None,
                playback_speed: 1.0,
                container_duration_ms,
                codec,
                tail_flushed: false,
            });
        }
        let converter = Box::new(DsdToPcm::new_with_taps(&layout, dsd_taps));
        // What this stream will hand the ring: the DSD bitstream's rate
        // divided down by the converter (44.1 kHz for DSD64, 88.2 for
        // DSD128, …), which is the rate the device should be asked for
        // when the user wants no resampling (#600).
        let declared_sample_rate = Some(converter.output_rate_hz);
        Ok(Self {
            backend: StreamBackend::Dsd {
                file,
                layout,
                converter,
                bytes_read: 0,
                dsd_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                pcm_src_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                taps: dsd_taps,
            },
            // Resampler from DSD output rate (44.1 kHz for DSD64,
            // 88.2 for DSD128, …) to dst is built lazily on the first
            // decode like the symphonia path — keeps the codepath
            // uniform.
            resampler: Resampler::Passthrough,
            src_channels,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain,
            src_sample_rate: 0,
            declared_sample_rate,
            playback_speed: 1.0,
            container_duration_ms,
            codec,
            tail_flushed: false,
        })
    }

    /// Decode one chunk of a DoP stream into interleaved 24-bit DoP
    /// words (marker in bits 23..16, two DSD bytes below), #495. No FIR,
    /// no resampler, no channel convert — the words go straight to the
    /// ring for a bit-perfect transport to the DAC. Returns `Ok(true)`
    /// at EOF, `Ok(false)` after a successful block.
    ///
    /// Only valid on a [`StreamBackend::Dop`]; other backends return an
    /// error (the caller picks `play_dop_track` vs `play_track` off the
    /// same DoP flag, so this never happens at runtime).
    pub fn decode_dop_block(&mut self, out: &mut Vec<u32>) -> Result<bool, String> {
        let StreamBackend::Dop {
            file,
            layout,
            encoder,
            bytes_read,
            dsd_scratch,
        } = &mut self.backend
        else {
            return Err("decode_dop_block called on a non-DoP stream".into());
        };

        if *bytes_read >= layout.data_len_bytes {
            return Ok(true);
        }
        // Round the read down to a whole interleave stride so the encoder
        // never pairs bytes across a channel boundary (same reasoning as
        // the DSD → PCM path).
        let stride = interleave_stride(layout);
        let remaining = layout.data_len_bytes - *bytes_read;
        let raw_want = remaining.min(DSD_READ_CHUNK as u64);
        let aligned = raw_want
            .checked_div(stride)
            .map(|q| q * stride)
            .unwrap_or(raw_want);
        if aligned == 0 {
            // Sub-stride residual at EOF — dropping < ~12 ms is
            // imperceptible and avoids a torn final frame.
            *bytes_read = layout.data_len_bytes;
            return Ok(true);
        }
        dsd_scratch.resize(aligned as usize, 0);
        let read = read_full_block(file, dsd_scratch).map_err(|e| format!("dop read: {e}"))?;
        // Same guard as the DSD → PCM path: a payload shorter than the
        // header claims can end mid-cycle, and a partial cycle would
        // pair bytes across channels.
        let read = sub_stride_trim(read, stride);
        if read == 0 {
            *bytes_read = layout.data_len_bytes;
            return Ok(true);
        }
        *bytes_read += read as u64;
        dsd_scratch.truncate(read);
        encoder.encode_block(dsd_scratch, out);
        Ok(false)
    }
}

/// Bytes making up one full interleave cycle of a DSD stream: a DSF
/// block per channel, or a single byte per channel for DFF. Every read
/// and every seek must land on a multiple of this, or the decoder starts
/// attributing one channel's bytes to another.
#[inline]
pub(super) fn interleave_stride(layout: &DsdLayout) -> u64 {
    match layout.block_interleave {
        Some(block) => (block as u64) * (layout.channels.count() as u64),
        None => layout.channels.count() as u64,
    }
}

/// Round a byte target down to a whole interleave stride and clamp it to
/// the payload — the shared half of both DSD seek paths.
#[inline]
pub(super) fn aligned_byte_offset(target: u64, stride: u64, data_len_bytes: u64) -> u64 {
    target
        .checked_div(stride)
        .map(|q| q * stride)
        .unwrap_or(target)
        .min(data_len_bytes)
}

/// Byte offset of `ms` into a DSD payload.
///
/// The division comes last on purpose. Folding it into a bytes-per-ms
/// constant first truncates the ratio — DSD64 stereo is 705.6 bytes/ms,
/// which becomes 705 — and the error is then multiplied by the seek
/// position: about 0.15 s short three minutes in, and growing from
/// there. u128 throughout so DSD512 stereo can't overflow.
#[inline]
pub(super) fn dsd_byte_offset_for_ms(layout: &DsdLayout, ms: u64) -> u64 {
    let bits_per_second = (layout.sample_rate_hz as u128) * (layout.channels.count() as u128);
    // Saturate rather than let the `as` truncate: a wrapped offset would
    // land somewhere plausible inside the payload and survive the clamp
    // in `aligned_byte_offset`, which only bounds the top end.
    ((ms as u128) * bits_per_second / 8 / 1000).min(u64::MAX as u128) as u64
}

/// Round a byte count down to a whole interleave stride — what's left of
/// a block that ended mid-cycle is unusable, not just short.
#[inline]
pub(super) fn sub_stride_trim(read: usize, stride: u64) -> usize {
    ((read as u64 / stride.max(1)) * stride) as usize
}

/// Read exactly `buf.len()` bytes unless the file genuinely ends first.
///
/// `Read::read` may come up short at any time, and for a DSD block that
/// is not a benign truncation: the requested size is a whole interleave
/// stride, so a fragment ends mid-cycle and every byte after it is
/// attributed to the wrong channel — audible as channel-swapped grit on
/// the PCM path, and as a torn frame against the marker cadence on the
/// DoP one. With this loop, a short return means real EOF, which is what
/// both callers already assume.
pub(super) fn read_full_block<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}
