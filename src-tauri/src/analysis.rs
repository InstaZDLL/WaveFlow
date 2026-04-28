//! Per-track audio analysis.
//!
//! Given a path on disk, decode the file with symphonia and compute:
//!
//! - **Peak**: max(|sample|) over the whole stream, in linear 0..1.
//! - **Loudness** (dB FS): `10·log10(mean(s²))` over a mono sum of
//!   the channels. Not BS.1770 K-weighted — the user-facing label
//!   says "Loudness" rather than "LUFS" to avoid implying conformity
//!   to the broadcast spec. Still good enough as a ReplayGain
//!   anchor and for relative comparisons inside one library.
//! - **ReplayGain**: `target − loudness_db` with the EBU/Apple
//!   target of −18 dB FS.
//! - **BPM**: autocorrelation peak on a coarse onset envelope.
//!   Works well for 4/4 tracks in the 60-200 BPM range; can land
//!   on half/double-time on heavily syncopated material — that's
//!   acceptable for an MVP shipped without aubio/essentia.
//!
//! Designed to run inside `spawn_blocking` because symphonia decode
//! is CPU-bound and we don't want the tokio runtime to stall.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const REPLAY_GAIN_TARGET_DB: f64 = -18.0;
/// Energy-envelope hop in samples at the analysis sample rate.
/// 11_025 / 256 ≈ 43 frames/s — enough resolution to land 60-200 BPM
/// peaks at 1 BPM granularity after autocorrelation refinement.
const ENVELOPE_HOP_SAMPLES_AT_11025: usize = 256;
/// Decimation target rate used internally for BPM detection. The
/// decode pipeline still produces native-rate stereo for the
/// loudness / peak pass; we just pick every Nth mono sum to feed
/// the BPM stage so the autocorrelation stays small.
const BPM_TARGET_RATE_HZ: u32 = 11_025;

#[derive(Debug, Clone, Default)]
pub struct AnalysisResult {
    /// Linear peak in 0..1.
    pub peak: f64,
    /// `10·log10(mean(s²))` of the mono-summed signal, in dB FS.
    pub loudness_db: f64,
    /// Suggested ReplayGain in dB. Positive = boost on quiet tracks,
    /// negative = attenuate on loud ones.
    pub replay_gain_db: f64,
    /// Estimated tempo in beats per minute, or `None` when the
    /// onset envelope had no usable autocorrelation peak (very short
    /// or near-silent tracks).
    pub bpm: Option<f64>,
}

/// Run the full analysis on a file. Returns the gathered metrics or
/// a string error so the caller (a tauri command, typically) can
/// surface it without dragging the symphonia error type through
/// the app's `AppError` enum.
pub fn analyze_file(path: &Path) -> Result<AnalysisResult, String> {
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

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable track".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec init: {e}"))?;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut src_channels: usize = 0;
    let mut src_rate: u32 = 0;

    // Loudness / peak accumulators on the mono-summed signal at the
    // file's native rate.
    let mut sum_squares: f64 = 0.0;
    let mut sample_count: u64 = 0;
    let mut peak: f64 = 0.0;

    // BPM envelope: every Nth mono sample, RMS over 256-sample hops.
    // We keep a running sum of squares for the current hop; when a
    // hop completes, we push its RMS into `envelope` and reset.
    let mut envelope: Vec<f32> = Vec::with_capacity(8192);
    let mut hop_sumsq: f32 = 0.0;
    let mut hop_count: usize = 0;
    // Decimation accumulator: stride between mono samples we
    // actually feed to the envelope. Computed once we know the
    // source rate.
    let mut decimate_stride: usize = 1;
    let mut decimate_counter: usize = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("next_packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::warn!(error = %e, "analysis: decode error, skipping packet");
                continue;
            }
            Err(e) => return Err(format!("decode: {e}")),
        };

        if sample_buf.is_none() {
            let spec = *decoded.spec();
            let capacity = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
            src_channels = spec.channels.count().max(1);
            src_rate = spec.rate.max(1);
            // Pre-decimate to ~11 kHz mono envelope for BPM. Feeding
            // the autocorrelation a 44.1 kHz envelope would balloon
            // memory and add no useful resolution.
            decimate_stride = (src_rate / BPM_TARGET_RATE_HZ).max(1) as usize;
        }
        let sb = sample_buf.as_mut().unwrap();
        sb.copy_interleaved_ref(decoded);
        let samples = sb.samples();

        // Walk frames: average channels, accumulate loudness + peak,
        // feed every Nth mono sum to the envelope.
        let frames = samples.len() / src_channels;
        for f in 0..frames {
            let base = f * src_channels;
            let mut sum: f32 = 0.0;
            for ch in 0..src_channels {
                sum += samples[base + ch];
            }
            let mono = sum / src_channels as f32;
            let abs = mono.abs() as f64;
            if abs > peak {
                peak = abs;
            }
            sum_squares += (mono as f64) * (mono as f64);
            sample_count += 1;

            if decimate_counter == 0 {
                hop_sumsq += mono * mono;
                hop_count += 1;
                if hop_count >= ENVELOPE_HOP_SAMPLES_AT_11025 {
                    let rms = (hop_sumsq / hop_count as f32).sqrt();
                    envelope.push(rms);
                    hop_sumsq = 0.0;
                    hop_count = 0;
                }
            }
            decimate_counter += 1;
            if decimate_counter >= decimate_stride {
                decimate_counter = 0;
            }
        }
    }

    if sample_count == 0 {
        return Err("no samples decoded".into());
    }

    let mean_sq = sum_squares / sample_count as f64;
    let loudness_db = if mean_sq > 0.0 {
        10.0 * mean_sq.log10()
    } else {
        -100.0
    };
    let replay_gain_db = REPLAY_GAIN_TARGET_DB - loudness_db;

    let bpm = estimate_bpm(&envelope, src_rate, decimate_stride);

    Ok(AnalysisResult {
        peak,
        loudness_db,
        replay_gain_db,
        bpm,
    })
}

/// Pick the dominant tempo from an onset-energy envelope by
/// auto-correlating it and picking the strongest lag in the
/// 60-200 BPM range.
///
/// `src_rate` and `decimate_stride` together tell us the effective
/// sample rate of the envelope: each envelope sample covers
/// `ENVELOPE_HOP_SAMPLES_AT_11025` mono samples taken every
/// `decimate_stride` source samples. The product gives the number
/// of source samples between consecutive envelope frames, hence
/// the seconds-per-frame.
fn estimate_bpm(envelope: &[f32], src_rate: u32, decimate_stride: usize) -> Option<f64> {
    if envelope.len() < 64 {
        return None;
    }

    // 1. High-pass differentiate the envelope to emphasize onsets
    //    (a positive jump in energy = a beat). Negative deltas are
    //    clamped to zero so the autocorrelation only sees attacks.
    let mut onset: Vec<f32> = Vec::with_capacity(envelope.len());
    onset.push(0.0);
    for i in 1..envelope.len() {
        onset.push((envelope[i] - envelope[i - 1]).max(0.0));
    }

    // 2. Subtract the running mean to remove DC bias before the
    //    autocorrelation, otherwise long signals correlate with
    //    themselves at every lag and the BPM peak gets buried.
    let mean: f32 = onset.iter().sum::<f32>() / onset.len() as f32;
    for v in onset.iter_mut() {
        *v -= mean;
    }

    // 3. Frame-rate of the envelope. Each frame represents
    //    `ENVELOPE_HOP_SAMPLES_AT_11025 * decimate_stride` source
    //    samples, so frames-per-second is the source rate divided
    //    by that product.
    let frames_per_sec =
        src_rate as f64 / (ENVELOPE_HOP_SAMPLES_AT_11025 as f64 * decimate_stride as f64);
    if frames_per_sec <= 0.0 {
        return None;
    }

    // 4. Autocorrelation over the BPM window. lag = frames between
    //    repeats of the energy pattern; bpm = 60 / (lag / fps).
    let min_bpm = 60.0;
    let max_bpm = 200.0;
    let max_lag = (frames_per_sec * 60.0 / min_bpm).round() as usize;
    let min_lag = (frames_per_sec * 60.0 / max_bpm).round() as usize;
    if max_lag >= onset.len() {
        return None;
    }

    let mut best_lag = 0usize;
    let mut best_score = f32::MIN;
    for lag in min_lag..=max_lag {
        let mut score: f32 = 0.0;
        // Sum of products onset[i] * onset[i+lag]. The cap on `i`
        // keeps the comparison length constant across lags so longer
        // lags aren't unfairly penalised.
        let limit = onset.len() - lag;
        for i in 0..limit {
            score += onset[i] * onset[i + lag];
        }
        // Normalise by the number of summed pairs so we compare
        // averages, not totals.
        score /= limit as f32;
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }

    if best_lag == 0 {
        return None;
    }
    let bpm = 60.0 * frames_per_sec / best_lag as f64;
    Some(bpm)
}
