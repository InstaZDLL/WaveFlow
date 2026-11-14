//! cpal output stream, hosted on a dedicated thread.
//!
//! On Windows `cpal::Stream` is `!Send` (WASAPI / COM handles don't
//! cross thread boundaries). To keep the Stream alive for the engine's
//! lifetime without forcing it into Tauri's managed state (which demands
//! `Send + Sync`), we spawn an "output thread" that:
//!
//! 1. creates the cpal device + stream locally (so the `!Send` value
//!    never leaves its origin thread),
//! 2. calls `stream.play()`,
//! 3. parks on a shutdown channel until the engine tears down — or
//!    until the stream reports itself stuck, in which case the thread
//!    drops it and asks for a rebuild on a backoff.
//!
//! The decoder-side `Producer<f32>` is `Send` and is handed back to the
//! caller along with the shutdown sender and the thread's join handle.
//!
//! The audio callback itself MUST NOT take locks, allocate, or block —
//! it only reads from `rtrb::Consumer` and mutates atomics in
//! [`SharedPlayback`].

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;
#[cfg(target_os = "linux")]
use waveflow_core::playback::channels::ChannelLayout;

use crate::error::{AppError, AppResult};

use super::state::SharedPlayback;

mod devices;
#[cfg(target_os = "linux")]
pub(super) mod linux_layout;
mod recovery;

use devices::device_key;
pub use devices::list_output_devices;
pub(super) use recovery::{
    notify_device_lost, os_default_output_name, schedule_device_rebuild, RebuildTarget,
};
// Called only by the Windows and macOS default-device listeners.
#[allow(unused_imports)]
pub(super) use recovery::schedule_default_device_follow;

/// Run the closure while ALSA library error messages are redirected
/// to /dev/null. On Linux, cpal's enumeration probes every PCM card
/// and ALSA helpfully prints `pcm_dmix` / `pcm_route` warnings for
/// cards that aren't currently usable (HDMI sinks with no monitor
/// attached, Bluetooth profiles in the wrong state, …). The warnings
/// are noise — failure to open during a probe is expected — so we
/// hide them while we're enumerating.
///
/// On non-Linux platforms this is a passthrough.
#[cfg(target_os = "linux")]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    use std::os::unix::io::AsRawFd;

    // Open /dev/null + dup the current stderr (fd 2) so we can put it
    // back. If anything fails, just run `f` with stderr untouched —
    // we'd rather show the spam than skip enumeration.
    let dev_null = match std::fs::OpenOptions::new().write(true).open("/dev/null") {
        Ok(f) => f,
        Err(_) => return f(),
    };
    let saved = unsafe { libc::dup(2) };
    if saved < 0 {
        return f();
    }
    if unsafe { libc::dup2(dev_null.as_raw_fd(), 2) } < 0 {
        unsafe { libc::close(saved) };
        return f();
    }
    let result = f();
    // Restore stderr — best-effort. If `dup2` fails here we can't do
    // much, but the OS will reclaim the fd at process exit.
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn silence_alsa_stderr<R, F: FnOnce() -> R>(f: F) -> R {
    f()
}

/// How many frames the decoder keeps queued ahead of the output: ~1 second
/// at 48 kHz, whatever the channel count. Plenty of headroom for the decoder
/// thread while keeping the delay of a moved setting low.
pub const RING_FRAMES: usize = 48_000;

/// Capacity of the SPSC sample ring, in f32 samples: [`RING_FRAMES`] of a
/// 7.1 output. Fixed, because each backend builds the ring before it knows
/// what the device will accept, and it costs 1.5 MB.
pub const RING_CAPACITY: usize = RING_FRAMES * 8;

/// Slots the decoder leaves free in the ring on an output of `channels`, so
/// the queue holds [`RING_FRAMES`] on any of them. A plain 96 000-sample ring
/// was a second of stereo but a quarter of a second of 7.1, and half that at
/// 96 kHz, and that is all a busy machine had to catch up in.
pub fn ring_reserve(channels: usize) -> usize {
    RING_CAPACITY.saturating_sub(RING_FRAMES.saturating_mul(channels.max(1)))
}

/// Exact (rate, channels) an output must open at to carry a DoP (DSD
/// over PCM) stream, #495. Unlike the normal path — where the device
/// picks the rate and the decoder resamples to it — a DoP stream must
/// reach the DAC at precisely `dsd_rate / 16` in 24-bit or the marker
/// cadence breaks. So the caller (the engine, when it loads a DSD track
/// with DoP enabled) hands the exclusive backend this forced format
/// instead of letting it negotiate. Ignored entirely on the cpal shared
/// path — DoP only runs over an exclusive backend: WASAPI Exclusive on
/// Windows, a raw ALSA `hw:` device on Linux, CoreAudio hog mode on
/// macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DopFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A format the caller demands of the output for one track.
///
/// Two demands ride this, and they differ in what a refusal means:
///
/// - **DoP** (#495) pins the rate *and* the channel layout, and must not
///   fall back to another rate — a marker cadence that gets resampled is
///   white noise — so a refusal fails the open outright and the caller
///   plays the track as DSD → PCM instead.
/// - **The track's own rate** (#600) pins the rate only, and is a
///   preference: a device that will not open at it falls back to the
///   rates it does offer, and the decoder's resampler meets it exactly as
///   it always did. The channel layout stays the device's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestedFormat {
    pub sample_rate: u32,
    /// Part of the format for DoP; `None` for a rate request.
    pub channels: Option<u16>,
    /// Whether a refusal is fatal — see above.
    pub dop: bool,
}

impl RequestedFormat {
    /// A DoP demand: both axes pinned, no fallback.
    pub fn dop(format: DopFormat) -> Self {
        Self {
            sample_rate: format.sample_rate,
            channels: Some(format.channels),
            dop: true,
        }
    }

    /// A request to open at this track's own rate (#600).
    pub fn source_rate(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: None,
            dop: false,
        }
    }

    /// The DoP format this request carries, for the handle's record and
    /// for the backends that branch on it. `None` for a rate request,
    /// which is not DoP however exact it turns out to be.
    pub fn as_dop(self) -> Option<DopFormat> {
        self.dop.then_some(DopFormat {
            sample_rate: self.sample_rate,
            // A DoP request always carries its channel count; two is the
            // only sane reading of a malformed one.
            channels: self.channels.unwrap_or(2),
        })
    }
}

/// Pick the right output backend based on the runtime preference.
/// With `exclusive=true`, tries the platform's exclusive backend first
/// and falls back to cpal shared if init fails (device busy, no
/// supported format, COM apartment conflict, …): WASAPI Exclusive on
/// Windows, a raw `hw:` device on Linux, hog mode on macOS. With
/// `exclusive=false`, always cpal.
///
/// The fallback is silent at the caller level — the warning is logged
/// so the user can see in `waveflow.log` why exclusive didn't engage.
pub fn spawn_output_with_mode(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    exclusive: bool,
    requested: Option<RequestedFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    // Forget the last refusal: this open says again whether there is one —
    // before the DoP branch too, which returns early.
    shared
        .exclusive_refusal
        .store(0, std::sync::atomic::Ordering::Release);

    // DoP (#495) is a hard requirement, not a preference: a DoP stream
    // that can't open its exact rate in exclusive form must NOT fall back
    // to a shared / mixed output (the OS mixer would resample the marker
    // cadence into white noise) — the caller instead falls back to
    // ordinary DSD → PCM. So when a DoP format is requested we try the
    // platform's exclusive backend only, and surface its error verbatim:
    // WASAPI Exclusive on Windows, a raw `hw:` device on Linux (ALSA),
    // hog mode on macOS (CoreAudio).
    //
    // A *rate* request (#600) is the other kind and deliberately does not
    // take this branch: it is a preference, so it goes down the ordinary
    // path below and keeps the shared-mode fallback that path provides.
    if let Some(dop) = requested.filter(|r| r.dop) {
        #[cfg(target_os = "windows")]
        return super::wasapi_exclusive::spawn_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(target_os = "linux")]
        return super::alsa_exclusive::spawn_alsa_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(target_os = "macos")]
        return super::coreaudio_exclusive::spawn_coreaudio_exclusive_output_thread(
            shared,
            app,
            device_name,
            Some(dop),
        );
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            let _ = (dop, &shared, &app, &device_name);
            return Err(AppError::Audio(
                "DoP output is not supported on this platform".into(),
            ));
        }
    }

    #[cfg(target_os = "windows")]
    if exclusive {
        match super::wasapi_exclusive::spawn_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: WASAPI Exclusive Mode engaged");
                return Ok(pair);
            }
            Err(err) => {
                let reason = super::engine::ExclusiveRefusal::of(&err);
                tracing::warn!(
                    %err,
                    ?reason,
                    "WASAPI Exclusive init failed, falling back to shared mode"
                );
                record_refusal(&shared, reason);
            }
        }
    }
    // Linux: the same bargain through a raw `hw:` device. The card is
    // usually held by PipeWire or PulseAudio at this point, so the
    // backend asks for it through the reservation protocol before
    // concluding it can't be had.
    #[cfg(target_os = "linux")]
    if exclusive {
        match super::alsa_exclusive::spawn_alsa_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: ALSA exclusive (raw hw:) engaged");
                return Ok(pair);
            }
            Err(err) => {
                let reason = super::alsa_exclusive::refusal_of(&err);
                tracing::warn!(
                    %err,
                    ?reason,
                    "ALSA exclusive init failed, falling back to shared mode"
                );
                record_refusal(&shared, reason);
            }
        }
    }

    // macOS: hog mode, which stops the system mixing anything else into
    // the device. Unlike the DoP path it leaves the device's physical
    // format alone — re-clocking a device the whole machine shares is a
    // price only a marker cadence justifies paying.
    #[cfg(target_os = "macos")]
    if exclusive {
        match super::coreaudio_exclusive::spawn_coreaudio_exclusive_output_thread(
            shared.clone(),
            app.clone(),
            device_name.clone(),
            requested,
        ) {
            Ok(pair) => {
                tracing::info!("audio output: CoreAudio exclusive (hog mode) engaged");
                return Ok(pair);
            }
            Err(err) => {
                let reason = super::engine::ExclusiveRefusal::of(&err);
                tracing::warn!(
                    %err,
                    ?reason,
                    "CoreAudio exclusive init failed, falling back to shared mode"
                );
                record_refusal(&shared, reason);
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let _ = (exclusive, requested); // no exclusive PCM backend on this target

    spawn_output_thread(shared, app, device_name)
}

/// Leave why exclusive was refused where the notice and the Settings card
/// read it ([`SharedPlayback::exclusive_refusal`]).
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
fn record_refusal(shared: &SharedPlayback, reason: super::engine::ExclusiveRefusal) {
    shared
        .exclusive_refusal
        .store(reason.as_u8(), std::sync::atomic::Ordering::Release);
}

/// Drain one period out of the ring into `samples`, applying the same
/// per-sample chain the cpal callback applies: volume and the normalize
/// attenuation. Shared by all three exclusive backends. The mono fold is
/// not here: it is done on the decoder thread (`audio::stereo`), before
/// the balance, which it has to precede.
///
/// Returns how many samples were actually pulled. Silence written
/// because the ring ran dry is deliberately NOT counted — every backend
/// agrees on that, because `samples_played` is the only clock the
/// progress bar, the lyrics sync and play-event crediting have, and
/// crediting an underrun would make the track run ahead of itself.
///
/// `_channels` is the frame width the backend negotiated. Nothing here
/// needs it since the mono fold left; it stays so the three backends'
/// calls, two of which only compile on their own OS, did not have to move.
pub(super) fn fill_pcm_period(
    shared: &SharedPlayback,
    consumer: &mut Consumer<f32>,
    samples: &mut [f32],
    _channels: usize,
) -> u64 {
    let volume = shared.volume();
    let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
    // Normalization applies a -3 dB reduction to leave headroom.
    let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };
    let mut written: u64 = 0;

    for slot in samples.iter_mut() {
        *slot = match consumer.pop() {
            Ok(s) => {
                written += 1;
                s * volume * norm_gain
            }
            Err(_) => 0.0,
        };
    }

    written
}

/// Handle retained by the engine so it can tear the output thread down
/// cleanly on shutdown or device switch. Separate from the decoder-side
/// `Producer` which is handed off independently — see the tuple returned
/// from [`spawn_output_thread`].
pub struct OutputHandle {
    pub shutdown_tx: Sender<()>,
    pub join: JoinHandle<()>,
    /// Device name this output was *asked* for — `None` means the OS
    /// default. It stays the request even when the backend fell back to
    /// the default because the name was no longer enumerated: the picker
    /// highlights it, a same-device pick no-ops on it, and a rebuild or a
    /// DoP reopen goes back to it, so the user's pin survives until the
    /// device returns. That also makes it no endpoint identity.
    pub device_name: Option<String>,
    /// Device this output actually opened, when the backend could name
    /// it. Distinct from [`Self::device_name`] on purpose: that one is
    /// the user's pin and must survive a fallback, this one is where
    /// the audio really goes. They differ whenever the pinned name was
    /// no longer enumerated and the backend silently took the default
    /// instead — the case where the picker used to tick a device that
    /// was not playing anything (#612). `None` means the backend opened
    /// something it cannot name, which is reported as "unknown" rather
    /// than guessed.
    pub opened_device: Option<String>,
    /// Whether this handle really owns its device — WASAPI Exclusive
    /// Mode on Windows, a raw `hw:` handle on Linux. The user
    /// preference can request it, but startup may fall back to cpal
    /// shared mode when the device rejects it.
    pub exclusive: bool,
    /// `Some(fmt)` when this output is carrying a DoP (DSD over PCM)
    /// stream, opened at exactly that rate (`dsd_rate / 16`) and channel
    /// count, #495. `None` for every ordinary PCM output. The engine
    /// reads it to decide whether the next track needs an output
    /// rebuild: a DoP track whose format differs in *any* field, or any
    /// PCM track after a DoP one, forces a re-open; a DoP track with the
    /// identical format can reuse it. The channel count is part of the
    /// comparison because the exclusive stream is opened for a fixed
    /// interleave — a stereo output handed 5.1 DoP frames would tear
    /// them across channels.
    pub dop: Option<DopFormat>,
}

impl OutputHandle {
    /// Signal the output thread to drop its Stream and exit, then wait
    /// for it. Called from `AudioEngine::shutdown` and `Drop`.
    pub fn stop(self) {
        // Ignore the send error — the receiver may already be gone if
        // the stream errored out on its own.
        let _ = self.shutdown_tx.send(());
        let _ = self.join.join();
    }
}

/// Spawn the dedicated output thread. Returns the decoder-side
/// `Producer<f32>` (hand it to the decoder) and an [`OutputHandle`] the
/// engine keeps around for teardown.
///
/// The thread is named `waveflow-audio-output` so it's easy to spot in
/// profilers / `perf top`. Any error during Stream construction is
/// surfaced via an init-result channel before this function returns, so
/// the caller learns synchronously whether playback is usable.
///
/// Takes an [`AppHandle`] so the cpal error callback can emit
/// `player:error` + `player:state` events on device loss (headphones
/// unplugged mid-playback).
pub fn spawn_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<Option<String>>>(1);

    let thread_shared = shared.clone();
    let thread_app = app.clone();
    let thread_device = device_name.clone();
    let join = std::thread::Builder::new()
        .name("waveflow-audio-output".into())
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
        .map_err(|e| AppError::Audio(format!("spawn output thread: {e}")))?;

    // Block until the thread reports whether the Stream opened cleanly.
    // Any failure here means we never reached `stream.play()`.
    match init_rx.recv() {
        Ok(Ok(opened_device)) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                opened_device,
                exclusive: false,
                dop: None,
            },
        )),
        Ok(Err(err)) => {
            // The thread already exited; join it so we don't leak.
            let _ = join.join();
            Err(err)
        }
        Err(_) => Err(AppError::Audio(
            "output thread died before reporting init result".into(),
        )),
    }
}

/// Thread body. Owns the `!Send` `cpal::Stream` locally, so nothing
/// crosses a thread boundary.
fn output_thread_main(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    shutdown_rx: Receiver<()>,
    init_tx: Sender<AppResult<Option<String>>>,
    app: AppHandle,
    device_name: Option<String>,
) {
    // Kept here as well as in the error callback so the receiver never
    // disconnects while the thread waits on it.
    let (stall_tx, stall_rx) = bounded::<()>(1);
    let xruns = Arc::new(AtomicU64::new(0));
    let (stream, opened_device, format) = match build_stream(
        shared.clone(),
        consumer,
        app.clone(),
        device_name,
        stall_tx.clone(),
        xruns.clone(),
    ) {
        Ok(pair) => pair,
        Err(err) => {
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    if let Err(err) = stream
        .play()
        .map_err(|e| AppError::Audio(format!("stream play: {e}")))
    {
        let _ = init_tx.send(Err(err));
        return;
    }

    // Only now the stream is really there: a device switch keeps the old one
    // when the new one is refused, and the decoder would go on writing the
    // refused format into it, 2-channel frames into an 8-channel ring at four
    // times the speed.
    format.publish(&shared);

    // Signal successful initialization, naming the endpoint that really
    // opened so the engine can report it rather than the request.
    let _ = init_tx.send(Ok(opened_device));

    // Park until the engine says shutdown. The Stream runs its callback
    // on its own (WASAPI-managed) thread on Windows, so we just need to
    // keep the Stream alive here — and log, now and then, the underruns
    // cpal recovered from, which its error callback only counts.
    let opened_at = std::time::Instant::now();
    let stalled = loop {
        crossbeam_channel::select! {
            recv(shutdown_rx) -> _ => break false,
            recv(stall_rx) -> _ => break true,
            default(XRUN_LOG_EVERY) => log_recovered_xruns(&xruns),
        }
    };
    drop(stream);
    drop(stall_tx);
    log_recovered_xruns(&xruns);
    if stalled {
        retry_stalled_output(&app, &shutdown_rx, opened_at.elapsed());
    } else if opened_at.elapsed() >= STALL_HOLD {
        // A stream that worked and was then replaced ends the streak too,
        // or the next device to stall would inherit its backoff.
        STALL_STREAK.store(0, Ordering::Relaxed);
    }
    tracing::debug!("audio output thread exiting");
}

/// How often the output thread logs the underruns cpal recovered from.
const XRUN_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// One line for the underruns counted since the last one, if any.
fn log_recovered_xruns(xruns: &AtomicU64) {
    let count = xruns.swap(0, Ordering::Relaxed);
    if count > 0 {
        tracing::info!(count, "audio output underran; cpal recovered the stream");
    }
}

/// Consecutive streams that got stuck soon after they opened. Process
/// wide because each rebuild spawns a new output thread, and the backoff
/// has to outlive them.
static STALL_STREAK: AtomicU32 = AtomicU32::new(0);

/// A stream that ran this long before getting stuck counts as having
/// worked: its stall starts the backoff over.
const STALL_HOLD: std::time::Duration = std::time::Duration::from_secs(10);
/// Longer than the rebuild gate's settle window plus the 300 ms delay in
/// [`schedule_device_rebuild`], so a retry is not turned down for the
/// same reason as the request before it.
const STALL_RETRY_MIN: std::time::Duration =
    super::engine::REBUILD_SETTLE_WINDOW.saturating_add(std::time::Duration::from_secs(1));
const STALL_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// How long to wait before the first rebuild after the `streak`-th stall
/// in a row: at once for a stream that had been working, then doubling
/// from [`STALL_RETRY_MIN`] up to [`STALL_RETRY_MAX`].
fn stall_retry_delay(streak: u32) -> std::time::Duration {
    match streak {
        0 => std::time::Duration::ZERO,
        n => STALL_RETRY_MIN
            .saturating_mul(1u32 << (n - 1).min(8))
            .min(STALL_RETRY_MAX),
    }
}

/// The wait after a rebuild request that did not replace this output —
/// the gate turned it down, or the rebuild failed.
fn next_stall_retry(delay: std::time::Duration) -> std::time::Duration {
    delay
        .saturating_mul(2)
        .clamp(STALL_RETRY_MIN, STALL_RETRY_MAX)
}

/// Runs on the output thread once its stream is dropped for being stuck.
///
/// The stream had to go: cpal's ALSA loop answers a device that keeps
/// raising `POLLERR` by polling it again at once, and a VM's sound device
/// kept that thread at 77 % of a core with every error silenced. Dropping
/// the stream stops the loop, but also the errors that used to carry the
/// retries, so the retries live here: ask for a rebuild, and ask again
/// until one replaces this output, which stops it through `shutdown_rx`.
///
/// A device that fails again right after each rebuild is retried less and
/// less often, up to once a minute; one that had been playing gets its
/// rebuild at once.
fn retry_stalled_output(app: &AppHandle, shutdown_rx: &Receiver<()>, lived: std::time::Duration) {
    let streak = if lived >= STALL_HOLD {
        STALL_STREAK.store(1, Ordering::Relaxed);
        0
    } else {
        STALL_STREAK.fetch_add(1, Ordering::Relaxed)
    };
    let mut delay = stall_retry_delay(streak);
    loop {
        match shutdown_rx.recv_timeout(delay) {
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            _ => return,
        }
        tracing::info!(streak, "rebuilding a stuck audio output");
        schedule_device_rebuild(app, RebuildTarget::Resolve);
        delay = next_stall_retry(delay);
    }
}

/// Build the cpal `Stream`. Called only from inside the output thread.
///
/// Returns the device actually opened alongside the stream: the lookup
/// below falls back to the default endpoint when the pinned name is no
/// longer enumerated, and the caller has no other way to learn that
/// (#612).
fn build_stream(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
    device_name: Option<String>,
    stall_tx: Sender<()>,
    xruns: Arc<AtomicU64>,
) -> AppResult<(Stream, Option<String>, StreamFormat)> {
    silence_alsa_stderr(|| build_stream_inner(shared, consumer, app, device_name, stall_tx, xruns))
}

/// The device the OS default output is right now, as a fixed endpoint.
///
/// Since cpal 0.18 the handle `default_output_device` returns on Windows
/// and macOS follows the system default by itself: a stream built on it
/// reroutes when the default moves and reports the move as an error
/// (`StreamInvalidated` on WASAPI, `DeviceChanged` on CoreAudio). Moving
/// to a new default is the engine's job (#627) — it reopens with the
/// user's settings and keeps playing — so the stream is built on the
/// enumerated endpoint with the same id instead, as cpal 0.17 did. ALSA's
/// default is the `default` PCM either way.
fn default_output_endpoint(host: &cpal::Host) -> AppResult<cpal::Device> {
    let default = host
        .default_output_device()
        .ok_or_else(|| AppError::Audio("no default audio output device".into()))?;
    #[cfg(not(target_os = "linux"))]
    if let (Ok(id), Ok(devices)) = (default.id(), host.output_devices()) {
        if let Some(endpoint) = devices
            .into_iter()
            .find(|d| d.id().is_ok_and(|other| other == id))
        {
            return Ok(endpoint);
        }
    }
    Ok(default)
}

fn build_stream_inner(
    shared: Arc<SharedPlayback>,
    consumer: Consumer<f32>,
    app: AppHandle,
    device_name: Option<String>,
    stall_tx: Sender<()>,
    xruns: Arc<AtomicU64>,
) -> AppResult<(Stream, Option<String>, StreamFormat)> {
    let host = cpal::default_host();
    // If a specific device was requested, look it up by name. If the
    // user picked a device that since vanished (USB DAC unplugged
    // between sessions), fall back to the OS default rather than
    // erroring out — the alternative is a silent app on next launch.
    let device = match device_name.as_deref() {
        Some(name) => {
            let mut found = None;
            if let Ok(iter) = host.output_devices() {
                for d in iter {
                    if device_key(&d).as_deref() == Some(name) {
                        found = Some(d);
                        break;
                    }
                }
            }
            match found {
                Some(d) => d,
                None => {
                    tracing::warn!(
                        device = %name,
                        "requested output device not found, falling back to default"
                    );
                    default_output_endpoint(&host)?
                }
            }
        }
        None => default_output_endpoint(&host)?,
    };

    // Read the name off the device we ended up with, not the one we
    // asked for. These differ exactly when the fallback above fired,
    // which is the case the picker used to misreport (#612).
    let opened_device = device_key(&device);

    let default_cfg = device
        .default_output_config()
        .map_err(|e| AppError::Audio(format!("default output config: {e}")))?;
    #[cfg(target_os = "linux")]
    let default_cfg = linux_layout::widen_to_sink(&device, device_name.as_deref(), default_cfg);

    let sample_format = default_cfg.sample_format();
    let channels = default_cfg.channels();
    let sample_rate = default_cfg.sample_rate();
    let config: StreamConfig = default_cfg.into();

    // cpal 0.18 ranks the integer formats by width when a device has no
    // `F32` (`I32` > `I24` > `I16`), so a raw `hw:` PCM now opens at its
    // widest format rather than at 16 bits.
    let open = (consumer, shared, app, stall_tx, xruns);
    let stream = match sample_format {
        SampleFormat::F32 => open_stream::<f32>(&device, config, open),
        SampleFormat::F64 => open_stream::<f64>(&device, config, open),
        SampleFormat::I32 => open_stream::<i32>(&device, config, open),
        SampleFormat::I24 => open_stream::<cpal::I24>(&device, config, open),
        SampleFormat::I16 => open_stream::<i16>(&device, config, open),
        SampleFormat::U16 => open_stream::<u16>(&device, config, open),
        other => Err(AppError::Audio(format!(
            "unsupported sample format: {other:?}"
        ))),
    }?;

    tracing::info!(
        sample_rate,
        channels,
        ?sample_format,
        "cpal output stream opened"
    );

    Ok((
        stream,
        opened_device,
        StreamFormat {
            sample_rate,
            channels,
            sample_format,
        },
    ))
}

/// What a cpal stream was opened at.
struct StreamFormat {
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
}

impl StreamFormat {
    /// Stamp the device config into the shared state so the decoder and
    /// position helper can compute timing without re-querying cpal.
    fn publish(&self, shared: &SharedPlayback) {
        shared
            .sample_rate
            .store(self.sample_rate, Ordering::Release);
        shared.channels.store(self.channels, Ordering::Release);
        // What a shared stream's channels are. On Windows the count is the mix
        // format's, in its WAVE order, and cpal's mask says to take the channels
        // as they come. An ALSA PCM has its own order.
        #[cfg(target_os = "linux")]
        let layout = ChannelLayout::alsa_default(usize::from(self.channels)).pack();
        #[cfg(not(target_os = "linux"))]
        let layout = 0;
        shared.output_layout.store(layout, Ordering::Release);
        shared.set_sample_format(match self.sample_format {
            SampleFormat::F32 => super::state::OutputSampleFormat::F32,
            SampleFormat::I32 => super::state::OutputSampleFormat::S32,
            // cpal's `I24` is 24 bits in a 32-bit container
            // (`SampleFormat::sample_size` is 4), on every backend.
            SampleFormat::I24 => super::state::OutputSampleFormat::S24In32,
            SampleFormat::I16 => super::state::OutputSampleFormat::S16,
            SampleFormat::U16 => super::state::OutputSampleFormat::U16,
            _ => super::state::OutputSampleFormat::Unknown,
        });
    }
}

/// What to do with one error from a cpal stream. See [`StreamErrorStorm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StormAction {
    /// The stream's first error: log it, tell the UI, and rebuild if the
    /// device is gone.
    Report { rebuild: bool },
    /// Errors are arriving too fast to be anything but a stuck stream:
    /// rebuild the output.
    Rebuild { errors: u32 },
    /// Already reported, and a rebuild already asked for recently.
    Quiet,
}

/// Errors from one cpal stream, so that a backend stuck in an error loop
/// costs one report and a rebuild rather than a log line and two UI
/// events per error. Lives as long as the stream: a rebuilt output gets a
/// fresh one, so a device that fails again after a rebuild is reported
/// again — at the rebuild gate's pace, not the error loop's.
///
/// A storm's rebuild is carried out by the output thread, which drops the
/// stream and keeps retrying on its own ([`retry_stalled_output`]): the
/// errors that would otherwise re-ask stop with the stream. A device loss
/// is still re-asked every [`Self::RETRY`] while the stream lives — the
/// rebuild gate turns a request down while another rebuild is armed or
/// settling.
#[derive(Debug, Default)]
struct StreamErrorStorm {
    reported: bool,
    /// A `DeviceNotAvailable` already answered with a rebuild. Kept apart
    /// from the storm: the device can go away after other errors were
    /// already reported, and its rebuild must not be swallowed with them.
    device_loss_handled: bool,
    last_rebuild: Option<std::time::Instant>,
    window_start: Option<std::time::Instant>,
    in_window: u32,
    total: u32,
    /// Xruns are counted apart: cpal recovers from each one, so they make
    /// a storm only at a rate no recovering stream can reach.
    xrun_window_start: Option<std::time::Instant>,
    xruns_in_window: u32,
}

impl StreamErrorStorm {
    /// Errors within [`Self::WINDOW`] that make a storm. A real device
    /// error arrives once, or a handful of times while it goes away; a
    /// stuck backend reports thousands a second.
    const THRESHOLD: u32 = 50;
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
    /// Longer than the rebuild gate's settle window, so a retry is not
    /// turned down for the same reason as the request before it.
    const RETRY: std::time::Duration =
        super::engine::REBUILD_SETTLE_WINDOW.saturating_add(std::time::Duration::from_secs(1));
    /// Xruns within [`Self::WINDOW`] that make a storm. A stream that
    /// recovers underruns at most once a period — a few hundred a second
    /// at the smallest periods, 24 a second measured under a PipeWire
    /// quantum forced to twice the period — while a backend that fails to
    /// recover loops millions of times a second.
    const XRUN_THRESHOLD: u32 = 1000;

    /// An xrun cpal has already recovered from (`ErrorKind::Xrun`): it is
    /// never reported — the stream plays on — and asks for a rebuild only
    /// when xruns come faster than any recovering stream produces them.
    fn on_xrun(&mut self, now: std::time::Instant) -> StormAction {
        match self.xrun_window_start {
            Some(start) if now.duration_since(start) < Self::WINDOW => {
                self.xruns_in_window = self.xruns_in_window.saturating_add(1);
            }
            _ => {
                self.xrun_window_start = Some(now);
                self.xruns_in_window = 1;
            }
        }
        let due = self
            .last_rebuild
            .map_or(true, |last| now.duration_since(last) >= Self::RETRY);
        if self.xruns_in_window < Self::XRUN_THRESHOLD || !due {
            return StormAction::Quiet;
        }
        self.last_rebuild = Some(now);
        StormAction::Rebuild {
            errors: self.xruns_in_window,
        }
    }

    /// `device_gone`: the error is `DeviceNotAvailable`.
    fn on_error(&mut self, now: std::time::Instant, device_gone: bool) -> StormAction {
        self.total = self.total.saturating_add(1);
        match self.window_start {
            Some(start) if now.duration_since(start) < Self::WINDOW => {
                self.in_window = self.in_window.saturating_add(1);
            }
            _ => {
                self.window_start = Some(now);
                self.in_window = 1;
            }
        }
        if !self.reported {
            self.reported = true;
            self.device_loss_handled = device_gone;
            if device_gone {
                self.last_rebuild = Some(now);
            }
            return StormAction::Report {
                rebuild: device_gone,
            };
        }
        let due = self
            .last_rebuild
            .map_or(true, |last| now.duration_since(last) >= Self::RETRY);
        // A device loss needs no storm to be worth a rebuild: the first
        // one gets it at once, and a later one again once the retry delay
        // has passed — the gate may have turned the previous request down.
        if device_gone && (!self.device_loss_handled || due) {
            self.device_loss_handled = true;
            self.last_rebuild = Some(now);
            return StormAction::Rebuild { errors: self.total };
        }
        if due && self.in_window >= Self::THRESHOLD {
            self.last_rebuild = Some(now);
            return StormAction::Rebuild { errors: self.total };
        }
        StormAction::Quiet
    }
}

/// What one error from a cpal stream says about the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamErrorClass {
    /// An underrun cpal has already recovered from; the stream plays on.
    Recovered,
    /// A notice that leaves the stream playing: a default-device stream
    /// rerouted by the OS, or real-time scheduling refused.
    PlaysOn,
    /// Something went wrong with the stream; `device_gone` when the
    /// device itself went away.
    Fault { device_gone: bool },
}

fn stream_error_class(kind: cpal::ErrorKind) -> StreamErrorClass {
    match kind {
        cpal::ErrorKind::Xrun => StreamErrorClass::Recovered,
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {
            StreamErrorClass::PlaysOn
        }
        cpal::ErrorKind::DeviceNotAvailable => StreamErrorClass::Fault { device_gone: true },
        _ => StreamErrorClass::Fault { device_gone: false },
    }
}

/// What a stream's two callbacks own: the ring's consumer and the shared
/// state for the data callback; the app handle, the stall signal and the
/// count of underruns cpal recovered from (logged by the output thread)
/// for the error callback.
type StreamParts = (
    Consumer<f32>,
    Arc<SharedPlayback>,
    AppHandle,
    Sender<()>,
    Arc<AtomicU64>,
);

/// Generic stream builder parameterized by the device's native sample
/// format. We always decode into `f32` internally and let cpal convert
/// to whatever the device wants at the last second via `FromSample`.
fn open_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    (mut consumer, shared, app, stall_tx, xruns): StreamParts,
) -> AppResult<Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + Send + 'static,
{
    // On device loss (headphones unplugged, sound server restart)
    // cpal fires this callback on a random thread. We flip state to
    // Paused, emit player:state + player:error so the UI can surface
    // the problem, and keep the Stream itself untouched — it's about
    // to be dropped by cpal anyway.
    //
    // On `DeviceNotAvailable` (#175) we additionally schedule an
    // auto-rebuild — see [`schedule_device_rebuild`]. The engine's
    // debounce guard coalesces a quick double-flap into a single
    // rebuild attempt.
    //
    // A stream reports each error once and moves on — except when the
    // backend is stuck: cpal's ALSA loop answers a `POLLERR` by calling
    // this again straight away, and a VM's sound device produced 3.7
    // million of them in an evening, each one logged and each one sent
    // to the webview as two events. The log grew by gigabytes and the
    // app sat at half a core. [`StreamErrorStorm`] keeps the first report
    // and turns a storm into a stall signal: the output thread drops the
    // stream — which stops the loop, where silencing it alone did not —
    // and rebuilds on a backoff ([`retry_stalled_output`]).
    //
    // Not every error is a fault. Since cpal 0.18 an underrun arrives as
    // `ErrorKind::Xrun` *after* cpal has recovered from it: its ALSA loop
    // re-prepares the PCM and plays on (a failed re-prepare is reported
    // as an error of its own). PipeWire underruns whenever it reshapes its
    // graph — another app opening a stream, a quantum change — and 0.17
    // answered each one with a `POLLERR` loop that only the storm's
    // rebuild ended, a 1-2 s cut every time (#1103). An xrun is therefore
    // only counted — the output thread logs the count, since on ALSA this
    // closure runs on the audio thread — and makes a storm only at a rate
    // no recovering stream reaches ([`StreamErrorStorm::on_xrun`]).
    let err_shared = shared.clone();
    let err_app = app.clone();
    let mut storm = StreamErrorStorm::default();
    let err_fn = move |err: cpal::Error| {
        let now = std::time::Instant::now();
        let action = match stream_error_class(err.kind()) {
            StreamErrorClass::Recovered => {
                xruns.fetch_add(1, Ordering::Relaxed);
                storm.on_xrun(now)
            }
            StreamErrorClass::PlaysOn => {
                tracing::info!(?err, "cpal stream notice");
                StormAction::Quiet
            }
            StreamErrorClass::Fault { device_gone } => storm.on_error(now, device_gone),
        };
        match action {
            StormAction::Report { rebuild } => {
                tracing::warn!(?err, "cpal stream error");
                notify_device_lost(&err_app, &err_shared, format!("audio device error: {err}"));
                if rebuild {
                    // The erroring handle is still parked in the engine's
                    // `self.output`, so the rebuild can self-resolve its device.
                    schedule_device_rebuild(&err_app, RebuildTarget::Resolve);
                }
            }
            StormAction::Rebuild { errors } => {
                tracing::warn!(
                    ?err,
                    errors,
                    "cpal stream keeps failing; dropping it and rebuilding the output"
                );
                // Full means the output thread was already told.
                let _ = stall_tx.try_send(());
            }
            StormAction::Quiet => {}
        }
    };

    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _info: &cpal::OutputCallbackInfo| {
                // Hard pause: while paused, the decoder stops pushing
                // into the ring, and we stop draining it so the next
                // `resume` picks right back up where it left off. We
                // just write silence into the device buffer — users
                // hear the pause within ~WASAPI's internal latency
                // (~50-200 ms) instead of the full ring length.
                if shared.paused_output.load(Ordering::Acquire) {
                    for slot in out.iter_mut() {
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Drain-silent mode: drop whatever's left in the
                // ring as fast as possible and write silence. Used
                // during a track switch / seek so the tail of the
                // old position never reaches the device. We pop in
                // bulk (not one-per-output-slot) so the decoder's
                // spin-wait on `producer.slots() == RING_CAPACITY`
                // completes in a single callback (~10-15 ms) instead
                // of waiting for the device to consume the whole queue
                // at its native cadence (a second of audio).
                if shared.drain_silent.load(Ordering::Acquire) {
                    while consumer.pop().is_ok() {}
                    for slot in out.iter_mut() {
                        *slot = T::from_sample(0.0_f32);
                    }
                    return;
                }

                // Read atomic flags once per buffer — cheap relaxed
                // loads that avoid ~5k redundant ops per callback. Mono
                // is folded on the decoder thread, before the balance.
                let volume = shared.volume();
                let normalize = shared.normalize_enabled.load(Ordering::Relaxed);
                // Normalization applies a −3 dB gain reduction (× 0.707)
                // to prevent clipping on loud source material.
                let norm_gain: f32 = if normalize { 0.707 } else { 1.0 };

                let mut written: u64 = 0;

                for slot in out.iter_mut() {
                    let sample = match consumer.pop() {
                        Ok(s) => {
                            written += 1;
                            s
                        }
                        Err(_) => 0.0,
                    };
                    *slot = T::from_sample(sample * volume * norm_gain);
                }

                if written > 0 {
                    shared.samples_played.fetch_add(written, Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| AppError::Audio(format!("build_output_stream: {e}")))?;

    Ok(stream)
}

#[cfg(test)]
mod storm_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod alsa_hint_tests;
