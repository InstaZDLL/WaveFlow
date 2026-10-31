//! Decoding one packet of a stream into the output's rate and channel
//! count.

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::TrackType;

use crate::playback::resampler::Resampler;

use super::dsd::{interleave_stride, read_full_block, sub_stride_trim};
use super::{ActiveStream, StreamBackend, StreamExtent, DSD_READ_CHUNK, MAX_SKIPPED_PACKETS};

impl ActiveStream {
    /// One packet of [`Self::decode_next`], without the resampler's tail.
    pub(super) fn decode_packet(
        &mut self,
        out: &mut Vec<f32>,
        interleaved_scratch: &mut Vec<f32>,
        dst_sample_rate: u32,
        dst_channels: usize,
    ) -> Result<bool, String> {
        // Snapshot fields used after `&mut backend` so the borrow
        // checker doesn't complain when we reach for `self.resampler`
        // or `self.src_channels` inside the match arms.
        let mut skipped = 0usize;
        match &mut self.backend {
            StreamBackend::Symphonia {
                format,
                decoder,
                decoded_interleaved,
                symphonia_track_id,
                spec_captured,
                extent,
            } => loop {
                let packet = match format.next_packet() {
                    Ok(Some(p)) => p,
                    Ok(None) => return Ok(true),
                    // A new logical stream, the way a chained Ogg radio
                    // stream changes song. symphonia's contract: read the
                    // track list again and build the decoder anew, its rate
                    // or layout may differ. Ending the track here stopped
                    // such a stream at its first song change.
                    Err(SymphoniaError::ResetRequired) => {
                        let Some(track) = format.default_track(TrackType::Audio) else {
                            return Ok(true);
                        };
                        let Some(params) = track.codec_params.as_ref().and_then(|p| p.audio())
                        else {
                            return Ok(true);
                        };
                        let registry = crate::audio_format::opus::codecs();
                        *decoder = registry
                            .make_audio_decoder(params, &AudioDecoderOptions::default())
                            .map_err(|e| format!("codec init after a new stream: {e}"))?;
                        self.codec = registry.get_audio_decoder(params.codec).map(|registered| {
                            crate::audio_format::codec_label(registered.codec.info.short_name)
                        });
                        *symphonia_track_id = track.id;
                        // The first stream's declared end says nothing
                        // about this one.
                        *extent = StreamExtent::new(track.start_ts.get(), None, None);
                        *spec_captured = false;
                        // Chained streams can follow one another with no
                        // audio between them: they count toward the bound.
                        // Checked once the decoder is rebuilt, since the
                        // reader is already past the change.
                        skipped += 1;
                        if skipped >= MAX_SKIPPED_PACKETS {
                            return Ok(false);
                        }
                        continue;
                    }
                    Err(SymphoniaError::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof
                            && extent.ends_within_last_packet() =>
                    {
                        tracing::warn!(
                            decoded_to = extent.decoded_to,
                            declared_end = ?extent.declared_end,
                            "stray bytes after the last frame; ending the track there"
                        );
                        return Ok(true);
                    }
                    Err(e) => return Err(format!("next_packet: {e}")),
                };
                if packet.track_id != *symphonia_track_id {
                    skipped += 1;
                    if skipped >= MAX_SKIPPED_PACKETS {
                        return Ok(false);
                    }
                    continue;
                }
                let decoded = match decoder.decode(&packet) {
                    Ok(d) => d,
                    Err(SymphoniaError::DecodeError(e)) => {
                        tracing::warn!(error = %e, "decode error, skipping packet");
                        skipped += 1;
                        if skipped >= MAX_SKIPPED_PACKETS {
                            return Ok(false);
                        }
                        continue;
                    }
                    // The decoded audio's rate or layout is about to
                    // change: take it again from the next packet.
                    Err(SymphoniaError::ResetRequired) => {
                        *spec_captured = false;
                        skipped += 1;
                        if skipped >= MAX_SKIPPED_PACKETS {
                            return Ok(false);
                        }
                        continue;
                    }
                    Err(e) => return Err(format!("decode fatal: {e}")),
                };
                // After the decode, not before: a packet skipped as
                // undecodable is audio that did not play.
                extent.read(packet.pts.get(), packet.dur.get());
                self.packets_read.bytes += packet.data.len() as u64;
                self.packets_read.frames += decoded.frames() as u64;
                if !*spec_captured {
                    let spec = decoded.spec();
                    let (channels, rate) = (spec.channels().count(), spec.rate());
                    // After a new logical stream the same rate and layout
                    // keep the resampler, and what it holds.
                    if channels != self.src_channels || rate != self.src_sample_rate {
                        // What the replaced resampler holds is the end of
                        // the stream before; it plays ahead of this one.
                        // A no-op on a track's first packet (passthrough).
                        self.resampler
                            .finish(out)
                            .map_err(|e| format!("resampler tail: {e}"))?;
                        self.src_channels = channels;
                        self.src_sample_rate = rate;
                        let effective_rate = ((rate as f32) * self.playback_speed).max(1.0) as u32;
                        self.resampler =
                            Resampler::new(effective_rate, dst_sample_rate, dst_channels)
                                .map_err(|e| format!("resampler init: {e}"))?;
                    }
                    *spec_captured = true;
                }
                decoded.copy_to_vec_interleaved(decoded_interleaved);
                interleaved_scratch.clear();
                crate::playback::channels::convert_channels(
                    decoded_interleaved,
                    self.src_channels,
                    dst_channels,
                    interleaved_scratch,
                );
                self.resampler
                    .process(interleaved_scratch, out)
                    .map_err(|e| format!("resample: {e}"))?;
                return Ok(false);
            },
            StreamBackend::Dsd {
                file,
                layout,
                converter,
                bytes_read,
                dsd_scratch,
                pcm_src_scratch,
                // taps is only needed to rebuild the converter on reset;
                // the decode loop reuses the existing one.
                taps: _,
            } => {
                if *bytes_read >= layout.data_len_bytes {
                    return Ok(true);
                }
                // Pull a fixed-size DSD chunk, capped at whatever's
                // left in the data block. Critically, round the
                // request down to a whole number of interleave
                // strides — for DSF that's block_size × channels
                // (typically 8192 bytes). A truncated last chunk
                // would leave the converter mid-cycle: ch0 would
                // receive its block, ch1 would receive a partial
                // one, and at the next call (or EOF) ch0 / ch1
                // sample counts diverge → channels desynchronise →
                // grit/noise audible during the primary's tail
                // (and through the first ~half of the crossfade
                // because g_out is still > 0 there).
                let stride = interleave_stride(layout);
                let remaining = layout.data_len_bytes - *bytes_read;
                let raw_want = remaining.min(DSD_READ_CHUNK as u64);
                let aligned = raw_want
                    .checked_div(stride)
                    .map(|q| q * stride)
                    .unwrap_or(raw_want);
                // If the leftover at EOF is smaller than one stride,
                // we'd never make progress. In that case treat the
                // residual as EOF — losing < stride / bytes-per-ms
                // ≈ 12 ms is far less perceptible than the grit it
                // would otherwise produce.
                let want = if aligned == 0 {
                    *bytes_read = layout.data_len_bytes;
                    return Ok(true);
                } else {
                    aligned as usize
                };
                dsd_scratch.resize(want, 0);
                let read =
                    read_full_block(file, dsd_scratch).map_err(|e| format!("dsd read: {e}"))?;
                // A short return means the payload ended earlier than
                // the header claimed (truncated file / bad `data_len`),
                // so it can land mid-cycle. Drop the sub-stride
                // remainder rather than feed the converter bytes it
                // would attribute to the wrong channel.
                let read = sub_stride_trim(read, stride);
                if read == 0 {
                    *bytes_read = layout.data_len_bytes;
                    return Ok(true);
                }
                *bytes_read += read as u64;
                dsd_scratch.truncate(read);

                // Stage 1: DSD bitstream → PCM at source channel count
                // and the converter's output rate (DSD64 → 44.1 kHz).
                pcm_src_scratch.clear();
                converter.decode_block(dsd_scratch, pcm_src_scratch);
                if pcm_src_scratch.is_empty() {
                    // The FIR is still priming on the first call —
                    // tell the engine we made progress without
                    // emitting samples so it doesn't think we hit EOF.
                    return Ok(false);
                }

                // Stage 2: channel-convert from DSD's source channels
                // (typically 2) to the device's channel count (often
                // 8 on a 7.1 cpal output). Skipping this step makes
                // the resampler interpret a stereo buffer as 8-channel
                // and read 4× too fast — exactly what you hear as
                // "accelerated + pixelated" playback.
                interleaved_scratch.clear();
                crate::playback::channels::convert_channels(
                    pcm_src_scratch,
                    self.src_channels,
                    dst_channels,
                    interleaved_scratch,
                );

                // Stage 3: resample from converter rate to device rate.
                // Lazily built on first decode now that we know the
                // actual source rate.
                if matches!(self.resampler, Resampler::Passthrough)
                    && (converter.output_rate_hz != dst_sample_rate
                        || (self.playback_speed - 1.0).abs() > f32::EPSILON)
                {
                    self.src_sample_rate = converter.output_rate_hz;
                    let effective_rate =
                        ((converter.output_rate_hz as f32) * self.playback_speed).max(1.0) as u32;
                    self.resampler = Resampler::new(effective_rate, dst_sample_rate, dst_channels)
                        .map_err(|e| format!("dsd resampler init: {e}"))?;
                } else if self.src_sample_rate == 0 {
                    // Mark that we've seen the true source rate even
                    // when speed is 1.0 and rates match (passthrough),
                    // so a later rebuild_resampler call knows what to
                    // multiply by.
                    self.src_sample_rate = converter.output_rate_hz;
                }

                self.resampler
                    .process(interleaved_scratch, out)
                    .map_err(|e| format!("dsd resample: {e}"))?;
                Ok(false)
            }
            // A DoP stream is driven exclusively by `decode_dop_block`,
            // which produces raw 24-bit words with no resampling. Reaching
            // the PCM decode path for it is a wiring bug, not a runtime
            // condition — surface it rather than emit silence.
            StreamBackend::Dop { .. } => {
                Err("decode_next called on a DoP stream (use decode_dop_block)".into())
            }
        }
    }
}
