//! Render AutoMix transitions between two files to WAV, to listen to them.
//!
//! ```text
//! cargo run --release -p waveflow-core --features playback --example automix_render -- <outgoing> <incoming> <out dir> [--cap-ms 12000] [--no-gain] [--trim-db dB] [--entry-beat N]
//! ```
//!
//! Both files are analysed with the app's own code (`analysis::analyze_file`,
//! so the same beat grid the library stores) and decoded through the app's
//! codec registry. Each style is written as `<out dir>/<n>-<style>.wav`, from
//! a few seconds before the transition to a few seconds after it, with
//! `plan.txt` beside them saying where each one starts and ends. ReplayGain
//! (track) is applied to both, as it would be with that setting on, unless
//! `--no-gain`.
//!
//! The styles:
//!
//! - `current` — what AutoMix does today: `automix::plan`, no stretching,
//!   or the plain crossfade over the last `cap` of the file when it falls
//!   back;
//! - `tempo-in` — the incoming track starts at the outgoing one's tempo and
//!   glides back to its own over 16 beats once the fade is over;
//! - `tempo-in-phase-corrected` — the same, the incoming track then moved by
//!   the phase that best matches the two tracks' kick drums within a beat
//!   ([`phase_shift`]), against a tracker that put one track's beats off
//!   the other's (an experiment: the measure is weak on real music);
//! - `tempo-out` — the outgoing track glides to the incoming one's tempo
//!   over the 8 beats before the fade, and the incoming plays as it is;
//! - `echo` — `tempo-in`, with the outgoing track cut on its last beat and
//!   that beat echoing on under the incoming track;
//! - `loop` — `tempo-in`, with the last 2 beats of the outgoing track
//!   looped four times while it fades;
//! - `stutter` — the same, the loop halved as it goes (2, 1, ½, ¼ beat);
//! - `filter` — `tempo-in`, with a high-pass rising on the outgoing track
//!   and a low-pass opening on the incoming one;
//! - `band-swap` — modelled on a measured reference transition ([`band_swap`]):
//!   the outgoing track glides to the incoming tempo well before, the
//!   incoming one comes in native on a phrase past its intro, at a break of
//!   the outgoing one, and the two hand over band by band, highs first,
//!   the outgoing bass cut on its last beat and its ring-out dropped;
//! - `band-swap-forced-entry` — the same entering on the incoming beat
//!   `--entry-beat` names, only when it is given.
//!
//! `--trim-db` adds a gain to both tracks, to match the level of a recording
//! to compare against. `AUTOMIX_SECTIONS=1` prints each beat's level and the
//! structural novelty peaks on stderr, `AUTOMIX_BANDS_CHECK=1` the band
//! split's response at full gain.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use waveflow_core::analysis::analyze_file;
use waveflow_core::automix::{self, Grid, TrackBeats};
use waveflow_core::playback::stretch::Stretcher;

/// Seconds of the outgoing track alone before the transition, and after it.
const PRE_S: f64 = 10.0;
const POST_S: f64 = 10.0;

/// The beats the grids are fitted on, and the fade lengths tried: those of
/// `automix`.
const FIT_BEATS: usize = automix::FIT_BEATS;
const FADE_BEATS: [u32; 4] = [32, 16, 8, 4];

/// The widest tempo gap the stretched styles take, either way.
const MAX_STRETCH: f64 = 0.08;

/// Incoming beats the tempo glides back over, after the fade.
const RAMP_IN_BEATS: f64 = 16.0;
/// Outgoing beats the tempo glides over, before the fade.
const RAMP_OUT_BEATS: f64 = 8.0;

/// Source frames the stretcher is fed at one tempo.
const BLOCK: usize = 256;

struct Track {
    path: PathBuf,
    rate: u32,
    channels: usize,
    pcm: Vec<f32>,
    beats: Vec<f64>,
    bpm: Option<f64>,
    energy: Option<f64>,
    gain: f32,
}

impl Track {
    fn frames(&self) -> usize {
        self.pcm.len() / self.channels
    }
    fn duration(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }
    fn beats_view(&self) -> TrackBeats<'_> {
        TrackBeats {
            beats: &self.beats,
            duration_ms: (self.duration() * 1000.0) as u64,
        }
    }
}

fn decode(path: &Path) -> Result<(u32, usize, Vec<f32>), String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = waveflow_core::audio_format::probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio params")?;
    let mut decoder = waveflow_core::audio_format::opus::codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;
    let mut pcm = Vec::new();
    let mut chunk = Vec::new();
    let mut spec = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("packet: {e}")),
        };
        if packet.track_id != id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        };
        if spec.is_none() {
            let s = decoded.spec();
            spec = Some((s.rate(), s.channels().count().max(1)));
        }
        decoded.copy_to_vec_interleaved(&mut chunk);
        pcm.extend_from_slice(&chunk);
    }
    let (rate, channels) = spec.ok_or("nothing decoded")?;
    Ok((rate, channels, pcm))
}

fn load(path: &Path, gain: bool) -> Result<Track, String> {
    let analysis = analyze_file(path)?;
    let (rate, channels, pcm) = decode(path)?;
    let gain_db = if gain {
        analysis.replay_gain_db.unwrap_or(0.0)
    } else {
        0.0
    };
    Ok(Track {
        path: path.to_path_buf(),
        rate,
        channels,
        pcm,
        beats: analysis.beats,
        bpm: analysis.bpm,
        energy: analysis.energy,
        gain: 10f32.powf(gain_db as f32 / 20.0),
    })
}

/// The incoming period folded onto the outgoing one, as `automix::plan`
/// folds it: `(fold, ratio)`, `ratio` being the incoming period over the
/// outgoing one after folding — the speed the incoming track plays at to
/// take the outgoing tempo.
fn fold(og: &Grid, ig: &Grid) -> (f64, f64) {
    let q = ig.period / og.period;
    let fold = [0.5, 1.0, 2.0]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (q / a).ln().abs().total_cmp(&(q / b).ln().abs()))
        .unwrap_or(1.0);
    (fold, q / fold)
}

/// A speed curve over a track's own time: how many seconds of track one
/// second of output plays, as a function of the track position.
type Speed = Box<dyn Fn(f64) -> f64>;

fn smoothstep(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * (3.0 - 2.0 * u)
}

/// A speed going from `from` to `to` between track times `a` and `b`.
fn ramp(from: f64, to: f64, a: f64, b: f64) -> Speed {
    Box::new(move |s| from + (to - from) * smoothstep((s - a) / (b - a)))
}

/// Output seconds track time `start..end` takes at `speed`, integrated the
/// way [`play`] feeds the stretcher: one tempo per block.
fn output_time(speed: &dyn Fn(f64) -> f64, rate: u32, start: f64, end: f64) -> f64 {
    let block = BLOCK as f64 / f64::from(rate);
    let mut s = start;
    let mut t = 0.0;
    while s + block <= end {
        t += block / speed(s + block / 2.0);
        s += block;
    }
    t + (end - s) / speed(s)
}

/// Track time reached after `t` output seconds from track time `start`.
fn track_time(speed: &dyn Fn(f64) -> f64, rate: u32, start: f64, t: f64) -> f64 {
    let block = BLOCK as f64 / f64::from(rate);
    let mut s = start;
    let mut left = t;
    loop {
        let v = speed(s + block / 2.0);
        let dt = block / v;
        if dt >= left {
            return s + left * v;
        }
        left -= dt;
        s += block;
    }
}

/// `frames` output frames of `pcm` from track frame `from`, at `speed`.
/// A speed of exactly 1 everywhere copies the samples.
fn play(
    pcm: &[f32],
    channels: usize,
    rate: u32,
    from: usize,
    speed: Option<&dyn Fn(f64) -> f64>,
    frames: usize,
) -> Vec<f32> {
    let Some(speed) = speed else {
        let mut out: Vec<f32> = pcm
            .get(from * channels..)
            .unwrap_or(&[])
            .iter()
            .take(frames * channels)
            .copied()
            .collect();
        out.resize(frames * channels, 0.0);
        return out;
    };
    let at = |f: usize| speed(f as f64 / f64::from(rate));
    let mut st = Stretcher::new(rate, channels, at(from + BLOCK / 2)).expect("stretcher");
    let mut out = Vec::with_capacity(frames * channels + BLOCK * 4);
    let mut f = from;
    while out.len() < frames * channels {
        let end = (f + BLOCK).min(pcm.len() / channels);
        if f >= end {
            st.finish(&mut out).expect("finish");
            break;
        }
        st.set_tempo(at(f + BLOCK / 2));
        st.process(&pcm[f * channels..end * channels], &mut out)
            .expect("process");
        f = end;
    }
    out.resize(frames * channels, 0.0);
    out
}

/// A transition as the renderer lays it out, every time in output seconds
/// from the start of the outgoing track's playback (which is its own time
/// when it is not stretched).
struct Layout {
    /// When the incoming track's first sample plays; the fade starts there.
    fade_start: f64,
    fade_end: f64,
    /// The outgoing track's output period over the fade.
    period: f64,
    beats: u32,
    fold: f64,
    ratio: f64,
    /// Speed of each side, over its own time; `None` is 1 throughout.
    out_speed: Option<Speed>,
    in_speed: Option<Speed>,
    /// Where the incoming tempo is back to its own, in output seconds.
    settled: f64,
    note: String,
    /// Where in its own time the incoming track starts, in seconds: 0, or a
    /// phrase past its intro for the band swap.
    in_from: f64,
}

/// Where the outgoing track leaves, as `automix` places it: one beat past
/// its last one. `(grid, run length, end in seconds)`.
fn outgoing_grid(beats: &[f64], duration: f64) -> Option<(Grid, usize, f64)> {
    if beats.len() < automix::MIN_GRID_BEATS {
        return None;
    }
    let run = &beats[beats.len().saturating_sub(FIT_BEATS)..];
    let grid = Grid::fit(run)?;
    if grid.scatter > automix::MAX_SCATTER * grid.period {
        return None;
    }
    Some((grid, run.len(), grid.at(run.len() as f64).min(duration)))
}

fn incoming_grid(beats: &[f64]) -> Option<Grid> {
    if beats.len() < automix::MIN_GRID_BEATS {
        return None;
    }
    let grid = Grid::fit(&beats[..beats.len().min(FIT_BEATS)])?;
    (grid.scatter <= automix::MAX_SCATTER * grid.period).then_some(grid)
}

/// The stretched plan: the beats lined up in the middle of the fade, both
/// tracks at one tempo through it. `tempo_out` stretches the outgoing track
/// to the incoming tempo before the fade; otherwise the incoming one is
/// stretched to the outgoing tempo, and glides back after.
fn synced(
    out_beats: &[f64],
    out_duration: f64,
    inc: &Track,
    rate: u32,
    cap: f64,
    tempo_out: bool,
    shift: f64,
) -> Result<Layout, String> {
    let (og, run_len, end) =
        outgoing_grid(out_beats, out_duration).ok_or("outgoing: no steady grid")?;
    let ig = incoming_grid(&inc.beats).ok_or("incoming: no steady grid")?;
    let (fold, ratio) = fold(&og, &ig);
    if ratio.ln().abs() > (1.0 + MAX_STRETCH).ln() {
        return Err(format!(
            "tempo gap {:+.1} % over the ±{:.0} % stretched",
            (ratio - 1.0) * 100.0,
            MAX_STRETCH * 100.0
        ));
    }
    let end_line = run_len as f64;
    for beats in FADE_BEATS {
        let n = f64::from(beats);
        let first = end_line - n;
        if og.at(first) < 0.0 {
            continue;
        }
        let step = fold.max(1.0);
        let anchor_lines = ((n / 2.0) / step).round() * step;
        let anchor_beat = anchor_lines / fold;
        // The outgoing track's speed over its own time, and the output time
        // of a moment of it.
        let (out_speed, in_rate): (Option<Speed>, f64) = if tempo_out {
            let a = og.at(first - RAMP_OUT_BEATS).max(0.0);
            (Some(ramp(1.0, 1.0 / ratio, a, og.at(first))), 1.0)
        } else {
            (None, ratio)
        };
        let out_t = |s: f64| match &out_speed {
            Some(v) => output_time(v.as_ref(), rate, 0.0, s),
            None => s,
        };
        let anchor = out_t(og.at(first + anchor_lines));
        let fade_end = out_t(end);
        let period = (fade_end - out_t(og.at(first))) / n;
        // The grids lined up, then the incoming track moved by the phase its
        // attacks sit at against the outgoing one's ([`phase_shift`]).
        let fade_start = anchor - ig.at(anchor_beat) / in_rate + shift * period;
        if fade_start < 0.0 || fade_end - fade_start > cap + 1e-9 {
            continue;
        }
        // The incoming track, stretched to the outgoing tempo through the
        // fade, glides back to its own over the next beats.
        let (in_speed, settled): (Option<Speed>, f64) = if tempo_out {
            (None, fade_end)
        } else {
            let at_end = (fade_end - fade_start) * ratio;
            let back = at_end + RAMP_IN_BEATS * ig.period;
            let settled =
                fade_start + output_time(&*ramp(ratio, 1.0, at_end, back), rate, 0.0, back);
            (Some(ramp(ratio, 1.0, at_end, back)), settled)
        };
        return Ok(Layout {
            fade_start,
            fade_end,
            period,
            beats,
            fold,
            ratio,
            out_speed,
            in_speed,
            settled,
            note: String::new(),
            in_from: 0.0,
        });
    }
    Err("nothing fits the crossfade length".into())
}

/// Bins of a beat in a rhythm profile.
const PROFILE_BINS: usize = 32;
/// Beats each rhythm profile is taken over: about what a fade mixes.
const PROFILE_BEATS: f64 = 16.0;

/// Where the attacks under 150 Hz fall within a beat, over `from..to`
/// seconds of `t`: each hop's attack added to the bin of its phase against
/// `grid` taken at `period`. The kick of most music piles up on one or two
/// bins; a track without one (an intro, a ballad) spreads flat.
fn profile(t: &Track, grid: &Grid, period: f64, from: f64, to: f64) -> [f64; PROFILE_BINS] {
    let rate = f64::from(t.rate);
    let a = (from.max(0.0) * rate) as usize;
    let b = ((to * rate) as usize).min(t.frames());
    let mut bins = [0.0; PROFILE_BINS];
    for (k, e) in attacks(&t.pcm, t.channels, t.rate, a, b, true)
        .into_iter()
        .enumerate()
    {
        let time = (a + k * 128 + 64) as f64 / rate;
        let phase = ((time - grid.origin) / period).rem_euclid(1.0);
        bins[((phase * PROFILE_BINS as f64) as usize).min(PROFILE_BINS - 1)] += e;
    }
    bins
}

/// How far, in beats, to move the incoming track so its attacks fall where
/// the outgoing track's do, once the grids are lined up: the circular shift
/// that best matches the two profiles, in `-0.5..0.5`, and how far its
/// score stands above the average shift's (1 is no preference at all).
/// Taken over the outgoing track's last [`FIT_BEATS`] beats and the
/// incoming one's first, folded onto the outgoing beat.
fn phase_shift(out: &Track, inc: &Track) -> Option<(f64, f64)> {
    let (og, run_len, end) = outgoing_grid(&out.beats, out.duration())?;
    let ig = incoming_grid(&inc.beats)?;
    let (fold, _) = fold(&og, &ig);
    let po = profile(
        out,
        &og,
        og.period,
        og.at(run_len as f64 - PROFILE_BEATS),
        end,
    );
    let ip = ig.period / fold;
    let pi = profile(inc, &ig, ip, 0.0, ig.at(PROFILE_BEATS / fold));
    // Centred and scaled, so the score is a correlation: what every bin has
    // in common (the attacks between the beats) says nothing of the phase.
    let centred = |p: [f64; PROFILE_BINS]| {
        let mean = p.iter().sum::<f64>() / PROFILE_BINS as f64;
        let c = p.map(|x| x - mean);
        let norm = c.iter().map(|x| x * x).sum::<f64>().sqrt();
        (norm > 0.0).then(|| c.map(|x| x / norm))
    };
    let (po, pi) = (centred(po)?, centred(pi)?);
    let score = |s: usize| -> f64 {
        (0..PROFILE_BINS)
            .map(|b| po[b] * pi[(b + PROFILE_BINS - s) % PROFILE_BINS])
            .sum()
    };
    let scores: Vec<f64> = (0..PROFILE_BINS).map(score).collect();
    let best = (0..PROFILE_BINS).max_by(|&a, &b| scores[a].total_cmp(&scores[b]))?;
    let (l, c, r) = (
        scores[(best + PROFILE_BINS - 1) % PROFILE_BINS],
        scores[best],
        scores[(best + 1) % PROFILE_BINS],
    );
    let refine = if l - 2.0 * c + r != 0.0 {
        0.5 * (l - r) / (l - 2.0 * c + r)
    } else {
        0.0
    };
    let s = ((best as f64 + refine) / PROFILE_BINS as f64).rem_euclid(1.0);
    Some((if s >= 0.5 { s - 1.0 } else { s }, c))
}

/// What AutoMix does today, or the plain crossfade it falls back to.
fn current(out: &Track, inc: &Track, cap: f64) -> Layout {
    match automix::plan(out.beats_view(), inc.beats_view(), (cap * 1000.0) as u64) {
        Ok(p) => Layout {
            fade_start: p.fade_start_us as f64 / 1e6,
            fade_end: p.fade_end_us as f64 / 1e6,
            period: outgoing_grid(&out.beats, out.duration()).map_or(0.0, |(g, _, _)| g.period),
            beats: p.beats,
            fold: p.fold,
            ratio: p.ratio,
            out_speed: None,
            in_speed: None,
            settled: p.fade_end_us as f64 / 1e6,
            note: format!("AutoMix plan, drift {:.1} ms", p.max_drift_ms),
            in_from: 0.0,
        },
        Err(why) => {
            let end = out.duration();
            let cap = cap.min(end / 2.0);
            Layout {
                fade_start: end - cap,
                fade_end: end,
                period: 0.0,
                beats: 0,
                fold: 1.0,
                ratio: 1.0,
                out_speed: None,
                in_speed: None,
                settled: end,
                note: format!("falls back to the plain crossfade ({why:?})"),
                in_from: 0.0,
            }
        }
    }
}

/// Two-pole filter, transposed direct form II, one state per channel.
#[derive(Clone)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: Vec<[f32; 2]>,
}

impl Biquad {
    fn new(channels: usize) -> Self {
        Self {
            b: [1.0, 0.0, 0.0],
            a: [0.0, 0.0],
            z: vec![[0.0; 2]; channels],
        }
    }

    /// RBJ cookbook high-pass (`high`) or low-pass at `freq`, quality `q`.
    fn set(&mut self, high: bool, freq: f64, q: f64, rate: u32) {
        let w = 2.0 * std::f64::consts::PI * freq.min(0.45 * f64::from(rate)) / f64::from(rate);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + cos) / 2.0, -(1.0 + cos))
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos)
        };
        self.b = [(b0 / a0) as f32, (b1 / a0) as f32, (b0 / a0) as f32];
        self.a = [(-2.0 * cos / a0) as f32, ((1.0 - alpha) / a0) as f32];
    }

    fn run(&mut self, ch: usize, x: f32) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// A 4th-order Linkwitz-Riley low-pass: two Butterworth sections.
struct Lr4 {
    lp: [Biquad; 2],
}

const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

impl Lr4 {
    fn new(freq: f64, channels: usize, rate: u32) -> Self {
        let section = || {
            let mut b = Biquad::new(channels);
            b.set(false, freq, BUTTERWORTH_Q, rate);
            b
        };
        Self {
            lp: [section(), section()],
        }
    }

    fn low(&mut self, ch: usize, x: f32) -> f32 {
        let low = self.lp[0].run(ch, x);
        self.lp[1].run(ch, low)
    }
}

/// The band swap's crossover frequencies, in Hz.
const SWAP_SPLITS: [f64; 3] = [150.0, 600.0, 2500.0];

/// Four bands — under 150 Hz, 150-600 Hz, 600 Hz-2.5 kHz, over 2.5 kHz —
/// taken by subtraction: the bass is the signal under the first low-pass,
/// each band above is what one low-pass lets through and the one below it
/// does not, the highs what is left. They add back to the signal exactly,
/// so a band at full gain changes nothing, phase included. A split whose
/// bands add up to an all-pass instead (a crossover tree) put the peaks of
/// a limited master 6.8 dB higher (1.075 to 2.348) at the same energy.
struct Bands {
    low: [Lr4; 3],
}

impl Bands {
    fn new(channels: usize, rate: u32) -> Self {
        Self {
            low: SWAP_SPLITS.map(|f| Lr4::new(f, channels, rate)),
        }
    }

    /// `[bass, low-mid, mid, high]` of one sample of channel `ch`.
    fn split(&mut self, ch: usize, x: f32) -> [f32; 4] {
        let l150 = self.low[0].low(ch, x);
        let l600 = self.low[1].low(ch, x);
        let l2500 = self.low[2].low(ch, x);
        [l150, l600 - l150, l2500 - l600, x - l2500]
    }
}

/// When each band changes hands over the swap, as fractions of it, read
/// off the reference transition (a 9 s swap): `(starts, done)` for the
/// incoming band coming up and for the outgoing one going, bass first.
/// The outgoing bass is not faded: it is cut on the last beat.
const SWAP_IN: [(f64, f64); 4] = [(0.61, 0.89), (0.50, 0.78), (0.17, 0.44), (0.0, 0.44)];
const SWAP_OUT: [(f64, f64); 4] = [(f64::NAN, f64::NAN), (0.94, 1.0), (0.72, 1.0), (0.0, 0.44)];

/// Median of a non-empty slice.
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

/// The band swap's plan, read from both tracks' beats and levels:
///
/// - **where the outgoing track's music stops**: the beat after the last
///   one within 6 dB of the level of its last 64 beats (the beat list runs
///   on through a ring-out the swap should drop);
/// - **where the swap starts on it**: the deepest one-beat dip (a break,
///   the end of a section) 8 to 32 beats before that, at least 3 dB under
///   the beats around it, else 16 beats before;
/// - **where the incoming track comes in**: on its first 32-beat phrase
///   past its intro (the first beat within 3 dB of its typical level),
///   counted from its first beat, unless `entry` names the beat;
/// - **the tempo**: the outgoing track glides to the incoming one's over 8
///   beats, 64 beats before the swap, and stays there; the incoming one
///   plays as it is.
fn band_swap(out: &Track, inc: &Track, cap: f64, entry: Option<usize>) -> Result<Layout, String> {
    let rate = out.rate;
    let ol = beat_db(out, &out.beats);
    let n = ol.len();
    if n < 64 {
        return Err("outgoing: too few beats".into());
    }
    let typical = median(&ol[n - 64..]);
    let k_end = (0..n)
        .rev()
        .find(|&k| ol[k] >= typical - 6.0)
        .map_or(n, |k| k + 1);
    if k_end < 40 {
        return Err("outgoing: no music end found".into());
    }
    let dip = |k: usize| -> f64 {
        let around: Vec<f64> = (k.saturating_sub(4)..(k + 5).min(n))
            .filter(|&j| j != k)
            .map(|j| ol[j])
            .collect();
        ol[k] - median(&around)
    };
    let (mut k_start, depth) = (k_end - 32..=k_end - 8)
        .map(|k| (k, dip(k)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((k_end - 16, 0.0));
    let mut how = format!("a {depth:+.1} dB dip");
    if depth > -3.0 {
        k_start = k_end - 16;
        how = "no dip, 16 beats before".into();
    }

    let il = beat_db(inc, &inc.beats);
    let head = &il[..il.len().min(200)];
    let in_typical = median(head);
    let intro_end = head
        .iter()
        .position(|&l| l >= in_typical - 3.0)
        .unwrap_or(0);
    let k_in = entry.unwrap_or(intro_end.div_ceil(32) * 32);
    if k_in + 32 > inc.beats.len() {
        return Err("incoming: entry too late".into());
    }

    let og = Grid::fit(&out.beats[k_end - 32..k_end]).ok_or("outgoing: no grid")?;
    let ig = Grid::fit(&inc.beats[k_in..k_in + 32]).ok_or("incoming: no grid")?;
    for (g, side) in [(&og, "outgoing"), (&ig, "incoming")] {
        if g.scatter > automix::MAX_SCATTER * g.period {
            return Err(format!("{side}: unsteady grid"));
        }
    }
    let (fold, ratio) = fold(&og, &ig);
    if ratio.ln().abs() > (1.0 + MAX_STRETCH).ln() {
        return Err(format!("tempo gap {:+.1} %", (ratio - 1.0) * 100.0));
    }
    // Within the crossfade length, in whole bars.
    let mut swap = (k_end - k_start) as f64;
    if swap * og.period * ratio > cap {
        swap = ((cap / (og.period * ratio) / 4.0).floor() * 4.0).max(4.0);
        k_start = k_end - swap as usize;
        how = format!("{how}, cut to fit {cap} s");
    }

    let glide_to = out.beats[k_start.saturating_sub(64)];
    let glide_from = out.beats[k_start.saturating_sub(72)];
    let out_speed = ramp(
        1.0,
        1.0 / ratio,
        glide_from,
        glide_to.max(glide_from + 1e-3),
    );
    let out_t = |s: f64| output_time(out_speed.as_ref(), rate, 0.0, s);
    let first = 32.0 - swap;
    let step = fold.max(1.0);
    let m = ((swap / 2.0) / step).round() * step;
    let anchor = out_t(og.at(first + m));
    let in_from = ig.at(0.0);
    let fade_start = anchor - (ig.at(m / fold) - in_from);
    let fade_end = out_t(og.at(32.0));
    let period = (fade_end - out_t(og.at(first))) / swap;
    let behind = track_time(out_speed.as_ref(), rate, 0.0, fade_start) - in_from;
    Ok(Layout {
        fade_start,
        fade_end,
        period,
        beats: swap as u32,
        fold,
        ratio,
        out_speed: Some(out_speed),
        in_speed: None,
        settled: fade_end,
        note: format!(
            "outgoing music ends on beat {k_end} ({:.2} s), swap from beat {k_start} ({:.2} s, {how}); \
             incoming enters on its beat {k_in} ({:.2} s, intro to beat {intro_end}); at the swap the incoming track runs {:.3} s behind the outgoing one",
            og.at(32.0),
            og.at(first),
            in_from,
            behind
        ),
        in_from,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Current,
    TempoInPhase,
    TempoIn,
    TempoOut,
    Echo,
    Loop,
    Stutter,
    Filter,
    /// The band swap with the incoming track's entry found by the rule.
    BandSwap,
    /// The band swap entering on the incoming beat given.
    BandSwapAt(usize),
}

impl Style {
    fn name(self) -> &'static str {
        match self {
            Style::Current => "current",
            Style::TempoInPhase => "tempo-in-phase-corrected",
            Style::TempoIn => "tempo-in",
            Style::TempoOut => "tempo-out",
            Style::Echo => "echo",
            Style::Loop => "loop",
            Style::Stutter => "stutter",
            Style::Filter => "filter",
            Style::BandSwap => "band-swap",
            Style::BandSwapAt(_) => "band-swap-forced-entry",
        }
    }
}

/// The outgoing track with its last beats looped after its end: the loop
/// lengths in beats, each from the same start, two beats before the end.
/// Each repeat is faded in and out over a few milliseconds, so the cut is
/// not a click. Returns the samples and the beats, both running on past the
/// original end on the same grid.
fn looped(out: &Track, lengths: &[f64]) -> Option<(Vec<f32>, Vec<f64>, f64)> {
    let (og, run_len, end) = outgoing_grid(&out.beats, out.duration())?;
    let rate = f64::from(out.rate);
    let ch = out.channels;
    let start = og.at(run_len as f64 - 2.0);
    let mut pcm = out.pcm[..((end * rate) as usize * ch).min(out.pcm.len())].to_vec();
    let edge = (0.004 * rate) as usize;
    let mut total = 0.0;
    for &len in lengths {
        let a = (start * rate).round() as usize;
        let b = ((start + len * og.period) * rate).round() as usize;
        let n = b - a;
        for i in 0..n {
            let g = if i < edge {
                i as f32 / edge as f32
            } else if n - i < edge {
                (n - i) as f32 / edge as f32
            } else {
                1.0
            };
            for c in 0..ch {
                pcm.push(out.pcm.get((a + i) * ch + c).copied().unwrap_or(0.0) * g);
            }
        }
        total += len;
    }
    // The grid runs on through the loops, a beat at a time.
    let mut beats: Vec<f64> = out.beats.iter().copied().filter(|&b| b < end).collect();
    let mut k = run_len as f64;
    while og.at(k) < end + total * og.period - 1e-6 {
        beats.push(og.at(k));
        k += 1.0;
    }
    Some((pcm, beats, end + total * og.period))
}

struct Rendered {
    samples: Vec<f32>,
    fade_at: f64,
    fade_end_at: f64,
    settled_at: f64,
    layout_note: String,
    /// How far the incoming track's attacks sit after the outgoing one's
    /// over the middle of the fade, in milliseconds ([`attack_lag_ms`]).
    lag_ms: Option<f64>,
}

/// Where the kick drum falls against a run of the tracker's beats, in
/// milliseconds (positive: the kicks come after the beats), within half a
/// period either way. A tracker locked on the off-beats reads near half a
/// period, which a mix would put against the other track's beats.
fn kick_phase_ms(t: &Track, beats: &[f64], period: f64) -> Option<f64> {
    let rate = f64::from(t.rate);
    let from = ((beats.first()? - period) * rate).max(0.0) as usize;
    let to = (((beats.last()? + period) * rate) as usize).min(t.frames());
    let lines: Vec<usize> = beats.iter().map(|b| (b * rate).round() as usize).collect();
    let clicks = click_train(&lines, t.frames(), t.channels);
    attack_lag_ms(
        true,
        &clicks,
        &t.pcm,
        t.channels,
        t.rate,
        from,
        to,
        period * 500.0,
    )
}

/// Interleaved clicks, one frame long, on every grid line in `from..to`.
fn click_train(lines: &[usize], frames: usize, channels: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames * channels];
    for &l in lines {
        for k in 0..128 {
            if let Some(x) = v.get_mut((l + k) * channels) {
                *x = if k == 0 { 1.0 } else { 0.0 };
            }
        }
    }
    v
}

/// Attack envelope of interleaved `pcm` over `from..to` frames: the rise of
/// the log energy of 128-frame hops, half-wave rectified, under 150 Hz —
/// the kick drum, which falls on the beats, where the hats and the snare
/// fill every subdivision and leave the phase ambiguous. Clicks (a single
/// sample) go through whole.
fn attacks(pcm: &[f32], channels: usize, rate: u32, from: usize, to: usize, low: bool) -> Vec<f64> {
    const HOP: usize = 128;
    let mut lp = Biquad::new(1);
    lp.set(false, 150.0, 0.7, rate);
    let mut lp2 = lp.clone();
    let mut prev = 0.0;
    let mut env = Vec::new();
    let mut f = from;
    while f + HOP <= to {
        let e: f64 = (f..f + HOP)
            .map(|k| {
                let mut m: f32 = pcm[k * channels..(k + 1) * channels].iter().sum();
                if low {
                    m = lp2.run(0, lp.run(0, m));
                }
                f64::from(m * m)
            })
            .sum();
        let l = (1.0 + 1e4 * e / HOP as f64).ln();
        env.push((l - prev).max(0.0));
        prev = l;
        f += HOP;
    }
    env
}

/// The lag, in milliseconds, that best lines up the attacks of `b` with
/// those of `a` over `from..to` frames, searched within ±`reach_ms`, refined
/// between hops by a parabola through the best three. Positive: `b` late.
#[allow(clippy::too_many_arguments)]
fn attack_lag_ms(
    clicks_a: bool,
    a: &[f32],
    b: &[f32],
    channels: usize,
    rate: u32,
    from: usize,
    to: usize,
    reach_ms: f64,
) -> Option<f64> {
    let ea = attacks(a, channels, rate, from, to, !clicks_a);
    let eb = attacks(b, channels, rate, from, to, true);
    let hop_ms = 128.0 * 1000.0 / f64::from(rate);
    let reach = (reach_ms / hop_ms).ceil() as isize;
    let n = ea.len() as isize;
    if n < 4 * reach {
        return None;
    }
    let score = |lag: isize| -> f64 {
        (reach..n - reach)
            .map(|k| ea[k as usize] * eb[(k + lag) as usize])
            .sum()
    };
    let scores: Vec<f64> = (-reach..=reach).map(score).collect();
    let best = (1..scores.len() - 1).max_by(|&x, &y| scores[x].total_cmp(&scores[y]))?;
    let (l, c, r) = (scores[best - 1], scores[best], scores[best + 1]);
    let shift = if l - 2.0 * c + r != 0.0 {
        0.5 * (l - r) / (l - 2.0 * c + r)
    } else {
        0.0
    };
    Some((best as f64 - reach as f64 + shift) * hop_ms)
}

/// Level of each beat of `t`, in dB: the RMS over all channels from beat
/// `k` to beat `k + 1` (the last one over the period before it).
fn beat_db(t: &Track, beats: &[f64]) -> Vec<f64> {
    let rate = f64::from(t.rate);
    let ch = t.channels;
    (0..beats.len())
        .map(|k| {
            let a = beats[k];
            let b = beats
                .get(k + 1)
                .copied()
                .unwrap_or(a + (a - beats[k.saturating_sub(1)]).max(0.3));
            let (fa, fb) = ((a * rate) as usize, ((b * rate) as usize).min(t.frames()));
            if fb <= fa {
                return -120.0;
            }
            let sum: f64 = t.pcm[fa * ch..fb * ch]
                .iter()
                .map(|s| f64::from(*s).powi(2))
                .sum();
            10.0 * (sum / ((fb - fa) * ch) as f64 + 1e-12).log10()
        })
        .collect()
}

/// One vector per beat of `t` that changes when the music does: the log
/// energy of 24 bands from 60 Hz to 12 kHz and the 12 pitch classes
/// between 100 Hz and 4 kHz, averaged over the beat's 2048-point spectra,
/// each dimension scaled to zero mean and unit variance over the beats
/// given.
fn beat_features(t: &Track, beats: &[f64]) -> Vec<Vec<f64>> {
    use realfft::RealFftPlanner;
    const N: usize = 2048;
    const HOP: usize = 1024;
    let rate = f64::from(t.rate);
    let ch = t.channels;
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N);
    let window: Vec<f64> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N as f64).cos())
        .collect();
    let hz = |bin: usize| bin as f64 * rate / N as f64;
    let band_of = |f: f64| -> Option<usize> {
        (60.0..12_000.0)
            .contains(&f)
            .then(|| ((f / 60.0).ln() / (200.0f64).ln() * 24.0) as usize)
    };
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut feats = Vec::with_capacity(beats.len());
    for k in 0..beats.len() {
        let a = (beats[k] * rate) as usize;
        let b = beats
            .get(k + 1)
            .map_or(a + HOP * 2, |&e| (e * rate) as usize)
            .min(t.frames().saturating_sub(N));
        let mut v = vec![0.0; 36];
        let mut frames = 0.0;
        let mut f = a;
        while f + N <= b + N / 2 && f + N <= t.frames() {
            for i in 0..N {
                let s: f32 = t.pcm[(f + i) * ch..(f + i + 1) * ch].iter().sum();
                input[i] = f64::from(s) * window[i];
            }
            fft.process(&mut input, &mut spec).expect("fft");
            for (bin, c) in spec.iter().enumerate().skip(1) {
                let p = c.norm_sqr();
                let f_hz = hz(bin);
                if let Some(band) = band_of(f_hz) {
                    v[band.min(23)] += p;
                }
                if (100.0..4000.0).contains(&f_hz) {
                    let pc = (12.0 * (f_hz / 440.0).log2()).round().rem_euclid(12.0) as usize;
                    v[24 + pc] += p;
                }
            }
            frames += 1.0;
            f += HOP;
        }
        let frames: f64 = if frames > 0.0 { frames } else { 1.0 };
        let chroma_total: f64 = v[24..].iter().sum::<f64>() + 1e-12;
        for (d, x) in v.iter_mut().enumerate() {
            *x = if d < 24 {
                (*x / frames + 1e-9).ln()
            } else {
                *x / chroma_total
            };
        }
        feats.push(v);
    }
    for d in 0..36 {
        let n = feats.len().max(1) as f64;
        let mean = feats.iter().map(|v| v[d]).sum::<f64>() / n;
        let sd = (feats.iter().map(|v| (v[d] - mean).powi(2)).sum::<f64>() / n).sqrt() + 1e-9;
        for v in &mut feats {
            v[d] = (v[d] - mean) / sd;
        }
    }
    feats
}

/// Where the music changes, beat by beat: the novelty of a checkerboard
/// kernel `2 × half` beats wide slid along the diagonal of the beats'
/// cosine self-similarity (high where what came before is alike, what
/// comes after is alike, and the two differ). Zero within `half` beats of
/// either end.
fn novelty(feats: &[Vec<f64>], half: usize) -> Vec<f64> {
    let n = feats.len();
    let norm: Vec<f64> = feats
        .iter()
        .map(|v| v.iter().map(|x| x * x).sum::<f64>().sqrt() + 1e-9)
        .collect();
    let sim = |i: usize, j: usize| -> f64 {
        feats[i]
            .iter()
            .zip(&feats[j])
            .map(|(a, b)| a * b)
            .sum::<f64>()
            / (norm[i] * norm[j])
    };
    let mut out = vec![0.0; n];
    for (c, o) in out
        .iter_mut()
        .enumerate()
        .take(n.saturating_sub(half))
        .skip(half)
    {
        let mut s = 0.0;
        for i in c - half..c + half {
            for j in c - half..c + half {
                let same_side = (i < c) == (j < c);
                let w = {
                    let di = (i as f64 - c as f64 + 0.5) / half as f64;
                    let dj = (j as f64 - c as f64 + 0.5) / half as f64;
                    (-(di * di + dj * dj) * 2.0).exp()
                };
                s += if same_side { w } else { -w } * sim(i, j);
            }
        }
        *o = s / (4.0 * (half * half) as f64);
    }
    out
}

fn render(
    out: &Track,
    inc: &Track,
    style: Style,
    cap: f64,
    shift: f64,
) -> Result<Rendered, String> {
    let rate = out.rate;
    let ch = out.channels;
    // The outgoing source: the track, or the track with its loop after it.
    let (out_pcm, out_beats, out_dur) = match style {
        Style::Loop => looped(out, &[2.0, 2.0, 2.0, 2.0]).ok_or("no grid to loop on")?,
        Style::Stutter => looped(out, &[2.0, 2.0, 1.0, 1.0, 0.5, 0.5, 0.25, 0.25, 0.25, 0.25])
            .ok_or("no grid to loop on")?,
        _ => (out.pcm.clone(), out.beats.clone(), out.duration()),
    };
    let layout = match style {
        Style::Current => current(out, inc, cap),
        Style::TempoInPhase => synced(&out_beats, out_dur, inc, rate, cap, false, shift)?,
        Style::TempoOut => synced(&out_beats, out_dur, inc, rate, cap, true, 0.0)?,
        Style::BandSwap => band_swap(out, inc, cap, None)?,
        Style::BandSwapAt(k) => band_swap(out, inc, cap, Some(k))?,
        _ => synced(&out_beats, out_dur, inc, rate, cap, false, 0.0)?,
    };

    // Output window: from PRE_S before the fade (or the tempo glide) to
    // POST_S after the incoming tempo has settled.
    let glide_from = match &layout.out_speed {
        Some(_) => layout.fade_start - RAMP_OUT_BEATS * layout.period - 1.0,
        None => layout.fade_start,
    };
    let swap = matches!(style, Style::BandSwap | Style::BandSwapAt(_));
    let (start, stop) = if swap {
        // Framed like the reference recording: 21 s before, 5 s after.
        ((layout.fade_start - 21.0).max(0.0), layout.fade_end + 5.0)
    } else {
        (
            (glide_from.min(layout.fade_start) - PRE_S).max(0.0),
            layout.settled.max(layout.fade_end) + POST_S,
        )
    };
    let frames = ((stop - start) * f64::from(rate)) as usize;
    let fr = |t: f64| ((t - start) * f64::from(rate)).round().max(0.0) as usize;

    // The outgoing side over the window. When stretched, it is played from
    // the start of its own time so the speed curve is followed from 1.
    let out_from = match &layout.out_speed {
        Some(v) => track_time(v.as_ref(), rate, 0.0, start),
        None => start,
    };
    let o = play(
        &out_pcm,
        ch,
        rate,
        (out_from * f64::from(rate)).round() as usize,
        layout.out_speed.as_deref(),
        frames,
    );
    let in_at = fr(layout.fade_start);
    let i_side = play(
        &inc.pcm,
        ch,
        rate,
        (layout.in_from * f64::from(rate)).round() as usize,
        layout.in_speed.as_deref(),
        frames - in_at,
    );
    let mut i = vec![0.0; in_at * ch];
    i.extend_from_slice(&i_side);

    let f0 = fr(layout.fade_start);
    let f1 = fr(layout.fade_end).max(f0 + 1);
    let span = (f1 - f0) as f64;
    let beat = (layout.period * f64::from(rate)).max(1.0);
    let progress = |f: usize| ((f as f64 - f0 as f64) / span).clamp(0.0, 1.0);

    // Gains, filters and effects.
    let mut hp = Biquad::new(ch);
    let mut lp = Biquad::new(ch);
    let delay = (0.75 * beat).round() as usize;
    let mut echo = vec![0.0_f32; (delay + 1) * ch];
    let mut echo_at = 0usize;
    let mut wet_hp = Biquad::new(ch);
    wet_hp.set(true, 300.0, 0.7, rate);
    let mut wet_lp = Biquad::new(ch);
    wet_lp.set(false, 5000.0, 0.7, rate);
    let mut out_bands = Bands::new(ch, rate);
    let mut in_bands = Bands::new(ch, rate);
    let mut g_bands_out = [1.0_f32; 4];
    let mut g_bands_in = [0.0_f32; 4];
    // The outgoing bass is cut on the last beat, over 10 ms.
    let last_beat = f1.saturating_sub(beat as usize);
    let cut = (0.01 * f64::from(rate)) as usize;
    let mut mix = vec![0.0_f32; frames * ch];
    for f in 0..frames {
        let t = progress(f);
        let (mut g_out, mut g_in) = if f < f0 {
            (1.0, 0.0)
        } else if f >= f1 {
            (0.0, 1.0)
        } else {
            let a = t * std::f64::consts::FRAC_PI_2;
            (a.cos(), a.sin())
        };
        let mut send = 0.0;
        match style {
            Style::Echo => {
                // The outgoing track holds (down 4.5 dB at the end) while the
                // incoming one comes up over the first three quarters, then is
                // cut on its last beat line; the last beat before it feeds
                // the echo.
                if (f0..f1).contains(&f) {
                    g_out = (0.6 * t * std::f64::consts::FRAC_PI_2).cos();
                    g_in = ((t / 0.75).min(1.0) * std::f64::consts::FRAC_PI_2).sin();
                }
                if f + beat as usize >= f1 && f < f1 {
                    send = g_out;
                }
            }
            Style::Filter => {
                if (f0..f1).contains(&f) {
                    // Both near full through the middle: the filters keep
                    // them apart.
                    g_out = (smoothstep((t - 0.4) / 0.6) * std::f64::consts::FRAC_PI_2).cos();
                    g_in = (smoothstep(t / 0.6) * std::f64::consts::FRAC_PI_2).sin();
                }
                if f % 16 == 0 {
                    let hp_f = 20.0 * (1200.0_f64 / 20.0).powf(t);
                    hp.set(true, hp_f, 0.9, rate);
                    let lp_f = 300.0 * (20_000.0_f64 / 300.0).powf((t / 0.8).min(1.0));
                    lp.set(false, lp_f, 0.9, rate);
                }
            }
            Style::BandSwap | Style::BandSwapAt(_) => {
                let rise = |(a, b): (f64, f64)| smoothstep((t - a) / (b - a)) as f32;
                for b in 0..4 {
                    g_bands_in[b] = if f < f0 { 0.0 } else { rise(SWAP_IN[b]) };
                    g_bands_out[b] = if f >= f1 {
                        0.0
                    } else if b == 0 {
                        1.0 - ((f.saturating_sub(last_beat)) as f32 / cut as f32).min(1.0)
                    } else {
                        1.0 - rise(SWAP_OUT[b])
                    };
                }
                g_out = 1.0;
                g_in = 1.0;
            }
            Style::Loop | Style::Stutter => {
                // The loop is the point: the outgoing track stays up through
                // the first half and fades over the second, the incoming one
                // comes up over the first 60 %.
                if (f0..f1).contains(&f) {
                    g_out = (smoothstep((t - 0.5) / 0.5) * std::f64::consts::FRAC_PI_2).cos();
                    g_in = ((t / 0.6).min(1.0) * std::f64::consts::FRAC_PI_2).sin();
                }
            }
            _ => {}
        }
        for c in 0..ch {
            let mut so = o[f * ch + c];
            let mut si = i[f * ch + c];
            if swap {
                let bo = out_bands.split(c, so);
                let bi = in_bands.split(c, si);
                so = (0..4).map(|b| bo[b] * g_bands_out[b]).sum();
                si = (0..4).map(|b| bi[b] * g_bands_in[b]).sum();
            }
            if style == Style::Filter && f >= f0 {
                so = hp.run(c, so);
                if f < f1 || t < 1.0 {
                    si = lp.run(c, si);
                }
            }
            let mut wet = 0.0;
            if style == Style::Echo {
                // Every repeat goes through the band-pass, the first one too:
                // its bass would muddy the incoming track's.
                wet = wet_lp.run(c, wet_hp.run(c, echo[echo_at * ch + c]));
                echo[echo_at * ch + c] = so * out.gain * send as f32 + wet * 0.6;
            }
            so *= out.gain;
            si *= inc.gain;
            mix[f * ch + c] = (so * g_out as f32 + si * g_in as f32 + wet * 0.8).clamp(-1.0, 1.0);
        }
        if style == Style::Echo {
            echo_at = (echo_at + 1) % (delay + 1);
        }
    }
    // Over the middle half of the fade, where both are loud; within a third
    // of a beat either way, so the next beat is not taken for this one.
    let lag_ms = (layout.beats > 0)
        .then(|| {
            attack_lag_ms(
                false,
                &o,
                &i,
                ch,
                rate,
                f0 + (f1 - f0) / 4,
                f1 - (f1 - f0) / 4,
                layout.period * 1000.0 / 3.0,
            )
        })
        .flatten();
    Ok(Rendered {
        lag_ms,
        samples: mix,
        fade_at: layout.fade_start - start,
        fade_end_at: layout.fade_end - start,
        settled_at: layout.settled - start,
        layout_note: format!(
            "{} beats, fold {}, tempo gap {:+.2} %{}{}",
            layout.beats,
            layout.fold,
            (layout.ratio - 1.0) * 100.0,
            if layout.note.is_empty() { "" } else { ", " },
            layout.note
        ),
    })
}

fn write_wav(path: &Path, samples: &[f32], rate: u32, channels: usize) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let data = (samples.len() * 3) as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?;
    w.write_all(&(channels as u16).to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * channels as u32 * 3).to_le_bytes())?;
    w.write_all(&(channels as u16 * 3).to_le_bytes())?;
    w.write_all(&24u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data.to_le_bytes())?;
    for s in samples {
        let v = (f64::from(s.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
        w.write_all(&v.to_le_bytes()[..3])?;
    }
    w.flush()
}

fn mmss(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut cap_ms = 12_000u64;
    let mut gain = true;
    let mut entry: Option<usize> = None;
    let mut trim_db = 0.0_f32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cap-ms" => cap_ms = it.next().and_then(|v| v.parse().ok()).expect("--cap-ms N"),
            "--no-gain" => gain = false,
            "--trim-db" => {
                trim_db = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--trim-db dB")
            }
            "--entry-beat" => {
                entry = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .expect("--entry-beat N"),
                );
            }
            _ => positional.push(PathBuf::from(a)),
        }
    }
    let [out_path, in_path, dir] = positional.as_slice() else {
        eprintln!("usage: automix_render <outgoing> <incoming> <out dir> [--cap-ms N] [--no-gain] [--trim-db dB] [--entry-beat N]");
        std::process::exit(2);
    };
    let mut out = load(out_path, gain).expect("outgoing");
    let mut inc = load(in_path, gain).expect("incoming");
    for t in [&mut out, &mut inc] {
        t.gain *= 10f32.powf(trim_db / 20.0);
    }
    if std::env::var_os("AUTOMIX_BANDS_CHECK").is_some() {
        // The four bands at full gain against the signal, tone by tone.
        for freq in [
            30.0, 100.0, 150.0, 300.0, 600.0, 1200.0, 2500.0, 5000.0, 12_000.0,
        ] {
            let mut b = Bands::new(1, 48_000);
            let (mut e_in, mut e_out) = (0.0, 0.0);
            for i in 0..96_000 {
                let x = (2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin() as f32;
                let y: f32 = b.split(0, x).iter().sum();
                if i >= 48_000 {
                    e_in += f64::from(x * x);
                    e_out += f64::from(y * y);
                }
            }
            eprintln!(
                "bands {freq:>6} Hz: {:+.3} dB",
                10.0 * (e_out / e_in).log10()
            );
        }
        // And the peaks of the incoming track through them, 50 to 65 s in.
        let ch = inc.channels;
        let mut b = Bands::new(ch, inc.rate);
        let (mut p_in, mut p_out) = (0.0_f32, 0.0_f32);
        let from = 50 * inc.rate as usize;
        for f in 0..(15 * inc.rate as usize).min(inc.frames() - from) {
            for c in 0..ch {
                let x = inc.pcm[(from + f) * ch + c];
                let y: f32 = b.split(c, x).iter().sum();
                p_in = p_in.max(x.abs());
                if f > 4800 {
                    p_out = p_out.max(y.abs());
                }
            }
        }
        eprintln!("bands on the incoming track: peak {p_in:.3} in, {p_out:.3} out");
    }
    if std::env::var_os("AUTOMIX_SECTIONS").is_some() {
        let od = beat_db(&out, &out.beats);
        let n = od.len();
        for k in n.saturating_sub(48)..n {
            eprintln!("out {k}: {:.2} s {:+.1} dB", out.beats[k], od[k]);
        }
        for (name, t) in [("out", &out), ("in", &inc)] {
            let nov = novelty(&beat_features(t, &t.beats), 8);
            let mut peaks: Vec<(usize, f64)> = (1..nov.len() - 1)
                .filter(|&k| nov[k] >= nov[k - 1] && nov[k] > nov[k + 1])
                .map(|k| (k, nov[k]))
                .collect();
            peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut top: Vec<(usize, f64)> = peaks.into_iter().take(14).collect();
            top.sort_by_key(|p| p.0);
            let line: Vec<String> = top
                .iter()
                .map(|(k, v)| format!("{k}@{:.1}s={v:.2}", t.beats[*k]))
                .collect();
            eprintln!("novelty {name}: {}", line.join("  "));
        }
        let id = beat_db(&inc, &inc.beats);
        for row in id.chunks(16).take(12).enumerate() {
            let line: Vec<String> = row.1.iter().map(|d| format!("{d:.0}")).collect();
            eprintln!(
                "in {:3} ({:6.2} s): {}",
                row.0 * 16,
                inc.beats[row.0 * 16],
                line.join(" ")
            );
        }
    }
    assert_eq!(
        (out.rate, out.channels),
        (inc.rate, inc.channels),
        "both files must share a rate and a channel count"
    );
    std::fs::create_dir_all(dir).expect("out dir");
    let cap = cap_ms as f64 / 1000.0;

    let mut report = String::new();
    for t in [&out, &inc] {
        let _ = writeln!(
            report,
            "{}: {:.1} BPM, energy {:.2}, {} beats, {:.1} s, gain {:+.1} dB",
            t.path.display(),
            t.bpm.unwrap_or(0.0),
            t.energy.unwrap_or(0.0),
            t.beats.len(),
            t.duration(),
            20.0 * t.gain.log10()
        );
    }
    let n = out.beats.len();
    let ends = [
        (
            "outgoing, last 32 beats",
            &out,
            &out.beats[n.saturating_sub(FIT_BEATS)..],
        ),
        (
            "incoming, first 32 beats",
            &inc,
            &inc.beats[..inc.beats.len().min(FIT_BEATS)],
        ),
    ];
    for (what, t, run) in ends {
        let phase = Grid::fit(run).and_then(|g| Some((g.period, kick_phase_ms(t, run, g.period)?)));
        match phase {
            Some((p, ms)) => {
                let _ = writeln!(
                    report,
                    "kick against the beats, {what}: {ms:+.1} ms ({:+.2} of a beat)",
                    ms / (p * 1000.0)
                );
            }
            None => {
                let _ = writeln!(report, "kick against the beats, {what}: not measured");
            }
        }
    }
    let _ = writeln!(report, "crossfade length {cap} s");
    let shift = match phase_shift(&out, &inc) {
        Some((s, strength)) => {
            let _ = writeln!(
                report,
                "rhythm profiles: incoming moved {s:+.2} of a beat (profiles correlate at {strength:.2})"
            );
            s
        }
        None => 0.0,
    };
    let _ = writeln!(report);
    let styles = [
        Style::Current,
        Style::TempoIn,
        Style::TempoInPhase,
        Style::TempoOut,
        Style::Echo,
        Style::Loop,
        Style::Stutter,
        Style::Filter,
        Style::BandSwap,
    ];
    let styles: Vec<Style> = styles
        .into_iter()
        .chain(entry.map(Style::BandSwapAt))
        .collect();
    for (n, style) in styles.into_iter().enumerate() {
        let name = format!("{}-{}", n + 1, style.name());
        match render(&out, &inc, style, cap, shift) {
            Ok(r) => {
                let path = dir.join(format!("{name}.wav"));
                write_wav(&path, &r.samples, out.rate, out.channels).expect("write");
                let _ = writeln!(
                    report,
                    "{name}: fade {} → {}, incoming tempo settled {} — {}; attacks {}",
                    mmss(r.fade_at),
                    mmss(r.fade_end_at),
                    mmss(r.settled_at),
                    r.layout_note,
                    r.lag_ms
                        .map_or("not measured".into(), |l| format!("{l:+.1} ms apart")),
                );
            }
            Err(e) => {
                let _ = writeln!(report, "{name}: not rendered — {e}");
            }
        }
    }
    print!("{report}");
    std::fs::write(dir.join("plan.txt"), report).expect("plan.txt");
}
