//! Channel conversion between a track's layout and the output's.

/// Convert interleaved `src_chans`-wide samples to interleaved
/// `dst_chans`-wide samples in `out`. Simple fixed rules:
/// - Equal counts: copy verbatim
/// - mono (1) → stereo (2): duplicate
/// - mono (1) → more than two: front left and right, the rest silent
/// - stereo (2) → mono (1): average
/// - ≥3 → 2: ITU-R BS.775 Lo/Ro downmix ([`downmix_frame_to_stereo`])
/// - anything else: take the first `min(src, dst)` channels, pad zeros
pub fn convert_channels(input: &[f32], src_chans: usize, dst_chans: usize, out: &mut Vec<f32>) {
    if src_chans == 0 || dst_chans == 0 {
        return;
    }
    if src_chans == dst_chans {
        out.extend_from_slice(input);
        return;
    }
    let frames = input.len() / src_chans;
    match (src_chans, dst_chans) {
        (1, 2) => {
            out.reserve(frames * 2);
            for &s in input.iter().take(frames) {
                out.push(s);
                out.push(s);
            }
        }
        // Mono onto a 5.1 or 7.1 output: the front pair, as on a stereo
        // one. Through the fallback below it came out of front left
        // alone.
        (1, n) if n > 2 => {
            out.reserve(frames * n);
            for &s in input.iter().take(frames) {
                out.push(s);
                out.push(s);
                out.resize(out.len() + n - 2, 0.0);
            }
        }
        (2, 1) => {
            out.reserve(frames);
            for f in 0..frames {
                out.push(0.5 * (input[f * 2] + input[f * 2 + 1]));
            }
        }
        // Any multichannel source → stereo Lo/Ro (ITU-R BS.775).
        // Per-frame fold so 3.0 / quad / 5.0 / 5.1 / 6.1 / 7.1 each
        // keep their centre + surrounds instead of being truncated to
        // the front pair.
        (s, 2) if s >= 3 => {
            out.reserve(frames * 2);
            for f in 0..frames {
                let frame = &input[f * s..f * s + s];
                let (lo, ro) = downmix_frame_to_stereo(frame);
                out.push(lo);
                out.push(ro);
            }
        }
        _ => {
            // Fallback: truncate or zero-pad.
            out.reserve(frames * dst_chans);
            for f in 0..frames {
                for ch in 0..dst_chans {
                    let v = if ch < src_chans {
                        input[f * src_chans + ch]
                    } else {
                        0.0
                    };
                    out.push(v);
                }
            }
        }
    }
}

/// Fold one interleaved multichannel frame down to a stereo (Lo, Ro)
/// pair following ITU-R BS.775. The centre and every surround are
/// summed in at −3 dB (×0.707); the LFE is discarded, as is standard
/// for a stereo fold-down.
///
/// Speaker positions are inferred from the channel **count** because
/// neither the FLAC path nor the DSD parser surfaces explicit channel
/// labels — `frame` is assumed to be in the WAVE/SMPTE order shared by
/// both (`FL FR FC LFE BL BR …`). The 4-channel case is treated as
/// quadraphonic (`FL FR BL BR`), the common 4-ch music layout; a rare
/// 3.1 source would fold its C/LFE as surrounds instead, which is still
/// preferable to dropping them. Summed peaks can exceed unity — the
/// player's final clamp to `[-1.0, 1.0]` is the backstop before the DAC.
#[inline]
fn downmix_frame_to_stereo(frame: &[f32]) -> (f32, f32) {
    const K: f32 = 0.707; // −3 dB
    let l = frame[0];
    let r = frame[1];
    match frame.len() {
        // 3.0 — FL FR FC
        3 => (l + K * frame[2], r + K * frame[2]),
        // quad 4.0 — FL FR BL BR (no centre)
        4 => (l + K * frame[2], r + K * frame[3]),
        // 5.0 — FL FR FC BL BR
        5 => (
            l + K * frame[2] + K * frame[3],
            r + K * frame[2] + K * frame[4],
        ),
        // 5.1 — FL FR FC LFE BL BR (LFE dropped)
        6 => (
            l + K * frame[2] + K * frame[4],
            r + K * frame[2] + K * frame[5],
        ),
        // 6.1 — FL FR FC LFE BL BR BC (rear-centre folded into both)
        7 => {
            let bc = K * frame[6];
            (
                l + K * frame[2] + K * frame[4] + bc,
                r + K * frame[2] + K * frame[5] + bc,
            )
        }
        // 7.1 and beyond — FL FR FC LFE BL BR SL SR (… plus any further
        // surround / height channels). The centre (idx 2) sums into
        // both, the LFE (idx 3) is dropped, and every channel from idx 4
        // on folds alternately left / right by parity so no channel is
        // ever discarded — matching the explicit 7.1 result and
        // extending it to wider Atmos-style beds.
        _ => {
            let mut lo = l + K * frame[2];
            let mut ro = r + K * frame[2];
            for (i, &s) in frame.iter().enumerate().skip(4) {
                if i % 2 == 0 {
                    lo += K * s;
                } else {
                    ro += K * s;
                }
            }
            (lo, ro)
        }
    }
}

#[cfg(test)]
mod tests;
