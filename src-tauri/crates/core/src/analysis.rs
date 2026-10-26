//! Per-track audio analysis.
//!
//! Given a path on disk, decode the file with symphonia and compute:
//!
//! - **Peak**: `max(|sample|)` across **every channel**, in linear
//!   0..1. Not the peak of a mono downmix: a mix with the channels
//!   in opposition sums to near silence while its samples sit at full
//!   scale, and the anti-clip limiter downstream would then hand out a
//!   gain that clips. This is what a `REPLAYGAIN_TRACK_PEAK` tag means
//!   too, so ours and a tagger's are comparable.
//! - **Loudness** (LUFS): ITU-R BS.1770-4 — K-weighted, gated. See
//!   [`loudness`] for the algorithm and for why an unweighted RMS
//!   wasn't good enough once we started reading other people's tags.
//! - **ReplayGain**: `target − loudness_lufs` against the ReplayGain
//!   2.0 target of −18 LUFS. `None` when the track has no measurable
//!   loudness (silence, or shorter than one 400 ms gating block) —
//!   suggesting a gain there would mean boosting silence.
//! - **Tempo** and **energy**: from a spectral-flux onset detector
//!   over a decimated mono sum, with a confidence for the tempo. See
//!   [`rhythm`] for the algorithm and for what version 1 got wrong.
//! - **Beats**: where each one falls, placed on the same onsets at that
//!   tempo. See [`beats`].
//!
//! Designed to run inside `spawn_blocking` because symphonia decode
//! is CPU-bound and we don't want the tokio runtime to stall.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub mod bandwidth;
pub mod beats;
pub mod key;
pub mod loudness;
pub mod rhythm;
pub mod spectrogram;

use bandwidth::BandwidthMeter;
use key::KeyDetector;
use loudness::LoudnessMeter;
use rhythm::OnsetDetector;

/// Which generation of this module produced a given measurement.
///
/// Stored alongside every row [`analyze_file`] feeds, so a later build
/// can tell what a stored number was measured *by* rather than guessing
/// from its timestamp. Rows written before the column existed read as
/// `NULL`, which is the "older than the versioning" signal.
///
/// - `NULL` — unweighted RMS loudness over a mono sum, and a peak taken
///   over that same downmix. Both superseded by #545. The peak is the
///   dangerous one: a downmix under-reports an out-of-phase mix, so
///   clipping prevention computed from it hands out a gain that clips.
/// - `1` — BS.1770-4 K-weighted gated loudness, and a peak across every
///   channel.
/// - `2` — the tempo from a spectral-flux onset
///   detector over 40-240 BPM, with a confidence, and an energy measure
///   that loudness normalisation does not erase ([`rhythm`]). Loudness
///   and peak are measured as in `1`. Never shipped in a release.
/// - `3` — `2`, and the key with its confidence ([`key`]). Never shipped
///   in a release.
/// - `4` — the current pass: `3`, and where the beats fall ([`beats`]).
///   Nothing else moves, but a row without its beats needs the decode
///   again to get them.
///
/// **Bump this whenever a change here would move a stored number**, and
/// only then — the value exists so a sweep can find rows worth
/// re-measuring, so bumping it for a refactor costs every user a full
/// re-decode of their library.
pub const ANALYSIS_VERSION: i64 = 4;

/// ReplayGain 2.0 reference level. A track measured at exactly this
/// loudness gets a gain of 0 dB.
const REPLAY_GAIN_TARGET_LUFS: f64 = -18.0;
/// Rate the onset detector runs at. The decode pipeline still produces
/// native-rate audio for the loudness / peak pass; the detector gets the
/// mono sum averaged down to about this rate, which keeps everything up
/// to ~5 kHz and costs a quarter of the FFTs.
const RHYTHM_TARGET_RATE_HZ: u32 = 11_025;

#[derive(Debug, Clone, Default)]
pub struct AnalysisResult {
    /// Linear peak in 0..1, taken across every channel.
    pub peak: f64,
    /// Integrated loudness in LUFS (BS.1770-4). `None` for a track
    /// with nothing above the absolute gate.
    pub loudness_lufs: Option<f64>,
    /// Suggested ReplayGain in dB. Positive = boost on quiet tracks,
    /// negative = attenuate on loud ones. `None` travels with
    /// `loudness_lufs`.
    pub replay_gain_db: Option<f64>,
    /// Estimated tempo in beats per minute, or `None` when there was
    /// nothing to read one from (very short or near-silent tracks).
    pub bpm: Option<f64>,
    /// How far to trust `bpm`, 0 to 1. A tempo read on the edge of the
    /// search range scores 0: that is the search running out.
    pub bpm_confidence: Option<f64>,
    /// How intense the track is, 0 (calm) to 1, independent of its level.
    pub energy: Option<f64>,
    /// Where the beats fall, in seconds, in order; empty without a tempo.
    /// Stored with [`beats::encode`].
    pub beats: Vec<f64>,
    /// The tempo and energy with their parts, for tuning (the
    /// `analysis_bench` example). Nothing stores these.
    pub rhythm: rhythm::Rhythm,
    /// The key read from the audio, `None` when there was too little to
    /// read one from. See [`key`].
    pub key: Option<key::Key>,
    /// The chromagram the key was read from, for tuning. Not stored.
    pub chroma: Option<[f64; 12]>,
    /// Where the high frequencies stop. See [`bandwidth`].
    pub bandwidth: Option<bandwidth::Bandwidth>,
}

/// Run the full analysis on a file. Returns the gathered metrics or
/// a string error so the caller (a tauri command, typically) can
/// surface it without dragging the symphonia error type through
/// the app's `AppError` enum.
pub fn analyze_file(path: &Path) -> Result<AnalysisResult, String> {
    let OpenedAudio {
        mut format,
        track_id,
        mut decoder,
        ..
    } = open_audio(path)?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut spec_captured = false;
    let mut src_channels: usize = 0;

    // The BS.1770 meter needs the channel count and rate, so it's
    // built once the first decoded packet reveals the spec.
    let mut meter: Option<LoudnessMeter> = None;
    let mut frames_seen: u64 = 0;
    let mut peak: f64 = 0.0;

    // Onset detector over the mono sum, averaged down by `stride`: the
    // average of each group is a crude low-pass, so the high end folds
    // back far less than taking every Nth sample would. Built once the
    // first packet reveals the rate.
    let mut onsets: Option<OnsetDetector> = None;
    let mut keys: Option<KeyDetector> = None;
    // At the native rate, unlike the two above: the highs it looks for
    // are exactly what the decimation throws away.
    let mut bandwidth: Option<BandwidthMeter> = None;
    let mut decimate_stride: usize = 1;
    let mut group_sum: f32 = 0.0;
    let mut group_len: usize = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("next_packet: {e}")),
        };
        if packet.track_id != track_id {
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

        if !spec_captured {
            let spec = decoded.spec();
            src_channels = spec.channels().count().max(1);
            let src_rate = spec.rate().max(1);
            spec_captured = true;
            meter = Some(LoudnessMeter::with_channel_weights(
                src_rate,
                &channel_weights(spec.channels()),
            ));
            decimate_stride = (src_rate / RHYTHM_TARGET_RATE_HZ).max(1) as usize;
            let rate = f64::from(src_rate) / decimate_stride as f64;
            onsets = Some(OnsetDetector::new(rate));
            keys = Some(KeyDetector::new(rate));
            bandwidth = Some(BandwidthMeter::new(f64::from(src_rate), src_channels));
        }
        decoded.copy_to_vec_interleaved(&mut interleaved);
        let samples = &interleaved[..];

        // Loudness runs on the interleaved buffer as-is: BS.1770
        // filters each channel separately and only sums the powers
        // afterwards, so a mono downmix here would cancel exactly the
        // out-of-phase content the measurement is supposed to weigh.
        if let Some(meter) = meter.as_mut() {
            meter.push_interleaved(samples);
        }

        // Walk frames: peak across channels, and the mono sum averaged
        // down into the onset detector.
        let frames = samples.len() / src_channels;
        frames_seen += frames as u64;
        for f in 0..frames {
            let base = f * src_channels;
            let mut sum: f32 = 0.0;
            for ch in 0..src_channels {
                let sample = samples[base + ch];
                sum += sample;
                let abs = f64::from(sample.abs());
                if abs > peak {
                    peak = abs;
                }
            }
            let mono = sum / src_channels as f32;
            if let Some(meter) = bandwidth.as_mut() {
                meter.push_frame(&samples[base..base + src_channels]);
            }

            group_sum += mono;
            group_len += 1;
            if group_len >= decimate_stride {
                let sample = group_sum / group_len as f32;
                if let Some(onsets) = onsets.as_mut() {
                    onsets.push(sample);
                }
                if let Some(keys) = keys.as_mut() {
                    keys.push(sample);
                }
                group_sum = 0.0;
                group_len = 0;
            }
        }
    }

    if frames_seen == 0 {
        return Err("no samples decoded".into());
    }

    // `None` here is a real answer, not a failure: the file decoded,
    // it just has no loudness worth referencing a gain to.
    let loudness_lufs = meter.as_ref().and_then(LoudnessMeter::finish);
    let replay_gain_db = loudness_lufs.map(|lufs| REPLAY_GAIN_TARGET_LUFS - lufs);

    let rhythm = onsets.map(OnsetDetector::finish).unwrap_or_default();
    let (key, chroma) = keys.map(KeyDetector::finish).unwrap_or_default();

    Ok(AnalysisResult {
        peak,
        loudness_lufs,
        replay_gain_db,
        bpm: rhythm.tempo.map(|t| t.bpm),
        bpm_confidence: rhythm.tempo.map(|t| t.confidence),
        energy: rhythm.energy.map(|e| e.value),
        beats: rhythm.beats.clone(),
        rhythm,
        key,
        chroma,
        bandwidth: bandwidth.and_then(BandwidthMeter::finish),
    })
}

/// A file opened for a full decode: its container, the audio track and a
/// decoder from the app's one codec registry.
struct OpenedAudio {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    /// Samples per channel, when the container says.
    num_frames: Option<u64>,
}

/// Open `path` the way every decode in this module does: probed with its
/// extension as a hint, decoded through `audio_format::opus::codecs()` —
/// the registry playback and the scanner use too, so what plays is what
/// gets measured.
fn open_audio(path: &Path) -> Result<OpenedAudio, String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "no decodable track".to_string())?;
    let track_id = track.id;
    let num_frames = track.num_frames;
    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "track has no audio codec params".to_string())?;
    let decoder = crate::audio_format::opus::codecs()
        .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
        .map_err(|e| format!("codec init: {e}"))?;
    Ok(OpenedAudio {
        format,
        track_id,
        decoder,
        num_frames,
    })
}

/// Why [`spectrogram_file`] produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpectrogramError {
    /// `keep_going` said stop.
    Cancelled,
    Failed(String),
}

/// Decode `path` and record its spectrum over time, about `width` columns
/// wide (see [`spectrogram`]), with the fake-lossless measure taken over
/// the same frames.
///
/// `fallback_frames` is the length to spread the columns over when the
/// container does not give one (samples per channel, from the library's
/// duration). `keep_going(done, total)` is called after every packet with
/// the samples decoded so far and the expected total (`0` when unknown);
/// returning `false` stops the decode with [`SpectrogramError::Cancelled`].
///
/// Blocking and CPU-bound, like [`analyze_file`].
pub fn spectrogram_file(
    path: &Path,
    width: usize,
    fallback_frames: impl Fn(u32) -> Option<u64>,
    mut keep_going: impl FnMut(u64, u64) -> bool,
) -> Result<(spectrogram::Spectrogram, Option<bandwidth::Bandwidth>), SpectrogramError> {
    let OpenedAudio {
        mut format,
        track_id,
        mut decoder,
        num_frames,
    } = open_audio(path).map_err(SpectrogramError::Failed)?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut meter: Option<BandwidthMeter> = None;
    let mut channels: usize = 1;
    let mut total: u64 = 0;
    let mut done: u64 = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(SpectrogramError::Failed(format!("next_packet: {e}"))),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::warn!(error = %e, "spectrogram: decode error, skipping packet");
                continue;
            }
            Err(e) => return Err(SpectrogramError::Failed(format!("decode: {e}"))),
        };
        if meter.is_none() {
            let spec = decoded.spec();
            channels = spec.channels().count().max(1);
            let rate = spec.rate().max(1);
            total = num_frames.or_else(|| fallback_frames(rate)).unwrap_or(0);
            let per_column = spectrogram::frames_per_column(total, bandwidth::FRAME, width);
            // Twice the width at most, whatever length was declared.
            meter = Some(BandwidthMeter::with_spectrogram(
                f64::from(rate),
                channels,
                per_column,
                2 * width.max(1),
            ));
        }
        decoded.copy_to_vec_interleaved(&mut interleaved);
        if let Some(meter) = meter.as_mut() {
            for frame in interleaved.chunks_exact(channels) {
                meter.push_frame(frame);
            }
        }
        done += (interleaved.len() / channels) as u64;
        if !keep_going(done, total) {
            return Err(SpectrogramError::Cancelled);
        }
    }

    let (spectrogram, bandwidth) = meter
        .map(BandwidthMeter::finish_with_spectrogram)
        .unwrap_or((None, None));
    spectrogram
        .map(|s| (s, bandwidth))
        .ok_or_else(|| SpectrogramError::Failed("no samples decoded".into()))
}

/// BS.1770-4 §2 channel weights, in the decoder's own channel order.
///
/// The spec gives the surround channels `G = 1.41` (+1.5 dB) and
/// excludes LFE outright; everything else is `1.0`. A layout we can't
/// interpret — discrete, ambisonic, custom labels — gets all `1.0`,
/// which is what the measurement did for every layout before this.
///
/// The buffer index of a positioned channel is the number of
/// lower-numbered position bits that are set, so walking the mask from
/// bit 0 upwards yields the weights in interleaved order.
fn channel_weights(channels: &Channels) -> Vec<f64> {
    /// The +1.5 dB the spec applies to the surround channels.
    const SURROUND: f64 = 1.41;

    let Channels::Positioned(positions) = channels else {
        return vec![1.0; channels.count().max(1)];
    };

    let mut weights = Vec::with_capacity(positions.bits().count_ones() as usize);
    for bit in 0..u32::BITS {
        let Some(position) = Position::from_bits(1 << bit) else {
            continue;
        };
        if !positions.contains(position) {
            continue;
        }
        weights.push(match position {
            // Excluded, not attenuated: a loud LFE would otherwise
            // make a film mix measure far louder than it sounds.
            Position::LFE1 | Position::LFE2 => 0.0,
            Position::REAR_LEFT
            | Position::REAR_RIGHT
            | Position::SIDE_LEFT
            | Position::SIDE_RIGHT => SURROUND,
            _ => 1.0,
        });
    }
    if weights.is_empty() {
        weights.push(1.0);
    }
    weights
}
