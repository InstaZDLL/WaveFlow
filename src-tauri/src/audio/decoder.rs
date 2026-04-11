//! Symphonia-based decoder thread.
//!
//! Blocks on the `AudioCmd` channel when Idle. On `LoadAndPlay`, opens
//! the file with symphonia, decodes packets in a loop, converts the
//! sample format / channel layout / sample rate to match the cpal
//! device config, and pushes interleaved f32 samples into the SPSC ring
//! feeding the audio callback.
//!
//! Commands are polled between packets via `cmd_rx.try_recv()` so
//! pause / stop / seek feel responsive even during long tracks.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::{chunks::ChunkError, CopyToUninit, Producer};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::engine::AudioCmd;
use super::resampler::Resampler;
use super::state::{PlayerState, SharedPlayback};

/// Spawn the decoder thread.
///
/// Takes ownership of:
/// - the command receiver,
/// - the rtrb producer feeding the cpal callback,
/// - a clone of [`SharedPlayback`] for state transitions and volume.
///
/// The device's sample rate and channel count are read from `shared`
/// after [`super::output::spawn_output_thread`] has stamped them in.
pub fn spawn_decoder_thread(
    cmd_rx: Receiver<AudioCmd>,
    mut producer: Producer<f32>,
    shared: Arc<SharedPlayback>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("waveflow-audio-decoder".into())
        .spawn(move || {
            decoder_loop(cmd_rx, &mut producer, shared);
        })
}

/// Top-level decoder thread loop. Never returns except on `Shutdown`.
fn decoder_loop(
    cmd_rx: Receiver<AudioCmd>,
    producer: &mut Producer<f32>,
    shared: Arc<SharedPlayback>,
) {
    loop {
        // Block until there's something to do. Idle = cheap.
        let cmd = match cmd_rx.recv() {
            Ok(c) => c,
            Err(_) => return, // sender dropped; engine shutting down
        };

        match cmd {
            AudioCmd::LoadAndPlay {
                path,
                start_ms,
                track_id,
                duration_ms,
            } => {
                shared.set_state(PlayerState::Loading);
                // Reset position counters so the UI clock starts from 0
                // (or from start_ms on a mid-track resume).
                shared.samples_played.store(0, Ordering::Relaxed);
                shared.base_offset_ms.store(start_ms, Ordering::Relaxed);

                if let Err(err) = play_track(
                    &path,
                    start_ms,
                    track_id,
                    duration_ms,
                    producer,
                    &shared,
                    &cmd_rx,
                ) {
                    tracing::warn!(?err, path = %path.display(), "playback failed");
                    shared.set_state(PlayerState::Idle);
                }
            }
            AudioCmd::Shutdown => return,
            // All other commands are no-ops when no track is playing —
            // they're only meaningful inside `play_track`'s inner loop.
            _ => {}
        }
    }
}

/// Result of [`push_samples`]. `Ok` means all samples were written;
/// any other variant signals that the outer loop should stop pushing
/// and react (propagate shutdown, end the track, or apply a seek).
enum PushOutcome {
    Ok,
    Stop,
    Shutdown,
    Seek(u64),
}

/// Decode a single track start-to-finish, honoring any commands that
/// arrive on `cmd_rx` between packets.
fn play_track(
    path: &Path,
    start_ms: u64,
    _track_id: i64,
    _duration_ms: u64,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    cmd_rx: &Receiver<AudioCmd>,
) -> Result<(), String> {
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
        .ok_or_else(|| "no decodable track found".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec init: {e}"))?;

    let src_sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| "no sample rate in codec params".to_string())?;
    let src_channels = codec_params
        .channels
        .ok_or_else(|| "no channel layout in codec params".to_string())?
        .count();

    let dst_sample_rate = shared.sample_rate.load(Ordering::Relaxed);
    let dst_channels = shared.channels.load(Ordering::Relaxed) as usize;
    if dst_sample_rate == 0 || dst_channels == 0 {
        return Err("cpal output not initialized (sample_rate=0)".into());
    }

    tracing::info!(
        src_sample_rate,
        src_channels,
        dst_sample_rate,
        dst_channels,
        path = %path.display(),
        "decoding start"
    );

    // If the caller asked for a mid-track start (resume from persisted
    // position), apply an initial seek BEFORE entering the packet loop.
    if start_ms > 0 {
        apply_seek(&mut format, track_id, start_ms);
    }

    // Channel conversion: we always emit `dst_channels`-wide frames.
    // Resampler works per destination channel count — we convert first
    // (so it sees a uniform layout), then resample.
    let mut resampler = Resampler::new(src_sample_rate, dst_sample_rate, dst_channels)
        .map_err(|e| format!("resampler: {e}"))?;

    let mut interleaved_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut resampled_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    shared.set_state(PlayerState::Playing);

    'pkt: loop {
        // Drain any pending commands between packets.
        match drain_commands(cmd_rx, shared) {
            ControlFlow::Continue => {}
            ControlFlow::Break => break 'pkt,
            ControlFlow::Shutdown => {
                shared.set_state(PlayerState::Idle);
                return Ok(());
            }
            ControlFlow::Seek(ms) => {
                apply_seek(&mut format, track_id, ms);
                reset_clock(shared, ms);
                resampler.flush();
                // Drop the decoder's internal state so the first decoded
                // packet after a seek doesn't carry pre-seek residue.
                decoder.reset();
                continue;
            }
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // End of stream — fall through to drain resampler.
                break 'pkt;
            }
            Err(SymphoniaError::ResetRequired) => break 'pkt,
            Err(e) => return Err(format!("next_packet: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::warn!(error = %e, "decode error, skipping packet");
                continue;
            }
            Err(e) => return Err(format!("decode fatal: {e}")),
        };

        if sample_buf.is_none() {
            let spec = *decoded.spec();
            let capacity = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
        }
        let sb = sample_buf.as_mut().unwrap();
        sb.copy_interleaved_ref(decoded);

        // Channel layout conversion: source -> destination channel count.
        interleaved_scratch.clear();
        convert_channels(sb.samples(), src_channels, dst_channels, &mut interleaved_scratch);

        // Resample if source and dest rates differ.
        resampled_scratch.clear();
        if let Err(e) = resampler.process(&interleaved_scratch, &mut resampled_scratch) {
            return Err(format!("resample: {e}"));
        }

        // Push into the ring, blocking (with short sleeps) when full so
        // we never drop samples.
        match push_samples(&resampled_scratch, producer, cmd_rx, shared) {
            PushOutcome::Ok => {}
            PushOutcome::Stop => break 'pkt,
            PushOutcome::Shutdown => {
                shared.set_state(PlayerState::Idle);
                return Ok(());
            }
            PushOutcome::Seek(ms) => {
                apply_seek(&mut format, track_id, ms);
                reset_clock(shared, ms);
                resampler.flush();
                decoder.reset();
                continue;
            }
        }
    }

    // Flush any trailing resampler state so we don't tail-cut the track.
    resampler.flush();
    shared.set_state(PlayerState::Ended);
    Ok(())
}

/// Reset the position counters so the UI clock jumps to `ms`. Must be
/// called after `format.seek()` to keep `SharedPlayback::current_position_ms`
/// in sync. Note: the cpal callback may keep draining ~1 s of stale
/// samples still in the ring buffer before the new audio is audible —
/// acceptable gap for MVP.
fn reset_clock(shared: &SharedPlayback, ms: u64) {
    shared.samples_played.store(0, Ordering::Relaxed);
    shared.base_offset_ms.store(ms, Ordering::Release);
    shared.seek_generation.fetch_add(1, Ordering::Release);
}

/// Apply a seek to the format reader. Errors are logged and ignored —
/// seeking past EOF on a VBR MP3, for instance, is not fatal.
fn apply_seek(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    ms: u64,
) {
    let time = Time::from(Duration::from_millis(ms));
    if let Err(err) = format.seek(
        SeekMode::Accurate,
        SeekTo::Time {
            time,
            track_id: Some(track_id),
        },
    ) {
        tracing::warn!(?err, ms, "format seek failed");
    }
}

enum ControlFlow {
    Continue,
    Break,
    Shutdown,
    Seek(u64),
}

/// Drain pending commands without blocking. Returns:
/// - `Continue` to keep decoding
/// - `Break` to stop the current track but keep the decoder alive
/// - `Shutdown` to exit the decoder loop entirely
/// - `Seek(ms)` to ask the caller to apply a seek on the format reader
///
/// On `Pause`, this function loops on `recv()` (blocking) until a
/// Resume / Stop / Shutdown arrives, so the decoder is cheap while
/// paused. While paused, Seek commands are buffered in a
/// `pending_seek` local and applied immediately after Resume — that
/// way clicking seek while paused doesn't silently drop the request.
fn drain_commands(cmd_rx: &Receiver<AudioCmd>, shared: &SharedPlayback) -> ControlFlow {
    loop {
        match cmd_rx.try_recv() {
            Ok(AudioCmd::Shutdown) => return ControlFlow::Shutdown,
            Ok(AudioCmd::Stop) => return ControlFlow::Break,
            Ok(AudioCmd::Seek(ms)) => return ControlFlow::Seek(ms),
            Ok(AudioCmd::Pause) => {
                shared.set_state(PlayerState::Paused);
                let mut pending_seek: Option<u64> = None;
                // Block for the next command.
                loop {
                    match cmd_rx.recv() {
                        Ok(AudioCmd::Resume) => {
                            shared.set_state(PlayerState::Playing);
                            break;
                        }
                        Ok(AudioCmd::Shutdown) => return ControlFlow::Shutdown,
                        Ok(AudioCmd::Stop) => return ControlFlow::Break,
                        Ok(AudioCmd::Seek(ms)) => pending_seek = Some(ms),
                        Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
                        Ok(_) => {} // ignore load-while-paused for MVP
                        Err(_) => return ControlFlow::Shutdown,
                    }
                }
                if let Some(ms) = pending_seek {
                    return ControlFlow::Seek(ms);
                }
            }
            Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
            // Resume is a no-op when already playing; LoadAndPlay
            // mid-track is not supported in this checkpoint.
            Ok(_) => {}
            Err(TryRecvError::Empty) => return ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => return ControlFlow::Shutdown,
        }
    }
}

/// Push all samples into the producer, sleeping briefly when the ring
/// is full. Commands are polled between retries so pause/stop/seek
/// still respond promptly even when the buffer is saturated.
///
/// rtrb 0.3's `write_chunk_uninit` either gives us the full `n` slots
/// we asked for or returns `TooFewSlots(available)`; in the latter case
/// we submit what fits, sleep, and loop. `CopyToUninit` handles the
/// actual `MaybeUninit<f32>` → `f32` copy without `unsafe` code.
fn push_samples(
    samples: &[f32],
    producer: &mut Producer<f32>,
    cmd_rx: &Receiver<AudioCmd>,
    shared: &SharedPlayback,
) -> PushOutcome {
    let mut written = 0;
    while written < samples.len() {
        let remaining = &samples[written..];
        let requested = remaining.len();
        let result = producer.write_chunk_uninit(requested);
        let (slots, chunk) = match result {
            Ok(chunk) => (requested, chunk),
            Err(ChunkError::TooFewSlots(available)) => {
                if available == 0 {
                    // Ring full. Yield briefly and poll commands so
                    // pause/stop/seek aren't blocked by a saturated
                    // buffer.
                    match drain_commands(cmd_rx, shared) {
                        ControlFlow::Shutdown => return PushOutcome::Shutdown,
                        ControlFlow::Break => return PushOutcome::Stop,
                        ControlFlow::Seek(ms) => return PushOutcome::Seek(ms),
                        ControlFlow::Continue => {}
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match producer.write_chunk_uninit(available) {
                    Ok(chunk) => (available, chunk),
                    Err(_) => unreachable!("available slots vanished under us"),
                }
            }
        };

        let slice = &remaining[..slots];
        // rtrb's WriteChunkUninit exposes two contiguous regions
        // (the ring may wrap between them). We copy into each region
        // separately, then commit the whole chunk.
        let mut chunk = chunk;
        {
            let (first, second) = chunk.as_mut_slices();
            let split = first.len();
            slice[..split].copy_to_uninit(first);
            if split < slice.len() {
                slice[split..].copy_to_uninit(second);
            }
        }
        // SAFETY: we just wrote exactly `slots` f32 values via
        // `copy_to_uninit` into the full chunk. Every `MaybeUninit<f32>`
        // slot is now initialized, satisfying `commit_all`'s contract.
        unsafe { chunk.commit_all() };
        written += slots;
    }
    PushOutcome::Ok
}

/// Convert interleaved `src_chans`-wide samples to interleaved
/// `dst_chans`-wide samples in `out`. Simple fixed rules:
/// - Equal counts: copy verbatim
/// - mono (1) → stereo (2): duplicate
/// - stereo (2) → mono (1): average
/// - ≥3 → 2: Lo/Ro downmix on the first 6 channels (ITU BS.775)
/// - anything else: take the first `min(src, dst)` channels, pad zeros
fn convert_channels(
    input: &[f32],
    src_chans: usize,
    dst_chans: usize,
    out: &mut Vec<f32>,
) {
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
            for f in 0..frames {
                let s = input[f];
                out.push(s);
                out.push(s);
            }
        }
        (2, 1) => {
            out.reserve(frames);
            for f in 0..frames {
                out.push(0.5 * (input[f * 2] + input[f * 2 + 1]));
            }
        }
        // 5.1 → stereo Lo/Ro (ITU-R BS.775): L' = L + 0.707*C + 0.707*Ls
        (s, 2) if s >= 6 => {
            const K: f32 = 0.707;
            out.reserve(frames * 2);
            for f in 0..frames {
                let base = f * s;
                let l = input[base];
                let r = input[base + 1];
                let c = input[base + 2];
                // LFE (base+3) skipped; Ls/Rs at base+4/5
                let ls = input[base + 4];
                let rs = input[base + 5];
                out.push(l + K * c + K * ls);
                out.push(r + K * c + K * rs);
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
