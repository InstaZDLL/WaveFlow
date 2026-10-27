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
//! [`layouts::collect_layout_candidates`]. The decoder's existing rubato
//! resampler still converts every source track to that rate — so this is
//! "bypass the OS mixer" rather than "honor the source rate
//! exactly". Per-track sample-rate switching is a future phase.
//!
//! Same SPSC ring contract as the cpal backend (`Producer<f32>` →
//! `Consumer<f32>`), so the decoder thread doesn't know which
//! backend is driving the device.

use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{bounded, Receiver, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use tauri::AppHandle;
use wasapi::{AudioClient, AudioRenderClient, Handle, SampleType, WaveFormat};

use super::output::{OutputHandle, RequestedFormat, RING_CAPACITY};
use super::state::SharedPlayback;
use crate::error::{AppError, AppResult};

mod event_loop;
mod layouts;
mod probe;
mod session;

pub(super) use probe::probe_capabilities;

use event_loop::run_event_loop;
use session::open_exclusive_session;

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
/// Why an exclusive open failed, from the `HRESULT` the error carries.
///
/// The `wasapi` crate reports Win32 failures as text, with the code in it
/// (`HRESULT(0x8889000A)`), and those codes are stable; the format probe
/// says so in its own words. Anything else is `Unknown`.
pub fn refusal_of(err: &AppError) -> super::engine::ExclusiveRefusal {
    use super::engine::ExclusiveRefusal;
    if let AppError::ExclusiveRefused { reason, .. } = err {
        return *reason;
    }
    let text = err.to_string().to_ascii_uppercase();
    if text.contains("8889000A") {
        // AUDCLNT_E_DEVICE_IN_USE
        ExclusiveRefusal::Busy
    } else if text.contains("8889000E") {
        // AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED
        ExclusiveRefusal::NotAllowed
    } else if text.contains("88890008")
        || text.contains("IS_SUPPORTED REJECTED")
        || text.contains("NO USABLE SAMPLE RATE")
    {
        // AUDCLNT_E_UNSUPPORTED_FORMAT, the probe before the open, or a
        // device that reports no layout to open at all
        ExclusiveRefusal::Format
    } else {
        ExclusiveRefusal::Unknown
    }
}

pub fn spawn_exclusive_output_thread(
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    device_name: Option<String>,
    requested: Option<RequestedFormat>,
) -> AppResult<(Producer<f32>, OutputHandle)> {
    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
    let (init_tx, init_rx) = bounded::<AppResult<Option<String>>>(1);

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
                requested,
            )
        })
        .map_err(|e| AppError::Audio(format!("spawn wasapi exclusive thread: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(opened_device)) => Ok((
            producer,
            OutputHandle {
                shutdown_tx,
                join,
                device_name,
                opened_device,
                exclusive: true,
                // A rate request is not DoP however exact it lands: the
                // handle records the packing, not the accuracy.
                dop: requested.and_then(RequestedFormat::as_dop),
            },
        )),
        Ok(Err(err)) => {
            let _ = join.join();
            Err(refusal_of(&err).refuse(err))
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
    init_tx: Sender<AppResult<Option<String>>>,
    app: AppHandle,
    device_name: Option<String>,
    requested: Option<RequestedFormat>,
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
    // Declared here so it drops last, after the session it guards. The
    // event loops used to call `deinitialize()` by hand at their end —
    // with the client and render interfaces still alive in that scope,
    // so they were released *after* `CoUninitialize`. And the early
    // return below, which a busy device takes every time, never
    // balanced the init at all.
    let _com = ComApartment;
    // Before the open, so the very first period is already served at
    // the priority every later one gets.
    let _priority = ProAudioThread::register();

    let session = match open_exclusive_session(&device_name, &shared, requested) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(?err, "wasapi exclusive init failed");
            let _ = init_tx.send(Err(err));
            return;
        }
    };

    // Signal a successful init back to the caller so the engine can
    // proceed to spawn the decoder thread, naming the endpoint that
    // really opened rather than the one that was requested (#612).
    let _ = init_tx.send(Ok(session.opened_device.clone()));

    tracing::info!(
        sample_rate = session.sample_rate,
        channels = session.channels,
        buffer_frames = session.buffer_frames,
        "wasapi exclusive stream opened"
    );

    let exit = run_event_loop(session, consumer, shutdown_rx, &shared);

    match exit {
        ExitReason::Shutdown => {
            tracing::debug!("wasapi exclusive output thread exiting");
        }
        // #405: the exclusive backend runs its own event loop, entirely
        // separate from cpal's error callback — so when the device goes
        // away here (`wait_for_event` / `write_to_device` failing), the
        // thread used to just return and nobody ever heard about it. The
        // engine kept its `wasapi_exclusive = true` handle for a thread
        // that no longer exists, `wasapi_exclusive_active` stayed latched
        // on, and the Settings toggle was stuck showing "on" until the
        // app restarted. Report the loss down the same two paths the cpal
        // callback uses: tell the UI, then schedule a rebuild (which is
        // what re-syncs the toggle, via `player:audio-mode-changed`).
        ExitReason::DeviceLost(reason) => {
            tracing::warn!(
                %reason,
                "wasapi exclusive output thread lost the device; requesting rebuild"
            );
            super::output::notify_device_lost(
                &app,
                &shared,
                format!("audio device error: {reason}"),
            );
            // The dead handle is still parked in the engine's
            // `self.output`, so the rebuild can self-resolve its device.
            super::output::schedule_device_rebuild(&app, super::output::RebuildTarget::Resolve);
        }
    }
}

/// Why [`run_event_loop`] returned.
///
/// The distinction matters: a clean shutdown is the engine tearing this
/// thread down on purpose (device switch, mode toggle, app exit) and
/// must NOT trigger a recovery, whereas a device loss must (#405).
enum ExitReason {
    /// The engine asked us to stop via the shutdown channel.
    Shutdown,
    /// The device stopped accepting buffers. Carries the failure for
    /// the user-facing `player:error` message.
    DeviceLost(String),
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
    /// `wBitsPerSample = 32`, valid bits 24, PCM. Four bytes per sample,
    /// and per WAVEFORMATEXTENSIBLE the valid audio is **left-aligned**:
    /// bits 31..8 carry the 24-bit sample, the least-significant byte is
    /// zero padding. Most integrated codecs that "support 24-bit" really
    /// want this layout in exclusive mode.
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

    /// What the rest of the engine calls this format.
    fn sample_format(self) -> super::state::OutputSampleFormat {
        use super::state::OutputSampleFormat as Out;
        match self {
            Self::Float32 => Out::F32,
            Self::Pcm24Packed => Out::S24,
            Self::Pcm24Padded => Out::S24In32,
            Self::Pcm16 => Out::S16,
        }
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

/// Format chain tried for a DoP stream (#495): 24-bit only, packed
/// first (3 bytes = exactly the 24-bit DoP word), then the 32-bit
/// padded container some codecs prefer. Float32 and 16-bit are
/// deliberately excluded — neither can carry a DoP payload intact.
const DOP_FORMAT_CHAIN: [ExclusiveSampleFormat; 2] = [
    ExclusiveSampleFormat::Pcm24Packed,
    ExclusiveSampleFormat::Pcm24Padded,
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

/// Bundle of everything the event loop needs after a successful init.
/// Kept private so callers can't misuse the wasapi handles outside
/// the thread that owns the COM apartment.
struct ExclusiveSession {
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
    /// Friendly name of the endpoint that actually opened. `pick_device`
    /// takes the default when the pinned name is no longer enumerated,
    /// and the picker has to tick what is playing rather than what was
    /// asked for (#612).
    opened_device: Option<String>,
    sample_rate: u32,
    channels: u16,
    buffer_frames: u32,
    /// Format the device actually accepted. Drives the f32 → bytes
    /// conversion inside `run_event_loop`.
    format: ExclusiveSampleFormat,
    /// True when this session carries a DoP (DSD over PCM) stream, #495.
    /// In DoP mode the event loop bypasses every DSP stage (volume,
    /// mono, normalize, clamp) and ships the ring's 24-bit values
    /// bit-exact — the DAC decodes the embedded 1-bit DSD itself.
    dop: bool,
}

/// Balances one [`wasapi::initialize_mta`] on every way out of a scope —
/// the `?` returns and an unwind included — and, because a guard drops
/// after the locals declared under it, only once every interface opened
/// inside that scope is released. Releasing one *after* `CoUninitialize`
/// is undefined, which is what hand-written pairings kept getting wrong
/// here: the event loops called `deinitialize()` while their client and
/// render interfaces were still alive.
///
/// It matters most in [`probe_capabilities`], which has a dozen exits
/// and runs on a **tokio blocking-pool thread**: pooled, long-lived, and
/// reused by everything else that blocks. An unbalanced init there
/// leaves that thread inside an apartment it never asked for, and raises
/// COM's reference count once per probe for the life of the process.
struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

/// The render thread's claim on the CPU, held for as long as it runs.
///
/// An exclusive stream has no system mixer and its buffer behind it: the
/// device asks for one period, 10 ms on most hardware, and a period not
/// written in time plays as a click. At normal priority the thread
/// competes with everything else on the machine, and a burst of work
/// elsewhere starves it — opening the immersive view, with its blur and
/// its video, made every track crackle for about three seconds. Shared
/// mode never did, because cpal raises its own render thread to
/// time-critical and the system mixer adds its buffer on top.
///
/// MMCSS's "Pro Audio" task is what Windows provides for exactly this:
/// the scheduler lifts the thread into the real-time range while it has
/// work and gives the time back when it waits. If the service refuses —
/// it can be disabled — the thread falls back to time-critical, the
/// priority cpal uses for shared mode. Neither is a condition of the
/// stream: a thread left at normal priority still plays, as it always
/// did, and refusing the open over it would cost the user exclusive mode.
///
/// The drop tries to revert the registration, on the thread that made
/// it, which is the only one allowed to. A failed revert is logged and
/// nothing more: the thread is ending.
struct ProAudioThread(Option<windows::Win32::Foundation::HANDLE>);

impl ProAudioThread {
    fn register() -> Self {
        use windows::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority,
            THREAD_PRIORITY_TIME_CRITICAL,
        };
        let mut task_index = 0u32;
        // SAFETY: a static, NUL-terminated task name and a live
        // out-pointer; the call only reads the one and writes the other.
        match unsafe {
            AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task_index)
        } {
            Ok(handle) => {
                tracing::info!(
                    task_index,
                    "exclusive render thread registered with MMCSS (Pro Audio)"
                );
                Self(Some(handle))
            }
            Err(err) => {
                // SAFETY: the pseudo-handle of the calling thread, which
                // is always valid and never needs closing.
                let raised =
                    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
                tracing::warn!(
                    %err,
                    time_critical = raised.is_ok(),
                    "MMCSS refused the exclusive render thread; raising its priority instead"
                );
                Self(None)
            }
        }
    }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: the handle MMCSS returned to this same thread, which
            // is the thread dropping it, released once.
            if let Err(err) = unsafe {
                windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle)
            } {
                tracing::debug!(%err, "MMCSS revert failed");
            }
        }
    }
}
