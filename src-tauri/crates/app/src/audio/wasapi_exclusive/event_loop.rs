//! The render loops, PCM and DoP: each drains the ring into the device
//! buffer on every buffer event.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;
use rtrb::Consumer;

use crate::audio::pcm_scale;
use crate::audio::state::SharedPlayback;

use super::{ExclusiveSampleFormat, ExclusiveSession, ExitReason};

/// Drain the SPSC ring into the wasapi render client on each buffer
/// event. The thread blocks on the OS event handle so it costs zero
/// CPU while idle.
///
/// Mirrors the cpal callback's logic for `paused_output`,
/// `drain_silent`, `volume` and `normalize_enabled` so the user-facing
/// behaviour is identical regardless of backend.
///
/// Returns why the loop stopped so the caller can decide whether a
/// recovery is warranted — see [`ExitReason`].
pub(super) fn run_event_loop(
    session: ExclusiveSession,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    shared: &Arc<SharedPlayback>,
) -> ExitReason {
    // DoP streams run an entirely separate loop: bit-exact, no DSP, and
    // marker-carrying silence on pause. Dispatch up-front so the PCM hot
    // path below stays exactly as it was (#495).
    if session.dop {
        return run_dop_event_loop(session, consumer, shutdown_rx, shared);
    }

    let mut consumer = consumer;
    let ExclusiveSession {
        client,
        render,
        event,
        channels,
        buffer_frames,
        format,
        ..
    } = session;

    let channels = channels as usize;
    let need_frames = buffer_frames as usize;
    let need_samples = need_frames * channels;
    let sample_bytes = format.bytes_per_sample();
    let frame_bytes = channels * sample_bytes;
    let buffer_bytes = need_frames * frame_bytes;

    // Two reusable scratch buffers: one for the f32 samples we apply
    // gain/normalize to, one for the little-endian byte image
    // we hand to WASAPI. Pre-allocated so the hot path never touches
    // the allocator.
    let mut samples: Vec<f32> = vec![0.0; need_samples];
    let mut bytes_scratch: Vec<u8> = vec![0u8; buffer_bytes];
    let silent_buf = vec![0u8; buffer_bytes];
    // Noise source for 16-bit output, held here so the period loop
    // neither allocates nor reseeds.
    let mut dither = pcm_scale::Dither16::new();

    // 2 s timeout is generous — for a 10 ms device period the event
    // should fire every 10 ms. A timeout means the device went away
    // (USB unplug, hibernate). We then exit so the engine can decide
    // whether to re-init or fall back.
    const EVENT_TIMEOUT_MS: u32 = 2000;

    let exit = loop {
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            Err(err) => {
                // A teardown racing the failure: the engine sent the
                // shutdown while we were parked in `wait_for_event`, and
                // releasing the device is what made the wait fail. That's
                // a clean stop, not a device loss — reporting it as one
                // would schedule a rebuild against a stream the engine is
                // deliberately replacing.
                if shutdown_rx.try_recv().is_ok() {
                    break ExitReason::Shutdown;
                }
                tracing::warn!(?err, "wasapi exclusive wait_for_event failed");
                break ExitReason::DeviceLost(format!("wasapi wait_for_event failed: {err:?}"));
            }
        }

        // Hard pause: write silence so the device underrun doesn't
        // click and the user-perceived pause is instant (instead of
        // waiting for the ~1 s pre-buffer to drain).
        let bytes: &[u8] = if shared.paused_output.load(Ordering::Acquire) {
            &silent_buf
        } else if shared.drain_silent.load(Ordering::Acquire) {
            // Drain-silent mode: drop whatever's queued and emit
            // silence. Pop the entire ring so the decoder's
            // spin-wait on a fresh `producer.slots() == RING_CAPACITY`
            // completes within one event period.
            while consumer.pop().is_ok() {}
            &silent_buf
        } else {
            // Same fill every backend uses: volume, the normalize
            // attenuation, and an underrun left uncredited. It lives in `output` so the three
            // exclusive paths and the cpal callback can't drift apart.
            let written = crate::audio::output::fill_pcm_period(
                shared,
                &mut consumer,
                &mut samples,
                channels,
            );

            // Pack `samples` into the byte layout the negotiated
            // exclusive format expects (#174). Hot path: no
            // allocations, no branches inside the inner loop
            // beyond the format dispatch above.
            pack_samples(format, &samples, &mut bytes_scratch, &mut dither);
            if written > 0 {
                shared
                    .samples_played
                    .fetch_add(written, std::sync::atomic::Ordering::Relaxed);
            }
            &bytes_scratch
        };

        if let Err(err) = render.write_to_device(need_frames, bytes, None) {
            // Same teardown race as the `wait_for_event` arm above.
            if shutdown_rx.try_recv().is_ok() {
                break ExitReason::Shutdown;
            }
            tracing::warn!(?err, "wasapi write_to_device failed");
            break ExitReason::DeviceLost(format!("wasapi write_to_device failed: {err:?}"));
        }

        // Quick non-blocking shutdown check after every buffer so a
        // user-initiated stop takes effect within one device period
        // (~10 ms) rather than waiting up to EVENT_TIMEOUT_MS.
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }
    };

    // Stop the stream so the next exclusive opener doesn't fight us
    // for the device. Errors are non-fatal — we're tearing down anyway.
    let _ = client.stop_stream();
    let _ = event; // released with the function frame
                   // Wait briefly so a slow `stop_stream` settles before the
                   // interfaces drop; not strictly required, but tidier under
                   // tracing. COM itself is left to `ComApartment`, up in the
                   // thread body, so it outlives everything it opened.
    std::thread::sleep(Duration::from_millis(5));

    exit
}

/// DoP (DSD over PCM) render loop, #495. Structurally the twin of
/// [`run_event_loop`] — same event wait, shutdown checks, device-loss
/// exit and teardown — but the sample stage is fundamentally different:
///
/// - **Bit-exact, no DSP.** The decoder already produced fully-formed
///   24-bit DoP words (marker + two DSD bytes) and shipped them through
///   the ring as `f32` bit patterns. We pull `to_bits()`, mask to 24
///   bits and write the little-endian bytes straight out. No volume, no
///   mono, no normalize, no clamp — touching a DoP word would corrupt
///   the embedded DSD and the marker cadence the DAC locks onto.
/// - **Marker-carrying silence.** On pause / drain / underrun we can't
///   write PCM zero: the DAC would lose DoP lock and click. Instead we
///   emit DoP *idle* frames — the standard 0x69 DSD-silence payload with
///   a live, per-frame-alternating marker — so the DAC stays in DSD mode
///   outputting analog silence.
fn run_dop_event_loop(
    session: ExclusiveSession,
    mut consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    shared: &Arc<SharedPlayback>,
) -> ExitReason {
    let ExclusiveSession {
        client,
        render,
        event,
        channels,
        buffer_frames,
        format,
        ..
    } = session;

    let channels = channels as usize;
    let padded = matches!(format, ExclusiveSampleFormat::Pcm24Padded);
    let need_frames = buffer_frames as usize;
    let buffer_bytes = need_frames * channels * crate::audio::dop_pack::sample_bytes(padded);

    // One byte scratch reused every period; no allocations in the loop.
    let mut bytes_scratch: Vec<u8> = vec![0u8; buffer_bytes];
    // Running DoP marker phase. Advances once per emitted frame — music
    // and idle alike — so the 0x05 / 0xFA cadence the DAC locks onto
    // never repeats or skips.
    let mut marker_phase: u64 = 0;

    const EVENT_TIMEOUT_MS: u32 = 2000;

    let exit = loop {
        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            Err(err) => {
                if shutdown_rx.try_recv().is_ok() {
                    break ExitReason::Shutdown;
                }
                tracing::warn!(?err, "wasapi exclusive (DoP) wait_for_event failed");
                break ExitReason::DeviceLost(format!("wasapi wait_for_event failed: {err:?}"));
            }
        }

        let paused = shared.paused_output.load(Ordering::Acquire);
        let draining = shared.drain_silent.load(Ordering::Acquire);

        if paused || draining {
            if draining {
                while consumer.pop().is_ok() {}
            }
            crate::audio::dop_pack::render_dop_silence(
                padded,
                channels,
                need_frames,
                &mut marker_phase,
                &mut bytes_scratch,
            );
        } else {
            let written = crate::audio::dop_pack::fill_dop_period(
                padded,
                channels,
                need_frames,
                &mut consumer,
                &mut marker_phase,
                &mut bytes_scratch,
            );
            if written > 0 {
                shared.samples_played.fetch_add(written, Ordering::Relaxed);
            }
        }

        if let Err(err) = render.write_to_device(need_frames, &bytes_scratch, None) {
            if shutdown_rx.try_recv().is_ok() {
                break ExitReason::Shutdown;
            }
            tracing::warn!(?err, "wasapi (DoP) write_to_device failed");
            break ExitReason::DeviceLost(format!("wasapi write_to_device failed: {err:?}"));
        }

        if shutdown_rx.try_recv().is_ok() {
            break ExitReason::Shutdown;
        }
    };

    let _ = client.stop_stream();
    let _ = event;
    std::thread::sleep(Duration::from_millis(5));

    exit
}

/// Pack the decoded `f32` sample buffer into the little-endian byte
/// layout the negotiated exclusive format expects.
///
/// The integer formats go through [`crate::audio::pcm_scale`], which inverts the
/// decoder's own float conversion exactly, so an untouched stream reaches
/// the device bit for bit. 16-bit output is dithered where the chain
/// moved a sample off a 16-bit step, and only there
/// ([`pcm_scale::Dither16`]). The decoder upstream applies
/// `volume * norm_gain` BEFORE this function, so the input is
/// already at the final analogue amplitude; a clipped sample here
/// means the upstream chain (EQ, ReplayGain, mono mix) pushed
/// past 0 dBFS and the user genuinely wants saturation.
///
/// All four packing paths are bounded by `samples.len()` and
/// `bytes.len()` so a mismatched slot count (`samples` short by
/// one) leaves trailing bytes at their previous values (which the
/// caller pre-cleared to zero). No panics in the hot path.
fn pack_samples(
    format: ExclusiveSampleFormat,
    samples: &[f32],
    bytes: &mut [u8],
    dither: &mut pcm_scale::Dither16,
) {
    match format {
        ExclusiveSampleFormat::Float32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                chunk.copy_from_slice(&sample.to_le_bytes());
            }
        }
        ExclusiveSampleFormat::Pcm24Packed => {
            // 24-bit signed integer, 3 bytes per sample, little-endian.
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(3)) {
                let v = pcm_scale::to_i24(*sample);
                chunk[0] = (v & 0xFF) as u8;
                chunk[1] = ((v >> 8) & 0xFF) as u8;
                chunk[2] = ((v >> 16) & 0xFF) as u8;
            }
        }
        ExclusiveSampleFormat::Pcm24Padded => {
            // 24 valid bits inside a 32-bit container, LE. WAVEFORMATEXTENSIBLE
            // pins the layout: "if wValidBitsPerSample is less than
            // wBitsPerSample, the valid bits are left-aligned within the
            // container. The unused bits in the least-significant portion of
            // the container should be set to zero." So the 24-bit value is
            // shifted up by 8 — right-justifying it would hand the device a
            // signal 48 dB down, since it reads precision from the top bits.
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                let v = pcm_scale::to_i24(*sample) << 8;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        ExclusiveSampleFormat::Pcm16 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(2)) {
                let v = dither.quantize(*sample);
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests;
