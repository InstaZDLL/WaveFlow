//! The measure behind adopting a decoder: what it outputs, byte for byte, to
//! hold against the format's reference tool.
//!
//! `Ok` proves nothing about a decoder, so the WavPack and Monkey's Audio
//! crates were taken on this and not on their READMEs, and a new version of
//! either is measured again the same way. Opens a file exactly as playback
//! does, through `audio_format::probe()` and `audio_format::opus::codecs()`.
//!
//! ```text
//! cargo run --release -p waveflow-core --example format_conformance -- decode in.wv out.raw
//! wvunpack --raw in.wv -o ref.raw     # mac in.ape ref.wav -d, for Monkey's Audio
//! cmp out.raw ref.raw
//! cargo run --release -p waveflow-core --example format_conformance -- seek in.wv
//! ```
//!
//! `decode` writes interleaved little-endian PCM at the file's own width,
//! 8-bit as unsigned, which is what the reference tools write. `seek` lands
//! on frames spread through the file and compares what follows with a full
//! decode, then reports how long the first packet took to arrive. `pace`
//! pulls packets at a given multiple of real time and reports the longest any
//! of them made the caller wait, which is what has to stay under the output
//! ring's length.

use std::fs::File;
use std::time::Instant;

use symphonia::core::audio::sample::i24;
use symphonia::core::audio::{AudioBytes, GenericAudioBufferRef};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

struct Opened {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    channels: usize,
    rate: u32,
    declared_frames: Option<u64>,
}

fn open(path: &str) -> Result<Opened, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let format = waveflow_core::audio_format::probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let declared_frames = track.num_frames;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio params")?
        .clone();
    let decoder = waveflow_core::audio_format::opus::codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;
    Ok(Opened {
        channels: params.channels.as_ref().map_or(0, |c| c.count()),
        rate: params.sample_rate.unwrap_or(0),
        declared_frames,
        format,
        decoder,
    })
}

/// Append a decoded buffer as raw PCM; the bytes per frame are the return.
fn append(buf: &GenericAudioBufferRef<'_>, out: &mut Vec<u8>) -> usize {
    macro_rules! bytes {
        ($b:expr, $s:ty) => {{
            let mut raw = vec![0u8; $b.byte_len_as::<$s>()];
            $b.copy_bytes_interleaved_as::<$s, _>(&mut raw[..]);
            out.extend_from_slice(&raw);
            $b.byte_len_per_frame_as::<$s>()
        }};
    }
    match buf {
        GenericAudioBufferRef::U8(b) => bytes!(b, u8),
        GenericAudioBufferRef::S8(b) => bytes!(b, u8),
        GenericAudioBufferRef::S16(b) => bytes!(b, i16),
        GenericAudioBufferRef::S24(b) => bytes!(b, i24),
        GenericAudioBufferRef::S32(b) => bytes!(b, i32),
        GenericAudioBufferRef::F32(b) => bytes!(b, f32),
        _ => panic!("sample format this tool does not write"),
    }
}

/// Decode to the end of the stream, or until `limit` bytes are in. Answers
/// the bytes per frame, and the longest any single packet took.
fn decode(opened: &mut Opened, limit: usize) -> Result<(Vec<u8>, usize, f64), String> {
    let mut pcm = Vec::new();
    let mut frame_bytes = 0;
    let mut slowest = 0f64;
    loop {
        let started = Instant::now();
        let Some(packet) = opened
            .format
            .next_packet()
            .map_err(|e| format!("next_packet: {e}"))?
        else {
            return Ok((pcm, frame_bytes, slowest));
        };
        let decoded = opened
            .decoder
            .decode(&packet)
            .map_err(|e| format!("decode: {e}"))?;
        frame_bytes = append(&decoded, &mut pcm);
        slowest = slowest.max(started.elapsed().as_secs_f64());
        if pcm.len() >= limit {
            return Ok((pcm, frame_bytes, slowest));
        }
    }
}

fn run_decode(path: &str, out_path: &str) -> Result<(), String> {
    let mut opened = open(path)?;
    let started = Instant::now();
    let (pcm, frame_bytes, slowest) = decode(&mut opened, usize::MAX)?;
    let elapsed = started.elapsed().as_secs_f64();
    let frames = (pcm.len() / frame_bytes.max(1)) as u64;
    std::fs::write(out_path, &pcm).map_err(|e| e.to_string())?;
    println!(
        "frames={frames} declared={:?} channels={} rate={} realtime={:.1}x slowest_packet={:.1}ms",
        opened.declared_frames,
        opened.channels,
        opened.rate,
        frames as f64 / f64::from(opened.rate.max(1)) / elapsed.max(1e-9),
        slowest * 1000.0,
    );
    if opened.declared_frames.is_some_and(|d| d != frames) {
        return Err("decoded length differs from the declared one".into());
    }
    Ok(())
}

fn run_seek(path: &str) -> Result<(), String> {
    let mut opened = open(path)?;
    let (full, frame_bytes, _) = decode(&mut opened, usize::MAX)?;
    let total = (full.len() / frame_bytes.max(1)) as u64;
    if total == 0 {
        return Err("no audio".into());
    }
    let compare_frames = 2048u64;
    // Spread through the file by an odd stride so block and frame boundaries
    // are crossed, then the two ends.
    let mut targets: Vec<u64> = (1..40u64).map(|i| i * total / 40 + i * 7).collect();
    targets.extend([
        0,
        total / 2,
        total / 2 + 1,
        total - 1,
        total - compare_frames.min(total),
    ]);

    let mut worst = 0f64;
    let mut mismatches = 0;
    for target in &targets {
        let target = (*target).min(total - 1);
        opened
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(target as i64),
                    track_id: 0,
                },
            )
            .map_err(|e| format!("seek to {target}: {e}"))?;
        opened.decoder.reset();
        let want = (compare_frames.min(total - target) as usize) * frame_bytes;
        let started = Instant::now();
        let (got, _, _) = decode(&mut opened, want)?;
        worst = worst.max(started.elapsed().as_secs_f64());
        let start = target as usize * frame_bytes;
        if got.len() < want || got[..want] != full[start..start + want] {
            mismatches += 1;
            println!("MISMATCH landing on frame {target}");
        }
    }
    println!(
        "seeks={} mismatches={mismatches} slowest_to_audio={:.1}ms",
        targets.len(),
        worst * 1000.0
    );
    if mismatches > 0 {
        return Err("a seek landed on the wrong audio".into());
    }
    Ok(())
}

/// What the analysis pass makes of the file, and how long it takes.
fn run_analyze(path: &str) -> Result<(), String> {
    let started = Instant::now();
    let analysis = waveflow_core::analysis::analyze_file(std::path::Path::new(path))?;
    println!(
        "peak={:.4} loudness={:?} bpm={:?} took={:.1}s",
        analysis.peak,
        analysis.loudness_lufs,
        analysis.bpm,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Pull packets at `speed` times real time, as the decoder thread does while a
/// track plays, and report how long any packet kept it waiting. The first is
/// left out: nothing is playing yet when it is asked for.
fn run_pace(path: &str, speed: f64) -> Result<(), String> {
    let mut opened = open(path)?;
    let rate = f64::from(opened.rate.max(1));
    let started = Instant::now();
    let mut played = 0f64;
    let mut slowest = 0f64;
    let mut first = true;
    let mut pcm = Vec::new();
    loop {
        let asked = Instant::now();
        let Some(packet) = opened
            .format
            .next_packet()
            .map_err(|e| format!("next_packet: {e}"))?
        else {
            break;
        };
        let decoded = opened
            .decoder
            .decode(&packet)
            .map_err(|e| format!("decode: {e}"))?;
        pcm.clear();
        append(&decoded, &mut pcm);
        played += decoded.frames() as f64 / rate;
        if !first {
            slowest = slowest.max(asked.elapsed().as_secs_f64());
        }
        first = false;
        let due = played / speed;
        let now = started.elapsed().as_secs_f64();
        if due > now {
            std::thread::sleep(std::time::Duration::from_secs_f64(due - now));
        }
    }
    println!(
        "played={played:.1}s speed={speed}x slowest_packet={:.2}ms",
        slowest * 1000.0
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match (args.get(1).map(String::as_str), args.get(2), args.get(3)) {
        (Some("decode"), Some(path), Some(out)) => run_decode(path, out),
        (Some("seek"), Some(path), _) => run_seek(path),
        (Some("analyze"), Some(path), _) => run_analyze(path),
        (Some("pace"), Some(path), speed) => {
            run_pace(path, speed.and_then(|s| s.parse().ok()).unwrap_or(1.0))
        }
        _ => Err(
            "usage: format_conformance decode <in> <out.raw> | seek <in> | pace <in> [speed] | analyze <in>"
                .into(),
        ),
    };
    if let Err(reason) = result {
        println!("ERR {reason}");
        std::process::exit(1);
    }
}
