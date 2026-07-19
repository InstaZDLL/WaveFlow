//! Windows-only WASAPI Exclusive Mode output backend.
//!
//! Bit-perfect audiophile output: the application owns the device's
//! mix layer outright. The Windows audio engine doesn't mix our
//! samples with system sounds / other applications, doesn't apply its
//! own resampler, and doesn't insert any DSP between us and the DAC.
//!
//! Trade-offs vs the default cpal shared-mode backend:
//!
//! - Only one application can hold the device exclusive at a time.
//!   If Spotify (or any other player) already grabbed it, our init
//!   fails and the engine falls back to shared mode.
//! - System sounds (notifications, Discord) are silenced for the
//!   duration — by design.
//! - Some USB DACs ship buggy drivers that misbehave in
//!   event-driven exclusive mode; if init fails with the OS-reported
//!   error, we surface it through `player:error` so the user can
//!   toggle the feature off without restarting.
//!
//! Scope: we initialize exclusive at the device's **endpoint format**
//! (`PKEY_AudioEngine_DeviceFormat`, i.e. the "Default Format" the
//! audiophile picked in the Windows Sound control panel), falling
//! back to the shared-mode mix format and then to plain stereo — see
//! [`collect_layout_candidates`]. The decoder's existing rubato resampler
//! still converts every source track to that rate — so this is
//! "bypass the OS mixer" rather than "honor the source rate
//! exactly". Per-track sample-rate switching is a future phase.
//!
//! Same SPSC ring contract as the cpal backend (`Producer<f32>` →
//! `Consumer<f32>`), so the decoder thread doesn't know which
//! backend is driving the device.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;
use wasapi::{
    AudioClient, AudioRenderClient, Device, DeviceEnumerator, Direction, Handle, SampleType,
    StreamMode, WaveFormat,
};

use super::output::{OutputHandle, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

/// Spawn the WASAPI Exclusive output thread. Returns the
/// decoder-side `Producer<f32>` (hand it to the decoder) and an
/// [`OutputHandle`] the engine keeps around for teardown.
///
/// Mirrors `output::spawn_output_thread`'s signature so the engine can
/// pick a backend without leaking the type into every layer.
///
/// Init is synchronous: if the device can't be opened in exclusive
/// mode (busy, unsupported format, no such device), the error is
/// surfaced before this function returns so the caller can fall back
/// to the cpal shared backend.
pub fn spawn_exclusive_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<()>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join: JoinHandle<()> = std::thread::Builder::new()
        .name("waveflow-wasapi-exclusive".into())
        .spawn(move || {
            output_thread_main(
                thread_shared,
                consumer,
                shutdown_rx,
                init_tx,
                thread_app,
                thread_device,
            )
        })
        .map_err(|e| AppError::Audio(format!("spawn wasapi exclusive thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(())) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                wasapi_exclusive: true,
            },
        )),
        Ok(Err(err)) => {
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "wasapi exclusive thread died before reporting init result".into(),
        )),
    }
}

/// Thread body. Owns the COM apartment + the wasapi handles.
fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<()>>,
    app: AppHandle,
    device_name: Option<String>,
) {
    // COM init for this thread. MTA is the right choice for an audio
    // worker that doesn't touch UI. Any HRESULT other than S_OK /
    // S_FALSE / RPC_E_CHANGED_MODE is a hard error.
    let hr = wasapi::initialize_mta();
    if let Err(err) = hr.ok() {
        let _ = init_tx.send(Err(AppError::Audio(format!(
            "CoInitializeEx(MTA) failed: {err:?}"
        ))));
        return;
    }

    let session = match open_exclusive_session(&device_name, &shared) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(?err, "wasapi exclusive init failed");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // Signal a successful init back to the caller so the engine can
    // proceed to spawn the decoder thread.
    let _ = init_tx.send(Ok(()));

    tracing::info!(
        sample_rate = session.sample_rate,
        channels = session.channels,
        buffer_frames = session.buffer_frames,
        "wasapi exclusive stream opened"
    );

    run_event_loop(session, consumer, shutdown_rx, &shared, &app);

    tracing::debug!("wasapi exclusive output thread exiting");
}

/// Bit depth + container layout actually accepted by the device in
/// exclusive mode. Returned from the negotiation in
/// [`open_exclusive_session`] so the hot path knows how to pack
/// `f32` samples into the byte buffer wasapi consumes (#174).
///
/// Order in [`FORMAT_FALLBACK_CHAIN`] is high-quality → low-quality:
/// most audiophile DACs accept Float32 natively; Realtek ALC and
/// many integrated codecs only honor PCM in exclusive mode and
/// reject Float32 outright with `AUDCLNT_E_UNSUPPORTED_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExclusiveSampleFormat {
    /// `wBitsPerSample = 32`, `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
    /// Native cpal-style pipeline — zero conversion at the boundary.
    Float32,
    /// `wBitsPerSample = 24`, container 24, PCM. Three bytes per
    /// sample, no padding. Compact wire format favored by USB
    /// class-1 / class-2 DACs.
    Pcm24Packed,
    /// `wBitsPerSample = 32`, valid bits 24, PCM. Four bytes per
    /// sample, high byte is the LSB of a sign-extended i32. Most
    /// integrated codecs that "support 24-bit" really want this
    /// layout in exclusive mode.
    Pcm24Padded,
    /// `wBitsPerSample = 16`, PCM. Two bytes per sample. Universal
    /// fallback for ancient or driver-limited hardware.
    Pcm16,
}

impl ExclusiveSampleFormat {
    /// Bytes per sample on the wire (per single channel).
    fn bytes_per_sample(self) -> usize {
        match self {
            Self::Float32 | Self::Pcm24Padded => 4,
            Self::Pcm24Packed => 3,
            Self::Pcm16 => 2,
        }
    }

    /// Build the WaveFormat the wasapi crate hands to WASAPI.
    /// `valid_bits` matches the actual precision, `bits_per_sample`
    /// matches the container size — the two differ for Pcm24Padded.
    fn to_wave_format(self, sample_rate: usize, channels: usize) -> WaveFormat {
        let (bits, valid, ty) = match self {
            Self::Float32 => (32, 32, SampleType::Float),
            Self::Pcm24Packed => (24, 24, SampleType::Int),
            Self::Pcm24Padded => (32, 24, SampleType::Int),
            Self::Pcm16 => (16, 16, SampleType::Int),
        };
        WaveFormat::new(bits, valid, &ty, sample_rate, channels, None)
    }

    /// Short label for the diagnostics log line.
    fn label(self) -> &'static str {
        match self {
            Self::Float32 => "F32",
            Self::Pcm24Packed => "S24_3LE",
            Self::Pcm24Padded => "S24_4LE",
            Self::Pcm16 => "S16_LE",
        }
    }
}

/// Order tried by [`open_exclusive_session`]. Float first (zero
/// conversion cost), then PCM 24-bit packed / padded, then PCM 16
/// as the universal last resort. Falling all the way through
/// triggers the cpal shared-mode fallback at the caller.
const FORMAT_FALLBACK_CHAIN: [ExclusiveSampleFormat; 4] = [
    ExclusiveSampleFormat::Float32,
    ExclusiveSampleFormat::Pcm24Packed,
    ExclusiveSampleFormat::Pcm24Padded,
    ExclusiveSampleFormat::Pcm16,
];

/// A (sample rate, channel count) pair to try in exclusive mode,
/// tagged with where it came from so the log says which source
/// actually got the device open (#409).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayoutCandidate {
    sample_rate: usize,
    channels: usize,
    origin: &'static str,
}

/// Build the layout candidates in priority order (#409).
///
/// The order matters, and the first entry is the whole point of this
/// function. `get_mixformat` returns the **shared-mode mix format** —
/// the shape of the pipe into the Windows audio engine, *after* the
/// engine has applied whatever it wants. With "Audio Enhancements",
/// Spatial Sound or a virtual-surround driver in the way, that comes
/// back as 8 channels even though the endpoint is a plain stereo jack.
/// Negotiating exclusive mode against it asks the hardware for a
/// layout it has never supported, so every format in the chain is
/// rejected with `AUDCLNT_E_UNSUPPORTED_FORMAT` (0x88890008) and the
/// user silently drops to shared mode.
///
/// `get_device_format` reads `PKEY_AudioEngine_DeviceFormat` instead —
/// the "Default Format" the user picked in the Windows Sound control
/// panel, which describes the endpoint itself rather than the engine
/// in front of it. That is the layout exclusive mode actually wants.
///
/// The mix format stays as the second candidate (it's correct on the
/// majority of machines, where the two agree anyway), and a plain
/// stereo entry closes the list for drivers that report a
/// multi-channel default no application can open.
fn collect_layout_candidates(device: &Device) -> Vec<LayoutCandidate> {
    // A device with no property store, or a driver that doesn't
    // publish the key, is a soft failure — the mix format still gets
    // its turn, which is exactly the pre-#409 behaviour.
    let endpoint = match device.get_device_format() {
        Ok(fmt) => Some((
            fmt.get_samplespersec() as usize,
            fmt.get_nchannels() as usize,
        )),
        Err(err) => {
            tracing::debug!(
                %err,
                "wasapi exclusive: endpoint device format unavailable, falling back to mix format"
            );
            None
        }
    };

    let mix = match device
        .get_iaudioclient()
        .and_then(|client| client.get_mixformat())
    {
        Ok(fmt) => Some((
            fmt.get_samplespersec() as usize,
            fmt.get_nchannels() as usize,
        )),
        Err(err) => {
            tracing::debug!(%err, "wasapi exclusive: mix format unavailable");
            None
        }
    };

    build_layout_candidates(endpoint, mix)
}

/// Ordering + dedupe half of [`collect_layout_candidates`], split out
/// so it can be unit-tested without a COM apartment or real hardware.
///
/// Both inputs are `(sample_rate, channels)`, and either may be absent
/// when the corresponding query failed.
fn build_layout_candidates(
    endpoint: Option<(usize, usize)>,
    mix: Option<(usize, usize)>,
) -> Vec<LayoutCandidate> {
    // Dedupe on the layout, not the origin: the endpoint format and
    // the mix format agree on most machines, and probing the same
    // pair twice would just double the failure log. A zero in either
    // field means the driver handed back a malformed format — skip it
    // rather than ask WASAPI for a 0-channel stream.
    fn push(
        candidates: &mut Vec<LayoutCandidate>,
        sample_rate: usize,
        channels: usize,
        origin: &'static str,
    ) {
        if channels > 0
            && sample_rate > 0
            && !candidates
                .iter()
                .any(|c| c.sample_rate == sample_rate && c.channels == channels)
        {
            candidates.push(LayoutCandidate {
                sample_rate,
                channels,
                origin,
            });
        }
    }

    let mut candidates: Vec<LayoutCandidate> = Vec::new();

    if let Some((rate, channels)) = endpoint {
        push(&mut candidates, rate, channels, "endpoint");
    }

    if let Some((rate, channels)) = mix {
        push(&mut candidates, rate, channels, "mix");
        // Channel-axis fallback: same rate, plain stereo. Catches the
        // case where both reported layouts are inflated.
        push(&mut candidates, rate, 2, "stereo");
    }

    // Stereo at the endpoint rate too, in case the endpoint and the
    // engine disagree on the rate as well as the channel count.
    if let Some((rate, _)) = endpoint {
        push(&mut candidates, rate, 2, "stereo");
    }

    candidates
}

/// Bundle of everything the event loop needs after a successful init.
/// Kept private so callers can't misuse the wasapi handles outside
/// the thread that owns the COM apartment.
struct ExclusiveSession {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
    sample_rate: u32,
    channels: u16,
    buffer_frames: u32,
    /// Format the device actually accepted. Drives the f32 → bytes
    /// conversion inside `run_event_loop`.
    format: ExclusiveSampleFormat,
}

/// Resolve the device, then walk every (layout × bit depth) candidate
/// (#174, #409). Layouts come from [`collect_layout_candidates`] —
/// endpoint format first, then the mix format, then plain stereo —
/// and each one is probed against the full format chain before moving
/// on. For each candidate we acquire a fresh `IAudioClient`,
/// check the format is supported in exclusive mode, and try to
/// initialize. Many consumer DACs (Realtek ALC, Conexant CX, some
/// USB codecs) reject `IEEE_FLOAT` outright with
/// `AUDCLNT_E_UNSUPPORTED_FORMAT` (0x88890008) but accept PCM at
/// 24-bit or 16-bit — without this chain those users got bumped to
/// cpal shared mode and never reached bit-perfect playback even
/// with the WASAPI Exclusive toggle on.
///
/// A fresh `IAudioClient` per attempt is deliberate: once an
/// `IAudioClient` has been initialized it can't be re-initialized,
/// and an `is_supported` rejection still leaves the COM object in
/// a "you initialized me with the wrong shape" state on some
/// drivers. Acquiring a new one is cheap and reliable.
fn open_exclusive_session(
    device_name: &Option<String>,
    shared: &Arc<SharedPlayback>,
) -> AppResult<ExclusiveSession> {
    let device = pick_device(device_name)?;

    let layouts = collect_layout_candidates(&device);
    if layouts.is_empty() {
        return Err(AppError::Audio(
            "wasapi exclusive: device reported no usable sample rate / channel layout".into(),
        ));
    }

    let mut last_err: Option<AppError> = None;
    // Every rejection, kept for the summary below. `debug` alone was
    // not enough: release logs run at `info`, so a user reporting a
    // failure only ever showed us the last attempt of eight and the
    // other seven had to be guessed at.
    let mut failures: Vec<String> = Vec::new();
    for layout in &layouts {
        for &format in FORMAT_FALLBACK_CHAIN.iter() {
            match try_open_with_format(&device, format, layout.sample_rate, layout.channels) {
                Ok((client, render, event, buffer_frames)) => {
                    tracing::info!(
                        sample_rate = layout.sample_rate as u32,
                        channels = layout.channels as u16,
                        format = format.label(),
                        layout = layout.origin,
                        "wasapi exclusive negotiation succeeded"
                    );
                    shared
                        .sample_rate
                        .store(layout.sample_rate as u32, Ordering::Release);
                    shared.channels.store(layout.channels as u16, Ordering::Release);
                    return Ok(ExclusiveSession {
                        client,
                        render,
                        event,
                        sample_rate: layout.sample_rate as u32,
                        channels: layout.channels as u16,
                        buffer_frames,
                        format,
                    });
                }
                Err(err) => {
                    // Log the full triple, not just the bit depth: a
                    // rejection blamed on the format is very often the
                    // channel count instead, and the pre-#409 log made
                    // that impossible to tell apart from a diagnostics
                    // dump.
                    tracing::debug!(
                        sample_rate = layout.sample_rate as u32,
                        channels = layout.channels as u16,
                        format = format.label(),
                        layout = layout.origin,
                        %err,
                        "wasapi exclusive candidate rejected; trying next"
                    );
                    failures.push(format!(
                        "{}@{}Hz/{}ch[{}]: {err}",
                        format.label(),
                        layout.sample_rate,
                        layout.channels,
                        layout.origin
                    ));
                    last_err = Some(err);
                }
            }
        }
    }

    tracing::warn!(
        candidates = ?layouts,
        attempts = failures.len(),
        failures = %failures.join(" | "),
        "wasapi exclusive: no (rate, channels, format) combination was accepted"
    );

    Err(last_err.unwrap_or_else(|| {
        AppError::Audio("wasapi exclusive: every format in the fallback chain was rejected".into())
    }))
}

/// Do two `WaveFormat`s describe the same stream? Compares every
/// field that matters to the driver, including the container tag
/// (`cbSize` distinguishes `WAVEFORMATEX` from `WAVEFORMATEXTENSIBLE`)
/// and the channel mask.
///
/// Used only to skip a redundant second init attempt, never to decide
/// whether a format is acceptable.
fn same_wave_format(a: &WaveFormat, b: &WaveFormat) -> bool {
    a.wave_fmt.Format.cbSize == b.wave_fmt.Format.cbSize
        && a.get_nchannels() == b.get_nchannels()
        && a.get_samplespersec() == b.get_samplespersec()
        && a.get_bitspersample() == b.get_bitspersample()
        && a.get_validbitspersample() == b.get_validbitspersample()
        && a.get_dwchannelmask() == b.get_dwchannelmask()
}

/// Single attempt with one bit-depth format. Probes the format, then
/// initializes + starts the stream on a fresh `IAudioClient`.
///
/// The probe goes through `is_supported_exclusive_with_quirks` rather
/// than a bare `is_supported` (#405). `WaveFormat::new` always builds a
/// `WAVEFORMATEXTENSIBLE`, and a good number of drivers reject that
/// representation outright with `AUDCLNT_E_UNSUPPORTED_FORMAT` while
/// happily accepting the *same* PCM layout described as a plain
/// `WAVEFORMATEX` — which is why a device can turn down 16-bit stereo
/// at its own reported rate, the most universally supported format
/// there is. The helper also re-probes against each recommended
/// `ksmedia.h` channel mask, since a multi-channel endpoint typically
/// accepts exactly one and `WaveFormat::new(.., None)` guesses.
fn try_open_with_format(
    device: &Device,
    format: ExclusiveSampleFormat,
    sample_rate: usize,
    channels: usize,
) -> AppResult<(AudioClient, AudioRenderClient, Handle, u32)> {
    let requested = format.to_wave_format(sample_rate, channels);

    let probe_client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient ({}): {e:?}", format.label())))?;
    let accepted = probe_client
        .is_supported_exclusive_with_quirks(&requested)
        .map_err(|e| {
            AppError::Audio(format!(
                "is_supported rejected {} in exclusive mode: {e:?}",
                format.label()
            ))
        })?;
    drop(probe_client);

    // wasapi's own docs warn that some drivers validate the simplified
    // representation but want the original one at init time. So try
    // the shape the probe accepted first, then fall back to the shape
    // we asked for — but only when they actually differ, otherwise the
    // retry is a guaranteed repeat of the same failure.
    match init_stream(device, format, channels, &accepted) {
        Ok(session) => Ok(session),
        Err(err) if !same_wave_format(&accepted, &requested) => {
            tracing::debug!(
                format = format.label(),
                %err,
                "wasapi exclusive: init with the probe-accepted shape failed, retrying as requested"
            );
            init_stream(device, format, channels, &requested)
        }
        Err(err) => Err(err),
    }
}

/// Initialize + start an exclusive stream with one concrete
/// `WaveFormat`. Pre-fills with silence sized to that format so the
/// first device event lands on clean state.
///
/// Always takes a fresh `IAudioClient`: once initialized (or once an
/// init has been refused) a client can't be reused, and some drivers
/// leave a rejected one in an unusable state.
fn init_stream(
    device: &Device,
    format: ExclusiveSampleFormat,
    channels: usize,
    wave: &WaveFormat,
) -> AppResult<(AudioClient, AudioRenderClient, Handle, u32)> {
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| AppError::Audio(format!("get IAudioClient ({}): {e:?}", format.label())))?;

    let (default_period, min_period) = client
        .get_device_period()
        .map_err(|e| AppError::Audio(format!("get_device_period: {e:?}")))?;
    let period_hns = min_period.max(default_period);
    let mode = StreamMode::EventsExclusive { period_hns };

    client
        .initialize_client(wave, &Direction::Render, &mode)
        .map_err(|e| {
            AppError::Audio(format!(
                "initialize_client {} exclusive: {e:?}",
                format.label()
            ))
        })?;

    let event = client
        .set_get_eventhandle()
        .map_err(|e| AppError::Audio(format!("set_get_eventhandle: {e:?}")))?;

    let buffer_frames = client
        .get_buffer_size()
        .map_err(|e| AppError::Audio(format!("get_buffer_size: {e:?}")))?;

    let render = client
        .get_audiorenderclient()
        .map_err(|e| AppError::Audio(format!("get_audiorenderclient: {e:?}")))?;

    let silent_bytes = vec![0u8; (buffer_frames as usize) * channels * format.bytes_per_sample()];
    render
        .write_to_device(buffer_frames as usize, &silent_bytes, None)
        .map_err(|e| AppError::Audio(format!("prefill render buffer: {e:?}")))?;

    client
        .start_stream()
        .map_err(|e| AppError::Audio(format!("start_stream: {e:?}")))?;

    Ok((client, render, event, buffer_frames))
}

fn pick_device(device_name: &Option<String>) -> AppResult<Device> {
    // wasapi 0.23 removed the free `get_default_device` / `DeviceCollection`
    // entry points; everything now goes through a `DeviceEnumerator`
    // (which owns the underlying `IMMDeviceEnumerator` COM pointer).
    let enumerator = DeviceEnumerator::new()
        .map_err(|e| AppError::Audio(format!("DeviceEnumerator::new: {e:?}")))?;
    if let Some(name) = device_name.as_deref().filter(|n| !n.is_empty()) {
        // Friendly-name lookup lives on the collection, not the
        // enumerator (the enumerator's `get_device` wants an opaque
        // device-id, not the human-readable name we persist).
        let coll = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(|e| AppError::Audio(format!("enumerate render devices: {e:?}")))?;
        if let Ok(dev) = coll.get_device_with_name(name) {
            return Ok(dev);
        }
        tracing::warn!(
            requested = name,
            "wasapi exclusive: requested device not found, falling back to default"
        );
    }
    enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| AppError::Audio(format!("default render device: {e:?}")))
}

/// Drain the SPSC ring into the wasapi render client on each buffer
/// event. The thread blocks on the OS event handle so it costs zero
/// CPU while idle.
///
/// Mirrors the cpal callback's logic for `paused_output`,
/// `drain_silent`, `volume`, `normalize_enabled`, and `mono_enabled`
/// so the user-facing behaviour is identical regardless of backend.
fn run_event_loop(
    session: ExclusiveSession,
    mut consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    shared: &Arc<SharedPlayback>,
    _app: &AppHandle,
) {
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
    // gain/mono/normalize to, one for the little-endian byte image
    // we hand to WASAPI. Pre-allocated so the hot path never touches
    // the allocator.
    let mut samples: Vec<f32> = vec![0.0; need_samples];
    let mut bytes_scratch: Vec<u8> = vec![0u8; buffer_bytes];
    let silent_buf = vec![0u8; buffer_bytes];

    // 2 s timeout is generous — for a 10 ms device period the event
    // should fire every 10 ms. A timeout means the device went away
    // (USB unplug, hibernate). We then exit so the engine can decide
    // whether to re-init or fall back.
    const EVENT_TIMEOUT_MS: u32 = 2000;

    loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(?err, "wasapi exclusive wait_for_event failed");
                break;
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
            let volume = shared.volume();
            let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
            let mono = shared.mono_enabled.load(Ordering::Relaxed);
            let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };

            // Samples actually pulled from the ring this period. Drives
            // `SharedPlayback::samples_played`, which is the only source
            // the progress bar, lyrics sync and play-event crediting have
            // for "where are we in the track" — see the counting note in
            // `state.rs`. Silence written on an underrun is deliberately
            // NOT counted, matching the cpal callback.
            let mut written: u64 = 0;

            if mono && channels >= 2 {
                // Mono downmix: average all channels per frame.
                let mut i = 0;
                while i + channels <= need_samples {
                    let mut sum = 0.0_f32;
                    let mut got = 0usize;
                    for slot in &mut samples[i..i + channels] {
                        match consumer.pop() {
                            Ok(s) => {
                                sum += s;
                                got += 1;
                                *slot = 0.0; // placeholder, overwritten below
                            }
                            Err(_) => *slot = 0.0,
                        }
                    }
                    let v = if got > 0 {
                        written += got as u64;
                        (sum / channels as f32) * volume * norm_gain
                    } else {
                        0.0
                    };
                    for slot in &mut samples[i..i + channels] {
                        *slot = v;
                    }
                    i += channels;
                }
            } else {
                // Normal multi-channel path.
                for slot in samples.iter_mut() {
                    *slot = match consumer.pop() {
                        Ok(s) => {
                            written += 1;
                            s * volume * norm_gain
                        }
                        Err(_) => 0.0,
                    };
                }
            }

            // Pack `samples` into the byte layout the negotiated
            // exclusive format expects (#174). Hot path: no
            // allocations, no branches inside the inner loop
            // beyond the format dispatch above.
            pack_samples(format, &samples, &mut bytes_scratch);
            if written > 0 {
                shared
                    .samples_played
                    .fetch_add(written, std::sync::atomic::Ordering::Relaxed);
            }
            &bytes_scratch
        };

        if let Err(err) = render.write_to_device(need_frames, bytes, None) {
            tracing::warn!(?err, "wasapi write_to_device failed");
            break;
        }

        // Quick non-blocking shutdown check after every buffer so a
        // user-initiated stop takes effect within one device period
        // (~10 ms) rather than waiting up to EVENT_TIMEOUT_MS.
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
    }

    // Stop the stream so the next exclusive opener doesn't fight us
    // for the device. Errors are non-fatal — we're tearing down anyway.
    let _ = client.stop_stream();
    let _ = event; // released with the function frame
                   // Wait briefly so a slow `stop_stream` settles before COM
                   // uninit; not strictly required, but tidier under tracing.
    std::thread::sleep(Duration::from_millis(5));
    wasapi::deinitialize();
}

/// Pack the decoded `f32` sample buffer into the little-endian byte
/// layout the negotiated exclusive format expects.
///
/// Saturation is done by clamping to `[-1.0, 1.0]` before scaling
/// rather than checking the post-cast i32/i16 — this side-steps the
/// undefined behaviour C-style casts have on `f32` overflow and
/// avoids a branch per sample. The decoder upstream applies
/// `volume * norm_gain` BEFORE this function, so the input is
/// already at the final analogue amplitude; a clipped sample here
/// means the upstream chain (EQ, ReplayGain, mono mix) pushed
/// past 0 dBFS and the user genuinely wants saturation.
///
/// All four packing paths are bounded by `samples.len()` and
/// `bytes.len()` so a mismatched slot count (`samples` short by
/// one) leaves trailing bytes at their previous values (which the
/// caller pre-cleared to zero). No panics in the hot path.
fn pack_samples(format: ExclusiveSampleFormat, samples: &[f32], bytes: &mut [u8]) {
    match format {
        ExclusiveSampleFormat::Float32 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                chunk.copy_from_slice(&sample.to_le_bytes());
            }
        }
        ExclusiveSampleFormat::Pcm24Packed => {
            // 24-bit signed integer, 3 bytes per sample, little-endian.
            // i32::MAX for 24-bit is 2^23 - 1 = 8_388_607.
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(3)) {
                let clamped = sample.clamp(-1.0, 1.0);
                let v = (clamped * 8_388_607.0) as i32;
                chunk[0] = (v & 0xFF) as u8;
                chunk[1] = ((v >> 8) & 0xFF) as u8;
                chunk[2] = ((v >> 16) & 0xFF) as u8;
            }
        }
        ExclusiveSampleFormat::Pcm24Padded => {
            // 24-bit valid bits inside a 32-bit container, LE. WASAPI
            // expects the 24-bit value in the LOW bytes with sign
            // extension into the high byte (so a positive i32 with
            // bits 24-30 == 0 is the right shape — left-shift would
            // be wrong, the container stores the raw signed value).
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(4)) {
                let clamped = sample.clamp(-1.0, 1.0);
                let v = (clamped * 8_388_607.0) as i32;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
        ExclusiveSampleFormat::Pcm16 => {
            for (sample, chunk) in samples.iter().zip(bytes.chunks_exact_mut(2)) {
                let clamped = sample.clamp(-1.0, 1.0);
                let v = (clamped * 32_767.0) as i16;
                chunk.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layouts(candidates: &[LayoutCandidate]) -> Vec<(usize, usize, &'static str)> {
        candidates
            .iter()
            .map(|c| (c.sample_rate, c.channels, c.origin))
            .collect()
    }

    /// The #409 report: Windows shows the endpoint as stereo 44.1 kHz,
    /// but the mix format comes back as 8 channels because the audio
    /// engine has a virtual-surround effect in front of it. Probing
    /// the mix format first is what made every format fail.
    #[test]
    fn endpoint_layout_is_tried_before_an_inflated_mix_format() {
        let candidates = build_layout_candidates(Some((44_100, 2)), Some((48_000, 8)));
        assert_eq!(
            layouts(&candidates),
            vec![
                (44_100, 2, "endpoint"),
                (48_000, 8, "mix"),
                (48_000, 2, "stereo"),
            ],
            "the endpoint's own format must be candidate #1"
        );
    }

    /// The common case: nothing is intercepting the stream, both
    /// queries agree. One probe, not two identical ones.
    #[test]
    fn agreeing_endpoint_and_mix_collapse_to_one_candidate() {
        let candidates = build_layout_candidates(Some((48_000, 2)), Some((48_000, 2)));
        assert_eq!(layouts(&candidates), vec![(48_000, 2, "endpoint")]);
    }

    /// Both sources inflated: the stereo entry is the only way out.
    #[test]
    fn stereo_closes_the_list_when_every_reported_layout_is_multichannel() {
        let candidates = build_layout_candidates(Some((48_000, 6)), Some((48_000, 8)));
        assert_eq!(
            layouts(&candidates),
            vec![
                (48_000, 6, "endpoint"),
                (48_000, 8, "mix"),
                (48_000, 2, "stereo"),
            ]
        );
    }

    /// Endpoint and mix disagree on the rate as well — stereo is
    /// offered at both rates, endpoint's first.
    #[test]
    fn stereo_fallback_covers_both_reported_rates() {
        let candidates = build_layout_candidates(Some((96_000, 8)), Some((48_000, 8)));
        assert_eq!(
            layouts(&candidates),
            vec![
                (96_000, 8, "endpoint"),
                (48_000, 8, "mix"),
                (48_000, 2, "stereo"),
                (96_000, 2, "stereo"),
            ]
        );
    }

    /// A driver that doesn't publish `PKEY_AudioEngine_DeviceFormat`
    /// must still get the pre-#409 behaviour rather than no candidate.
    #[test]
    fn missing_endpoint_format_falls_back_to_the_mix_format() {
        let candidates = build_layout_candidates(None, Some((44_100, 2)));
        assert_eq!(layouts(&candidates), vec![(44_100, 2, "mix")]);
    }

    #[test]
    fn no_usable_format_yields_no_candidates() {
        assert!(build_layout_candidates(None, None).is_empty());
    }

    /// A malformed format must not reach WASAPI as a 0-channel or
    /// 0 Hz stream request. A usable rate carried alongside a bogus
    /// channel count still earns a stereo probe — that is the whole
    /// point of the channel-axis fallback.
    #[test]
    fn degenerate_formats_are_discarded() {
        let candidates = build_layout_candidates(Some((0, 2)), Some((48_000, 0)));
        assert_eq!(layouts(&candidates), vec![(48_000, 2, "stereo")]);
    }

    #[test]
    fn pack_samples_float32_round_trips() {
        let samples = [0.0_f32, 1.0, -1.0, 0.5];
        let mut bytes = vec![0u8; samples.len() * 4];
        pack_samples(ExclusiveSampleFormat::Float32, &samples, &mut bytes);
        for (i, sample) in samples.iter().enumerate() {
            let round = f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
            assert_eq!(round, *sample);
        }
    }

    #[test]
    fn pack_samples_s16_saturates_and_round_trips() {
        let samples = [0.0_f32, 1.0, -1.0, 0.5, 2.0, -2.0];
        let mut bytes = vec![0u8; samples.len() * 2];
        pack_samples(ExclusiveSampleFormat::Pcm16, &samples, &mut bytes);
        let extract = |i: usize| i16::from_le_bytes(bytes[i * 2..i * 2 + 2].try_into().unwrap());
        assert_eq!(extract(0), 0);
        assert_eq!(extract(1), 32_767);
        assert_eq!(extract(2), -32_767);
        assert_eq!(extract(3), 16_383); // ~0.5 * 32_767
        assert_eq!(extract(4), 32_767); // saturated above 1.0
        assert_eq!(extract(5), -32_767); // saturated below -1.0
    }

    #[test]
    fn pack_samples_s24_packed_three_bytes_per_sample() {
        let samples = [0.0_f32, 1.0, -1.0];
        let mut bytes = vec![0u8; samples.len() * 3];
        pack_samples(ExclusiveSampleFormat::Pcm24Packed, &samples, &mut bytes);
        // 0.0 → 0
        assert_eq!(&bytes[0..3], &[0, 0, 0]);
        // 1.0 → 8_388_607 = 0x7F_FF_FF → LE [0xFF, 0xFF, 0x7F]
        assert_eq!(&bytes[3..6], &[0xFF, 0xFF, 0x7F]);
        // -1.0 → -8_388_607 = signed i32 0xFF_80_00_01 → low 3 LE bytes
        let expected = (-8_388_607_i32).to_le_bytes();
        assert_eq!(&bytes[6..9], &expected[0..3]);
    }

    #[test]
    fn pack_samples_s24_padded_four_bytes_per_sample() {
        let samples = [1.0_f32];
        let mut bytes = vec![0u8; 4];
        pack_samples(ExclusiveSampleFormat::Pcm24Padded, &samples, &mut bytes);
        // 1.0 → 8_388_607 as i32, LE. High byte stays 0 (positive value).
        assert_eq!(bytes, 8_388_607_i32.to_le_bytes().to_vec());
    }
}
