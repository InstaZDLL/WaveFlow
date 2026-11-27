//! Render AutoMix transitions between two files to WAV, to listen to them.
//!
//! ```text
//! cargo run --release -p waveflow-core --features playback --example automix_render -- <outgoing> <incoming> <out dir> [options]
//!
//!   --cap-ms N        crossfade length the transition must fit in (12000)
//!   --no-gain         no ReplayGain (track gain is applied otherwise)
//!   --trim-db dB      a gain on both, to match the level of a recording
//!   --entry-beat N    enter the incoming track on this beat only
//!   --window S,S      seconds shown before and after the transition (15,8)
//! ```
//!
//! Both files are analysed with the app's own code (`analysis::analyze_file`:
//! the beat grid the library stores) and decoded through the app's codec
//! registry. The planner ([`waveflow_core::automix::transition::planner`])
//! then lays out every template on every way out and in it finds, scores
//! them and keeps the best, or decides not to mix. Written to `<out dir>`:
//!
//! - `0-choice-<template>.wav`, or `0-no-mix.wav` (the plain crossfade the
//!   player falls back to) when the planner refuses;
//! - `<n>-<template>.wav`: each template at its own best candidate;
//! - `report.txt`: the points found, every candidate with its score, the
//!   reasons the others could not be laid out, the choice or the refusal.
//!
//! Everything a transition does goes through the bricks of
//! `automix::transition`: band gains, ramps, the bass cut, effects, the
//! power kept through the overlap and the last-resort limiter, whose count
//! of frames touched is in the report. The stretching goes through the
//! playback stretcher, a tempo per block.

use std::collections::BTreeMap;
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
use waveflow_core::automix::transition::bands::{band_powers, BandGains, MAX_BANDS};
use waveflow_core::automix::transition::envelope::{Ramp, Shape};
use waveflow_core::automix::transition::fx::{loop_segments, Echo, Sweep, LOOP_EDGE_S};
use waveflow_core::automix::transition::level::{keep_power, Limiter};
use waveflow_core::automix::transition::planner::{self, Candidate, Layout, Plan, PlanRules};
use waveflow_core::automix::transition::points::{self, beat_levels};
use waveflow_core::automix::transition::template::{self, Effect, Gains, Template};
use waveflow_core::automix::transition::tempo::Glide;
use waveflow_core::playback::stretch::Stretcher;

/// Source frames the stretcher is fed at one tempo.
const BLOCK: usize = 256;

/// The limiter that catches what the headroom did not: under full scale,
/// 1.5 ms ahead, back over 50 ms.
const CEILING: f32 = 0.98;
const LOOKAHEAD_S: f64 = 0.0015;
const RELEASE_S: f64 = 0.05;

/// The most the overlap is turned down by, linear (-6 dB).
const HEADROOM_FLOOR: f64 = 0.5;

struct Track {
    path: PathBuf,
    rate: u32,
    channels: usize,
    pcm: Vec<f32>,
    beats: Vec<f64>,
    levels: Vec<f64>,
    bpm: Option<f64>,
    gain: f32,
}

impl Track {
    fn frames(&self) -> usize {
        self.pcm.len() / self.channels
    }
    fn duration(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }
    fn view(&self) -> planner::Track<'_> {
        planner::Track {
            beats: &self.beats,
            levels: &self.levels,
            duration: self.duration(),
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

fn load(path: &Path, gain: bool, trim_db: f32) -> Result<Track, String> {
    let analysis = analyze_file(path)?;
    let (rate, channels, pcm) = decode(path)?;
    let gain_db = if gain {
        analysis.replay_gain_db.unwrap_or(0.0) as f32
    } else {
        0.0
    };
    let levels = beat_levels(&pcm, channels, rate, &analysis.beats);
    Ok(Track {
        path: path.to_path_buf(),
        rate,
        channels,
        pcm,
        levels,
        beats: analysis.beats,
        bpm: analysis.bpm,
        gain: 10f32.powf((gain_db + trim_db) / 20.0),
    })
}

/// `frames` output frames of `pcm` from track frame `from`, following
/// `glide` over the track's own time. A glide at 1 throughout copies.
fn play(
    pcm: &[f32],
    channels: usize,
    rate: u32,
    from: usize,
    glide: Glide,
    frames: usize,
) -> Vec<f32> {
    if glide == Glide::constant(1.0) {
        let mut out: Vec<f32> = pcm
            .get(from * channels..)
            .unwrap_or(&[])
            .iter()
            .take(frames * channels)
            .copied()
            .collect();
        out.resize(frames * channels, 0.0);
        return out;
    }
    let at = |f: usize| glide.speed(f as f64 / f64::from(rate));
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

/// The outgoing track as the transition plays it: up to the end of its
/// music, then the loops of an [`Effect::Loop`], each faded in and out at
/// its edges.
fn outgoing_source(out: &Track, t: &Template, l: &Layout) -> Vec<f32> {
    let Effect::Loop {
        back_beats,
        lengths,
    } = &t.effect
    else {
        return out.pcm.clone();
    };
    let rate = f64::from(out.rate);
    let ch = out.channels;
    let end = ((l.out_end * rate) as usize).min(out.frames());
    let mut pcm = out.pcm[..end * ch].to_vec();
    let edge = (LOOP_EDGE_S * rate) as usize;
    for (a, b) in loop_segments(l.out_end, l.out_period, *back_beats, lengths) {
        let (a, b) = ((a * rate).round() as usize, (b * rate).round() as usize);
        let n = b - a;
        for i in 0..n {
            let g = (i.min(n - i) as f32 / edge.max(1) as f32).min(1.0);
            for c in 0..ch {
                pcm.push(out.pcm.get((a + i) * ch + c).copied().unwrap_or(0.0) * g);
            }
        }
    }
    pcm
}

/// `side` through band gains that follow `gains_at(frame)`, lined up with
/// its input again (the window's latency taken out).
fn through_bands(
    side: &[f32],
    channels: usize,
    rate: u32,
    cutoffs: &[f64],
    gains_at: impl Fn(usize) -> [f32; MAX_BANDS],
) -> Vec<f32> {
    let mut b = BandGains::new(cutoffs, channels, rate);
    let latency = b.latency();
    let frames = side.len() / channels;
    let mut out = vec![0.0; side.len()];
    let zero = vec![0.0; channels];
    let mut y = vec![0.0; channels];
    for f in 0..frames + latency {
        // A window covers the last `latency` frames: its gains are those of
        // its middle.
        b.set_gains(&gains_at(f.saturating_sub(latency / 2)));
        let x = if f < frames {
            &side[f * channels..(f + 1) * channels]
        } else {
            &zero[..]
        };
        b.frame(x, &mut y);
        if f >= latency {
            out[(f - latency) * channels..(f - latency + 1) * channels].copy_from_slice(&y);
        }
    }
    out
}

struct Rendered {
    samples: Vec<f32>,
    fade_at: f64,
    fade_end_at: f64,
    settled_at: f64,
    /// Frames the limiter turned down, the least gain the headroom used.
    limited: u64,
    headroom: f32,
}

fn render(out: &Track, inc: &Track, t: &Template, l: &Layout, window: (f64, f64)) -> Rendered {
    let rate = out.rate;
    let ch = out.channels;
    let r = f64::from(rate);
    let start = (l.fade_start - window.0).max(0.0);
    let stop = l.settled.max(l.fade_end) + window.1;
    let frames = ((stop - start) * r) as usize;
    let fr = |t: f64| ((t - start) * r).round().max(0.0) as usize;

    let source = outgoing_source(out, t, l);
    let out_from = l.out_glide.track_time(0.0, start);
    let o = play(
        &source,
        ch,
        rate,
        (out_from * r).round() as usize,
        l.out_glide,
        frames,
    );
    let in_at = fr(l.fade_start).min(frames);
    let mut i = vec![0.0; in_at * ch];
    i.extend(play(
        &inc.pcm,
        ch,
        rate,
        (l.in_from * r).round() as usize,
        l.in_glide,
        frames - in_at,
    ));

    let f0 = fr(l.fade_start);
    let f1 = fr(l.fade_end).max(f0 + 1);
    let u = move |f: usize| ((f as f64 - f0 as f64) / (f1 - f0) as f64).clamp(0.0, 1.0);
    let beat = (l.period * r).max(1.0);
    let cut = l.bass_cut_at.map(fr);

    // Gains per band and side at a frame; one band for whole-signal gains.
    let (cutoffs, out_ramps, in_ramps): (Vec<f64>, Vec<Ramp>, Vec<Ramp>) = match &t.gains {
        Gains::Whole { out, inc } => (Vec::new(), vec![*out], vec![*inc]),
        Gains::Bands { cutoffs, out, inc } => (cutoffs.clone(), out.clone(), inc.clone()),
    };
    let bands = out_ramps.len();
    let bass = t.bass_cut;
    let cut_frames = bass.map_or(1, |b| ((b.fade_s * r) as usize).max(1));
    let out_gain = |f: usize| -> [f32; MAX_BANDS] {
        let mut g = [0.0; MAX_BANDS];
        for (b, ramp) in out_ramps.iter().enumerate() {
            g[b] = if f >= f1 {
                0.0
            } else if f < f0 {
                1.0
            } else {
                ramp.fall(u(f)) as f32
            };
            if let (Some(c), Some(bc)) = (cut, bass) {
                if b <= bc.top_band {
                    let left = 1.0 - (f.saturating_sub(c) as f32 / cut_frames as f32).min(1.0);
                    g[b] = if f >= f1 { 0.0 } else { left };
                }
            }
        }
        g
    };
    let in_gain = |f: usize| -> [f32; MAX_BANDS] {
        let mut g = [0.0; MAX_BANDS];
        for (b, ramp) in in_ramps.iter().enumerate() {
            g[b] = if f < f0 { 0.0 } else { ramp.rise(u(f)) as f32 };
        }
        g
    };

    // Each side's power per band over the transition, before any gain.
    let powers = |side: &[f32]| -> Vec<f64> {
        if cutoffs.is_empty() {
            let s = &side[f0 * ch..(f1 * ch).min(side.len())];
            vec![s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len().max(1) as f64]
        } else {
            band_powers(side, ch, rate, &cutoffs, f0, f1)
        }
    };
    // At the gains the mix plays them at (ReplayGain and trim).
    let scaled = |p: Vec<f64>, g: f32| -> Vec<f64> {
        p.into_iter().map(|x| x * f64::from(g).powi(2)).collect()
    };
    let (p_out, p_in) = (scaled(powers(&o), out.gain), scaled(powers(&i), inc.gain));

    // The sides through their gains: band gains in the spectrum, whole
    // gains per sample.
    let (o_g, i_g) = if cutoffs.is_empty() {
        let apply = |side: &[f32], g: &dyn Fn(usize) -> [f32; MAX_BANDS]| -> Vec<f32> {
            side.iter()
                .enumerate()
                .map(|(k, x)| x * g(k / ch)[0])
                .collect()
        };
        (apply(&o, &out_gain), apply(&i, &in_gain))
    } else {
        (
            through_bands(&o, ch, rate, &cutoffs, out_gain),
            through_bands(&i, ch, rate, &cutoffs, in_gain),
        )
    };

    // Effects.
    let mut echo = match &t.effect {
        Effect::Echo {
            delay_beats,
            feedback,
            low_cut,
            high_cut,
            ..
        } => Some(Echo::new(
            (delay_beats * beat).round() as usize,
            ch,
            rate,
            *feedback,
            *low_cut,
            *high_cut,
        )),
        _ => None,
    };
    let mut sweeps = match &t.effect {
        Effect::Sweep {
            out_high_pass,
            in_low_pass,
            in_open,
            q,
        } => Some((
            Sweep::new(
                true,
                out_high_pass.0,
                out_high_pass.1,
                Ramp::new(0.0, 1.0, Shape::Linear),
                *q,
                ch,
                rate,
            ),
            Sweep::new(false, in_low_pass.0, in_low_pass.1, *in_open, *q, ch, rate),
        )),
        _ => None,
    };

    let mut limiter = Limiter::new(ch, rate, CEILING, LOOKAHEAD_S, RELEASE_S);
    let latency = limiter.latency();
    let mut mix = vec![0.0_f32; frames * ch];
    let mut send = vec![0.0; ch];
    let mut wet = vec![0.0; ch];
    let mut io = vec![0.0; ch];
    let mut headroom = 1.0_f32;
    let mut least = 1.0_f32;
    for f in 0..frames + latency {
        if f < frames {
            if f % 64 == 0 {
                headroom = if (f0..f1).contains(&f) {
                    keep_power(
                        &out_gain(f)[..bands],
                        &in_gain(f)[..bands],
                        &p_out,
                        &p_in,
                        t.headroom,
                        HEADROOM_FLOOR,
                    )
                } else {
                    1.0
                };
                least = least.min(headroom);
            }
            let (g_send, wet_gain) = match &t.effect {
                Effect::Echo {
                    send_beats, wet, ..
                } if f + (send_beats * beat) as usize >= f1 && f < f1 => (out_gain(f)[0], *wet),
                Effect::Echo { wet, .. } => (0.0, *wet),
                _ => (0.0, 0.0),
            };
            if let Some(s) = sweeps.as_mut() {
                s.0.at(u(f));
                s.1.at(u(f));
            }
            for c in 0..ch {
                let mut so = o_g[f * ch + c];
                let mut si = i_g[f * ch + c];
                if let Some(s) = sweeps.as_mut() {
                    if f >= f0 {
                        so = s.0.run(c, so);
                        if f < f1 {
                            si = s.1.run(c, si);
                        }
                    }
                }
                // What the echo is fed is the outgoing track before its
                // gain, at the gain it had: its last beat as heard.
                send[c] = o[f * ch + c] * g_send * out.gain;
                io[c] = (so * out.gain + si * inc.gain) * headroom;
            }
            if let Some(e) = echo.as_mut() {
                e.frame(&send, &mut wet);
                for c in 0..ch {
                    io[c] += wet[c] * wet_gain * headroom;
                }
            }
        } else {
            io.fill(0.0);
        }
        limiter.frame(&mut io);
        if f >= latency {
            for c in 0..ch {
                mix[(f - latency) * ch + c] = io[c].clamp(-1.0, 1.0);
            }
        }
    }
    Rendered {
        samples: mix,
        fade_at: l.fade_start - start,
        fade_end_at: l.fade_end - start,
        settled_at: l.settled - start,
        limited: limiter.touched(),
        headroom: least,
    }
}

/// The plain crossfade the player makes when the planner refuses: the last
/// `cap` seconds of the outgoing file (at most half of it), equal power.
fn no_mix_layout(out: &Track, cap: f64) -> Layout {
    let end = out.duration();
    let cap = cap.min(end / 2.0);
    Layout {
        fade_start: end - cap,
        fade_end: end,
        period: 0.5,
        beats: 0,
        fold: 1.0,
        ratio: 1.0,
        out_glide: Glide::constant(1.0),
        in_glide: Glide::constant(1.0),
        in_from: 0.0,
        out_end: end,
        out_period: 0.5,
        bass_cut_at: None,
        settled: end,
        drift_ms: 0.0,
    }
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

fn describe(c: &Candidate, out: &Track, inc: &Track) -> String {
    let l = &c.layout;
    let entry = match c.entry.beat {
        None => format!("from its start (intro to beat {})", c.entry.intro_end),
        Some(k) => format!("on its beat {k} ({:.2} s)", inc.beats[k]),
    };
    let exit_start = out.beats.get(c.exit.start).copied().unwrap_or(0.0);
    let s = c.score;
    format!(
        "score {:.3} (grid {:.2}, tempo {:.2}, exit {:.2}, entry {:.2}, energy {:.2}) — \
         {} beats, gap {:+.2} %, fold {}, drift {:.1} ms; \
         out: music ends on beat {} ({:+.1} dB drop), {:?} start on beat {} ({:.2} s, {:+.1} dB); in: {entry}",
        s.total,
        s.grid,
        s.tempo,
        s.exit,
        s.entry,
        s.energy,
        l.beats,
        (l.ratio - 1.0) * 100.0,
        l.fold,
        l.drift_ms,
        c.exit.end,
        c.exit.end_drop_db,
        c.exit.kind,
        c.exit.start,
        exit_start,
        c.exit.dip_db,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_one(
    dir: &Path,
    name: &str,
    out: &Track,
    inc: &Track,
    t: &Template,
    l: &Layout,
    window: (f64, f64),
    report: &mut String,
) {
    let r = render(out, inc, t, l, window);
    write_wav(
        &dir.join(format!("{name}.wav")),
        &r.samples,
        out.rate,
        out.channels,
    )
    .expect("write");
    let _ = writeln!(
        report,
        "  {name}.wav: transition {} → {}, incoming tempo settled {}; headroom down to {:+.1} dB, limiter on {} frames",
        mmss(r.fade_at),
        mmss(r.fade_end_at),
        mmss(r.settled_at),
        20.0 * r.headroom.log10(),
        r.limited
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut cap_ms = 12_000u64;
    let mut gain = true;
    let mut trim_db = 0.0_f32;
    let mut window = (15.0, 8.0);
    let mut rules = PlanRules::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--cap-ms" => cap_ms = value().parse().expect("--cap-ms N"),
            "--no-gain" => gain = false,
            "--trim-db" => trim_db = value().parse().expect("--trim-db dB"),
            "--entry-beat" => {
                rules.points.only_entry = Some(value().parse().expect("--entry-beat N"))
            }
            "--window" => {
                let v = value();
                let (a, b) = v.split_once(',').expect("--window S,S");
                window = (a.parse().expect("seconds"), b.parse().expect("seconds"));
            }
            _ => positional.push(PathBuf::from(a)),
        }
    }
    let [out_path, in_path, dir] = positional.as_slice() else {
        eprintln!("usage: automix_render <outgoing> <incoming> <out dir> [--cap-ms N] [--no-gain] [--trim-db dB] [--entry-beat N] [--window S,S]");
        std::process::exit(2);
    };
    let out = load(out_path, gain, trim_db).expect("outgoing");
    let inc = load(in_path, gain, trim_db).expect("incoming");
    assert_eq!(
        (out.rate, out.channels),
        (inc.rate, inc.channels),
        "both files must share a rate and a channel count"
    );
    std::fs::create_dir_all(dir).expect("out dir");
    let cap = cap_ms as f64 / 1000.0;
    let templates = template::builtin();

    let mut report = String::new();
    for (side, t) in [("outgoing", &out), ("incoming", &inc)] {
        let _ = writeln!(
            report,
            "{side}: {} — {:.1} BPM, {} beats, {:.1} s, gain {:+.1} dB",
            t.path.display(),
            t.bpm.unwrap_or(0.0),
            t.beats.len(),
            t.duration(),
            20.0 * t.gain.log10()
        );
    }
    let _ = writeln!(report, "crossfade length {cap} s\n");
    for e in points::exits(&out.levels, &rules.points, 16) {
        let _ = writeln!(
            report,
            "exit: music ends on beat {} ({:+.1} dB drop), {:?} start on beat {} ({:+.1} dB)",
            e.end, e.end_drop_db, e.kind, e.start, e.dip_db
        );
    }
    for e in points::entries(&inc.levels, &rules.points) {
        let _ = writeln!(
            report,
            "entry: {:?} (intro to beat {})",
            e.beat, e.intro_end
        );
    }

    let plan: Plan = planner::plan(out.view(), inc.view(), cap, &templates, &rules);
    let _ = writeln!(report, "\ncandidates, best first:");
    for c in &plan.candidates {
        let _ = writeln!(
            report,
            "  {}: {}",
            templates[c.template].name,
            describe(c, &out, &inc)
        );
    }
    let _ = writeln!(report, "\nnot laid out:");
    let mut reasons: BTreeMap<(usize, String), usize> = BTreeMap::new();
    for r in &plan.rejected {
        *reasons.entry((r.template, r.reason.clone())).or_default() += 1;
    }
    for ((t, why), n) in reasons {
        let _ = writeln!(report, "  {}: {why} (×{n})", templates[t].name);
    }

    let _ = writeln!(report, "\nchoice:");
    match &plan.choice {
        Ok(k) => {
            let c = &plan.candidates[*k];
            let t = &templates[c.template];
            let _ = writeln!(report, "  mix with {}", t.name);
            write_one(
                dir,
                &format!("0-choice-{}", t.name),
                &out,
                &inc,
                t,
                &c.layout,
                window,
                &mut report,
            );
        }
        Err(why) => {
            let _ = writeln!(report, "  no mix: {why}; the plain crossfade instead");
            let plain = &templates[0];
            write_one(
                dir,
                "0-no-mix",
                &out,
                &inc,
                plain,
                &no_mix_layout(&out, cap),
                window,
                &mut report,
            );
        }
    }

    let _ = writeln!(report, "\neach template at its best:");
    for (k, t) in templates.iter().enumerate() {
        match plan.candidates.iter().find(|c| c.template == k) {
            Some(c) => write_one(
                dir,
                &format!("{}-{}", k + 1, t.name),
                &out,
                &inc,
                t,
                &c.layout,
                window,
                &mut report,
            ),
            None => {
                let _ = writeln!(report, "  {}-{}: nothing laid out", k + 1, t.name);
            }
        }
    }
    print!("{report}");
    std::fs::write(dir.join("report.txt"), report).expect("report.txt");
}
