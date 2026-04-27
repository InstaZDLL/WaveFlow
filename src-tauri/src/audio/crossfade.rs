//! Crossfade DSP helpers.
//!
//! Apart from the equal-power gain curves, this module knows how to
//! open a "secondary" symphonia stream to the same f32 / dst_rate /
//! dst_channels pipeline used for the primary, so [`crate::audio::decoder`]
//! can mix two tracks into the SPSC ring during a fade window without
//! re-implementing the symphonia init dance twice.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::resampler::Resampler;

/// All per-stream decoder state, packaged so the primary stream can be
/// swapped out for the secondary one when a crossfade completes.
pub struct ActiveStream {
    pub format: Box<dyn FormatReader>,
    pub decoder: Box<dyn Decoder>,
    pub sample_buf: Option<SampleBuffer<f32>>,
    pub resampler: Resampler,
    pub src_channels: usize,
    pub symphonia_track_id: u32,
    pub track_id: i64,
    pub duration_ms: u64,
    pub source_type: String,
    pub source_id: Option<i64>,
}

impl ActiveStream {
    /// Open `path`, probe + build the codec, and stash everything
    /// needed to feed the resample/channel-convert pipeline. The
    /// resampler stays in `Passthrough` until the first packet is
    /// decoded — only then do we know the source sample rate.
    pub fn open(
        path: &Path,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
    ) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("probe: {e}"))?;
        let format = probed.format;
        let track_symphonia = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| "no decodable track".to_string())?;
        let symphonia_track_id = track_symphonia.id;
        let codec_params = track_symphonia.codec_params.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| format!("codec init: {e}"))?;

        Ok(Self {
            format,
            decoder,
            sample_buf: None,
            // Real resampler is built after the first packet (we need
            // the actual source rate, which AAC/M4A only reveals then).
            resampler: Resampler::Passthrough,
            src_channels: 0,
            symphonia_track_id,
            track_id,
            duration_ms,
            source_type,
            source_id,
        })
    }

    /// Decode one packet from this stream and append the resampled,
    /// channel-converted f32 samples (interleaved at `dst_channels`)
    /// into `out`. Returns `Ok(true)` on EOF, `Ok(false)` on a
    /// successful packet decode.
    ///
    /// `interleaved_scratch` is a caller-owned reusable buffer for the
    /// channel-conversion stage so we don't re-allocate per packet.
    pub fn decode_next(
        &mut self,
        out: &mut Vec<f32>,
        interleaved_scratch: &mut Vec<f32>,
        dst_sample_rate: u32,
        dst_channels: usize,
    ) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(true);
                }
                Err(SymphoniaError::ResetRequired) => return Ok(true),
                Err(e) => return Err(format!("next_packet: {e}")),
            };
            if packet.track_id() != self.symphonia_track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(e)) => {
                    tracing::warn!(error = %e, "decode error, skipping packet");
                    continue;
                }
                Err(e) => return Err(format!("decode fatal: {e}")),
            };
            if self.sample_buf.is_none() {
                let spec = *decoded.spec();
                let capacity = decoded.capacity() as u64;
                self.sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
                self.src_channels = spec.channels.count();
                let src_sample_rate = spec.rate;
                self.resampler =
                    Resampler::new(src_sample_rate, dst_sample_rate, dst_channels)
                        .map_err(|e| format!("resampler init: {e}"))?;
            }
            let sb = self.sample_buf.as_mut().unwrap();
            sb.copy_interleaved_ref(decoded);
            interleaved_scratch.clear();
            super::decoder::convert_channels(
                sb.samples(),
                self.src_channels,
                dst_channels,
                interleaved_scratch,
            );
            self.resampler
                .process(interleaved_scratch, out)
                .map_err(|e| format!("resample: {e}"))?;
            return Ok(false);
        }
    }
}

/// Equal-power fade gains for a normalized progress `t ∈ [0, 1]`.
/// Returns `(fade_out_gain, fade_in_gain)` where
/// `fade_out² + fade_in² = 1` so total power is preserved across the
/// mix — gives a noticeably less "scooped" mid-fade than linear gains.
#[inline]
pub fn equal_power_gains(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let angle = t * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}
