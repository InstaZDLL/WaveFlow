//! Rebuilding the output on its own: after a device error, when the
//! system default moves (#627), and the gates that keep one rebuild per
//! burst (#322, #365).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::Emitter;

use crate::error::AppResult;

use super::super::output::{DopFormat, OutputHandle};
use super::command::AudioCmd;
use super::AudioEngine;

/// A WASAPI-exclusive "flap storm" (#322) is this many `DeviceNotAvailable`
/// rebuilds within [`EXCLUSIVE_FLAP_WINDOW`]. Some onboard codecs (Realtek)
/// reset the device right after an exclusive grab, so every re-engage
/// re-triggers the reset and the engine thrashes forever.
const EXCLUSIVE_FLAP_THRESHOLD: u32 = 3;
/// Time window over which [`EXCLUSIVE_FLAP_THRESHOLD`] flaps trip the
/// session-level exclusive suppression.
pub(super) const EXCLUSIVE_FLAP_WINDOW: Duration = Duration::from_secs(12);

/// Rolling counter of exclusive-mode device flaps, behind
/// [`AudioEngine::try_rebuild_after_device_error`].
#[derive(Default)]
pub(super) struct FlapWindow {
    count: u32,
    window_start: Option<Instant>,
}

impl FlapWindow {
    /// Record one flap at `now` and report whether it trips the storm
    /// threshold ([`EXCLUSIVE_FLAP_THRESHOLD`] flaps within
    /// [`EXCLUSIVE_FLAP_WINDOW`]). A flap outside the current window
    /// restarts the count at 1. Clock is injected so the logic is
    /// deterministic under test.
    pub(super) fn record(&mut self, now: Instant) -> bool {
        let within_window = self
            .window_start
            .is_some_and(|start| now.duration_since(start) <= EXCLUSIVE_FLAP_WINDOW);
        if within_window {
            self.count += 1;
        } else {
            self.window_start = Some(now);
            self.count = 1;
        }
        self.count >= EXCLUSIVE_FLAP_THRESHOLD
    }
}

/// Quiet period after a rebuild attempt finishes during which a fresh
/// `DeviceNotAvailable` does NOT arm another rebuild (#365).
///
/// On a DAC that resets its audio session every time a client grabs it in
/// exclusive mode, our own re-open is what produces the next error — so
/// reacting to it re-arms the loop. Each pass would then open exclusive
/// while the previous pass's stream was still settling, and the second
/// open collides with `AUDCLNT_E_DEVICE_IN_USE (0x8889000A)`, whose shared
/// fallback then fails too, leaving the engine and the Settings toggle
/// inconsistent (the #355 report).
///
/// 2 s is long enough to cover such a self-inflicted reset and short
/// enough that a genuine unplug still recovers promptly.
pub(in crate::audio) const REBUILD_SETTLE_WINDOW: Duration = Duration::from_secs(2);

/// Single-owner gate for the automatic post-`DeviceNotAvailable` rebuild
/// (#365).
///
/// The `rebuild_in_progress` debounce only covers a rebuild *call that is
/// currently executing*. It cannot coalesce the real-world pattern, because
/// the recovery is **deferred**: the cpal error callback schedules the
/// rebuild 300 ms later, so consecutive errors each queue their own task
/// and the passes never overlap — one finishes before the next begins, so
/// every one of them sees a free debounce slot.
///
/// This gate closes that hole by tracking the whole arm → delay → run
/// lifecycle plus a settle window afterwards, so exactly one rebuild runs
/// per burst of device errors.
///
/// Clock is injected so the logic is deterministic under test, same as
/// [`FlapWindow`].
#[derive(Default)]
pub(super) struct RebuildGate {
    /// A deferred rebuild has been scheduled and hasn't finished yet.
    armed: bool,
    /// When the last rebuild attempt finished, successful or not.
    last_finished: Option<Instant>,
}

impl RebuildGate {
    /// Decide whether a `DeviceNotAvailable` should schedule a rebuild.
    /// Returns `true` at most once per burst: `false` while one is already
    /// armed, and `false` inside the settle window after the last one.
    pub(super) fn try_arm(&mut self, now: Instant, settle: Duration) -> bool {
        if self.armed {
            return false;
        }
        if self
            .last_finished
            .is_some_and(|t| now.duration_since(t) < settle)
        {
            return false;
        }
        self.armed = true;
        true
    }

    /// Mark the armed rebuild as finished and start the settle window.
    /// Must run on EVERY exit path of the rebuild, otherwise `armed`
    /// stays latched and no later device error can ever recover.
    pub(super) fn finish(&mut self, now: Instant) {
        self.armed = false;
        self.last_finished = Some(now);
    }

    /// Drop the settle window entirely so the very next [`try_arm`]
    /// succeeds. Used when a deliberate output change we suppressed for
    /// (via [`AudioEngine::begin_deliberate_output_change`]) turns out
    /// to have installed no stream at all: the recovery we then schedule
    /// must not be swallowed by the window we opened for a swap that
    /// never happened (#405).
    pub(super) fn reopen(&mut self) {
        self.armed = false;
        self.last_finished = None;
    }
}

/// Releases the [`RebuildGate`] on **every** exit path of a rebuild,
/// including the early returns and a panic.
///
/// Held by both paths that arm the gate — the device-error recovery and
/// the default-device follow (#627). Without it a rebuild that bailed
/// out would leave `armed` latched and no later device event could ever
/// schedule anything again. Declare it FIRST in the function so it drops
/// last, after the `output` lock has been released: the settle window
/// should start when the rebuild is really over.
struct RebuildGateGuard<'a>(&'a Mutex<RebuildGate>);

impl Drop for RebuildGateGuard<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(Instant::now());
    }
}

/// How long a device loss owns the recovery, keeping the default-device
/// follow off the same rebuild (#617). Comfortably longer than the
/// recovery's own 300 ms backoff, short enough that a default change a
/// few seconds later is still followed normally.
pub(super) const DEVICE_LOSS_OWNERSHIP: Duration = Duration::from_secs(3);

/// Why the output is being rebuilt. It changes exactly one decision:
/// whether landing on a *different* endpoint should park playback
/// instead of resuming it (#617).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RebuildCause {
    /// The stream we were playing on failed — the device may be gone.
    DeviceLost,
    /// We are moving on purpose: the user asked for a reopen, or the
    /// system's default changed while nothing was pinned (#627). Neither
    /// is a device disappearing, and neither pauses anything.
    Deliberate,
}

/// Whether a rebuild put the audio on a different endpoint than the one
/// it was on.
///
/// Unknown on either side means "not moved", deliberately: the names
/// come from different backends (cpal's display name, WASAPI's friendly
/// name, and hog mode can name nothing at all), and the cost of a wrong
/// "moved" is playback stopping for a user who asked for none of this.
/// A wrong "not moved" only costs the pause #617 adds, leaving the
/// behaviour that shipped before it.
pub(super) fn endpoint_moved(before: Option<&str>, after: Option<&str>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if before != after)
}

/// Whether a track's request needs the output reopened.
///
/// Reopened when there is no stream, or when DoP has to start, stop or
/// change shape. Otherwise only a request **for a rate** can call for
/// it, and that rate is compared against what the last open *asked*
/// for, never what it got (#600): a device that refused 96 kHz has its
/// own rate installed, and comparing against that would rebuild the
/// output on every single track for as long as the preference stayed on.
///
/// A request with no rate — the DoP check of an ordinary PCM track, a
/// remote or HTTP load putting the output back on PCM, the test tone —
/// is about DoP only and leaves the rate alone. It used to count as a
/// request for "no rate in particular", which differs from any rate the
/// last track matched: with "Match each track's sample rate" on, every
/// track reopened the output twice, once to the device's default rate
/// for the DoP check and once to its own rate, two gaps where the guard
/// above promised none.
pub(super) fn track_needs_reopen(
    has_output: bool,
    current_dop: Option<DopFormat>,
    want_dop: Option<DopFormat>,
    last_requested_rate: u32,
    requested_rate: Option<u32>,
) -> bool {
    if !has_output || current_dop != want_dop {
        return true;
    }
    requested_rate.is_some_and(|rate| rate != last_requested_rate)
}

/// Whether the old output has to be released *before* the new one is
/// opened, rather than the other way round. `old_is_exclusive` is `None`
/// when there is no stream installed at all.
///
/// Spawn-first is the order we want wherever it works: a failed open then
/// costs nothing, because the stream the user is listening to is still
/// installed and still playing. Two situations take that away.
///
/// - **The old stream is exclusive**, on any platform. It owns the device
///   outright and nothing — shared or exclusive — can open that device
///   until it lets go (#322, then #405 for the other direction).
/// - **We are entering exclusive on macOS.** Hog mode is recorded as a
///   *pid*, and the client it would have to evict here is our own cpal
///   stream, in this very process — so it evicts nothing. On Linux the
///   reservation protocol makes the sound server hand the card over, and
///   on Windows a shared client is simply no obstacle (below); macOS has
///   neither. The new AudioUnit then comes up on a device our old one is
///   still driving and renders nothing: no sound, and a position counter
///   frozen where it stood.
///
///   Measured on a MacBook Air, and only on the toggle. Armed before
///   launch the same code opens on an idle device and plays, which is
///   what made this look like a backend fault rather than an ordering
///   one.
///
/// A **shared** stream is deliberately not a reason to release first, and
/// that is measured rather than assumed. On Windows 11, `Initialize` in
/// exclusive mode succeeds while one of our own shared clients is open and
/// running on the same endpoint; only an *exclusive* client draws
/// `AUDCLNT_E_DEVICE_IN_USE`, and it stops doing so about 19 ms after that
/// client is dropped. Keeping spawn-first here is what preserves the
/// rollback for the ordinary case.
///
/// Releasing first is safe wherever it applies because
/// [`spawn_output_with_mode`] falls back to shared mode on its own, so the
/// caller still comes back holding a stream.
pub(super) fn must_release_before_reopening(
    old_is_exclusive: Option<bool>,
    entering_exclusive: bool,
) -> bool {
    match old_is_exclusive {
        None => false,
        Some(true) => true,
        Some(false) => cfg!(target_os = "macos") && entering_exclusive,
    }
}

/// Which device a forced rebuild targets.
///
/// `Pinned` exists so a caller doesn't have to read the name first. That
/// read takes the output lock and gives it back, and a device pick
/// landing in the gap installs *and persists* another device — after
/// which the rebuild would reinstall the stale one, leaving the stream
/// disagreeing with the saved preference. Resolving inside the rebuild's
/// own acquisition closes the window (#612).
pub(super) enum RebuildDevice {
    /// Whatever the installed handle is pinned to, read under the
    /// rebuild's lock.
    Pinned,
    /// A target the caller computed deliberately — the device-error
    /// recovery resolves its own and has to keep it.
    Explicit(Option<String>),
    /// The OS default, but only while nothing is pinned — the
    /// default-device follow (#627). The condition is re-checked inside
    /// the rebuild's own lock, so a pin installed in the meantime wins
    /// and no rebuild happens at all.
    OsDefaultIfUnpinned,
}

/// What a default-device follow did, so its caller knows whether the
/// change was actually dealt with (#627).
pub(in crate::audio) enum FollowOutcome {
    /// Dealt with: a rebuild ran, or the checks said none was needed.
    Settled,
    /// Not dealt with — the rebuild gate was busy. Nothing was changed,
    /// and the caller has to come back, or the system default and the
    /// open device stay apart until the next notification, which may
    /// never come: moving the default twice in quick succession is one
    /// notification per change, not a repeating signal.
    Deferred,
}

/// How a command leaves the pending-pause flag (#611).
///
/// A `Pause` raises it, and everything that asks for playback clears it:
/// `Resume`, and any load, because a load is a request to play — which
/// is also when the decoder clears its own `paused_output`. Every other
/// command leaves it alone; a `Stop` in particular, since the rebuilds
/// send one and it decides nothing about whether the user wants to hear
/// anything afterwards.
///
/// Pure so the lifecycle can be exercised without an engine.
pub(super) fn pause_pending_after(cmd: &AudioCmd, current: bool) -> bool {
    match cmd {
        AudioCmd::Pause => true,
        AudioCmd::Resume
        | AudioCmd::LoadAndPlay { .. }
        | AudioCmd::LoadRemoteFileAndPlay { .. }
        | AudioCmd::LoadUrlAndPlay { .. } => false,
        _ => current,
    }
}

/// Whether a default-device change is worth a rebuild.
///
/// Pure so the decision can be tested without a sound card;
/// [`AudioEngine::follow_os_default_output`] adds the pin check, which
/// needs the engine's lock.
///
/// - a default the host cannot name (`None`) is nothing to follow:
///   tearing down a working stream to open "no device" would lose the
///   audio we still have;
/// - an output the backend cannot name (`None`) cannot be compared, and
///   "cannot tell" must not mean "do nothing" in the one feature whose
///   whole job is to move — CoreAudio hog mode with no pin lands here;
/// - otherwise the names decide.
///
/// The two names do not always come from the same call: the shared cpal
/// path fills `opened_device` through the same `device_display_name` the
/// default lookup uses, while WASAPI exclusive names itself through
/// `Device::get_friendlyname()`. Both read Windows' friendly name, so
/// they normally agree — and if they ever didn't, the cost is a
/// redundant rebuild on a default change that really happened, never a
/// missed one. Which is why this compares exactly and normalises
/// nothing: a smarter match could only hide a real change.
pub(super) fn should_follow_default(opened: Option<&str>, new_default: Option<&str>) -> bool {
    match (opened, new_default) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(opened), Some(new_default)) => opened != new_default,
    }
}

/// What a device-error rebuild does with the session it interrupts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RebuildResume {
    /// Nothing was loaded: reopen the output and stop there.
    Nothing,
    /// The session was playing: pick the track back up where it was.
    Play,
    /// The user had paused a library track: keep it that way (#611).
    StayPaused,
}

/// Decide [`RebuildResume`] from what the rebuild finds.
///
/// The state alone cannot tell the two sessions apart:
/// [`super::super::output::notify_device_lost`] has already turned a playing one
/// into `Paused` by the time the rebuild runs, so both arrive as `Paused`,
/// and resuming both started music the user had paused on whatever device
/// the system fell back to. `paused_output` does tell them apart. On the
/// playback path only the decoder's own `Pause` raises it, and nothing on
/// the device-loss path touches it (shutdown and `reset_app` raise it too,
/// but neither is followed by a rebuild).
///
/// **`Loading` counts as a session.** The decoder is inside `play_track`
/// from the moment it accepts a load, so a track picked while the
/// rebuild is between its `Stop` and its swap is a track this answer
/// decides the fate of. It used to decide more than that: a
/// `SwapProducer` reaching `play_track` was dropped on the floor, so
/// answering `Nothing` here cost the swap itself and left the decoder
/// writing into a ring whose consumer had just been torn down. That
/// half is fixed at the source — the drains install the producer and
/// end the track (#639) — and what is left is the plain question. The
/// track the user picked has been stopped by the swap, `last_load`
/// holds it, and only `Play` sends it back out, under its own intent.
///
/// `Ended` and `Idle` stay outside: there is no session to interrupt,
/// the decoder is parked at the top-level loop, and resuming would
/// restart a track that had finished.
///
/// Until #617 only a *library* track could stay paused, because what made
/// it resumable afterwards was the persisted resume point `resume_last`
/// loads — and that point is always a library track's, so a radio station
/// or a server track parked the same way came back as the last library
/// track instead. Both are now held by
/// [`AudioEngine::park_session`](AudioEngine::park_session) itself, so a
/// session the user paused stays paused whatever it is playing.
pub(super) fn rebuild_resume(
    state: super::super::state::PlayerState,
    paused_output: bool,
) -> RebuildResume {
    use super::super::state::PlayerState;
    match state {
        PlayerState::Playing | PlayerState::Paused | PlayerState::Loading if paused_output => {
            RebuildResume::StayPaused
        }
        PlayerState::Playing | PlayerState::Paused | PlayerState::Loading => RebuildResume::Play,
        PlayerState::Idle | PlayerState::Ended => RebuildResume::Nothing,
    }
}

impl AudioEngine {
    /// Name of the cpal device feeding the current output thread, or
    /// `None` if it's tracking the OS default. Returned to the
    /// frontend so the device picker can highlight the active row.
    pub fn current_output_device(&self) -> Option<String> {
        self.output
            .lock()
            .ok()
            .and_then(|guard| guard.pinned_device())
    }

    /// The device the output is really driving, when the backend could
    /// name it — as opposed to [`Self::current_output_device`], which is
    /// the user's pin and survives a fallback.
    ///
    /// The picker flags its active row from this one: a pinned device
    /// that is no longer enumerated is replaced by the default endpoint
    /// on open, and ticking the pin then described a device playing
    /// nothing (#612).
    ///
    /// Returned as `(opened, pinned)` under a **single** lock.
    ///
    /// Reading the two names through separate accessors let a rebuild
    /// swap the handle in between, pairing the old stream's opened name
    /// with the new stream's pin — the picker would then tick one row
    /// and mark another as pinned, each describing a different stream.
    pub fn current_output_devices(&self) -> (Option<String>, Option<String>) {
        self.output
            .lock()
            .ok()
            .map(|guard| {
                (
                    guard.handle.as_ref().and_then(|h| h.opened_device.clone()),
                    guard.pinned_device(),
                )
            })
            .unwrap_or((None, None))
    }

    /// Re-open the output on the device it is already pinned to.
    ///
    /// [`Self::set_output_device`] short-circuits when the request
    /// equals the current pin. That is right for a pick, but it left the
    /// user no way back when the stream was bound to an endpoint that is
    /// no longer the one they want — and the picker disabled its active
    /// row, so "choose another device, then choose this one again" was
    /// the only cure (#612). This goes straight to the rebuild path that
    /// device-error recovery already uses.
    ///
    /// Carries the same two protections every other deliberate rebuild
    /// has, because `force_rebuild_output` owns neither: its recovery
    /// caller computes them.
    pub fn reopen_output_device(&self) -> AppResult<()> {
        // Honour the #322 session kill switch. A flap storm that gave up
        // on exclusive for this session must not be undone by a click
        // that only asks for the same device again — this is not the
        // fresh chance a re-toggle or a device pick is, so the
        // suppression also isn't reset here.
        let exclusive = self
            .exclusive_output
            .load(std::sync::atomic::Ordering::Relaxed)
            && !self
                .exclusive_suppressed
                .load(std::sync::atomic::Ordering::Relaxed);
        // Replacing the stream disturbs the outgoing one on purpose, and
        // that self-inflicted device error would otherwise schedule a
        // recovery rebuild fighting this one — the #405 echo. Reopen the
        // window when nothing got installed, so a genuine error still
        // reaches the recovery path.
        self.begin_deliberate_output_change();
        // The device is resolved by the rebuild, under the same lock it
        // swaps the handle with. Reading it here first would leave a
        // window where a device pick installs and persists another one,
        // after which this would reinstall the stale device and leave the
        // stream disagreeing with the saved preference.
        // Deliberate: the user asked for this device, so landing on it
        // is the point and nothing here pauses (#617).
        let result =
            self.force_rebuild_output(RebuildDevice::Pinned, exclusive, RebuildCause::Deliberate);
        if result.is_err() {
            self.cancel_deliberate_output_change();
        }
        result
    }

    /// True when the active output is currently carrying a native DSD
    /// stream via DoP (#495). Reflects what actually engaged — a DAC
    /// that refused the DoP format leaves this `false` even with the
    /// opt-in on. Drives the "DSD natif" pill in the pipeline popover.
    pub fn current_output_is_dop(&self) -> bool {
        self.output
            .lock()
            .ok()
            .and_then(|guard| guard.handle.as_ref().map(|h| h.dop.is_some()))
            .unwrap_or(false)
    }

    /// Recover from a mid-stream `cpal::StreamError::DeviceNotAvailable`
    /// (#175). The cpal error callback fires on a random thread when
    /// Windows resets its audio session, a USB DAC unplugs, or a
    /// Bluetooth source flaps — without an automatic rebuild the user
    /// is stuck on a paused stream until they touch the device menu.
    ///
    /// Rebuilds with the SAME pinned device + WASAPI Exclusive
    /// preference the engine was using before the error. Re-querying
    /// the OS default here would be wrong: the default is what gets
    /// SWAPPED when Windows decides to reset the session, so the user
    /// would silently land on a different output (different sample
    /// rate, different channel count) every time the original device
    /// flapped.
    ///
    /// Debounced via `rebuild_in_progress`: a quick double-flap (the
    /// pattern seen in the original bug report — three open/close
    /// cycles in 14 seconds) only triggers one rebuild attempt
    /// instead of stacking three concurrent SwapProducer cmds onto
    /// the decoder.
    pub(in crate::audio) fn try_rebuild_after_device_error(
        &self,
        target: super::super::output::RebuildTarget,
    ) -> AppResult<()> {
        use std::sync::atomic::Ordering;

        // Release the #365 gate on EVERY exit path below, including the
        // early `return`s and any panic.
        let _gate = RebuildGateGuard(&self.rebuild_gate);

        // Acquire the debounce slot. `swap(true)` returns the
        // previous value, so a `true` here means somebody else is
        // already rebuilding — bail without disturbing them.
        if self.rebuild_in_progress.swap(true, Ordering::AcqRel) {
            tracing::debug!("device-error rebuild already in flight; skipping concurrent trigger");
            return Ok(());
        }

        // RAII guard so the debounce slot clears even if rebuild
        // panics or returns Err mid-flight — otherwise a single
        // failure would lock out every subsequent retry.
        struct ResetGuard<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for ResetGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = ResetGuard(&self.rebuild_in_progress);

        // `Resolve` reads the device off the live (dead) handle still
        // parked in `self.output` — right for the cpal / exclusive
        // device-loss callers. `Device` is passed when the caller already
        // released the handle, so a self-resolve would see `None` and
        // reopen the OS default instead of the user's pick (#405).
        let pinned = match target {
            super::super::output::RebuildTarget::Resolve => self.current_output_device(),
            super::super::output::RebuildTarget::Device(device) => device,
        };
        let pref_exclusive = self.exclusive_output.load(Ordering::Relaxed);

        // #322: an exclusive-mode flap storm. A device that resets on every
        // exclusive grab fires DeviceNotAvailable ~300 ms after each
        // re-engage, so re-opening exclusive here just re-arms the loop.
        // Count flaps and, past the threshold, give up on exclusive for the
        // rest of the session so the device settles on shared mode.
        let mut exclusive = pref_exclusive && !self.exclusive_suppressed.load(Ordering::Relaxed);
        if exclusive {
            let tripped = self
                .exclusive_flaps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(Instant::now());
            if tripped {
                self.exclusive_suppressed.store(true, Ordering::Relaxed);
                exclusive = false;
                tracing::warn!(
                    "Exclusive output disabled for this session after repeated device \
                     flaps; staying on shared mode. Re-enable it in Settings to retry."
                );
            }
        }

        tracing::info!(
            device = pinned.as_deref().unwrap_or("<os-default>"),
            exclusive,
            "rebuilding cpal output after DeviceNotAvailable"
        );

        // Force-rebuild path: bypasses set_output_device's no-op
        // shortcut for "same device" because the device is the
        // same — we just need a fresh stream after the OS reset.
        //
        // The one caller that can land somewhere else than it asked for:
        // when the pinned endpoint is really gone, the backend falls back
        // to the default, and that is the move #617 refuses to follow the
        // music onto.
        self.force_rebuild_output(
            RebuildDevice::Explicit(pinned),
            exclusive,
            RebuildCause::DeviceLost,
        )
    }

    /// Ask permission to schedule a deferred rebuild after a cpal
    /// `DeviceNotAvailable` (#365). Returns `true` for the caller that
    /// should own the recovery, `false` for everyone else.
    ///
    /// Called from the deferred recovery task (after its 300 ms backoff),
    /// not from the cpal error callback itself — the engine may not be
    /// registered in Tauri state yet at error time. A burst of errors
    /// therefore queues several tasks, but only the first to wake arms;
    /// the rest bail here, so the burst still collapses to one rebuild.
    ///
    /// A `false` means either a rebuild is already armed / running, or
    /// we're inside [`REBUILD_SETTLE_WINDOW`] after the last one — on a
    /// DAC that resets on every exclusive grab, that second case is our
    /// own re-open echoing back, and reacting to it is exactly what
    /// re-arms the cascade.
    ///
    /// The matching release lives in `try_rebuild_after_device_error`'s
    /// RAII guard, so a scheduled-but-failed rebuild still reopens the
    /// gate.
    pub fn try_arm_device_rebuild(&self) -> bool {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_arm(Instant::now(), REBUILD_SETTLE_WINDOW)
    }

    /// Move an unpinned output onto the system's new default device
    /// (#627).
    ///
    /// Called (after a backoff) by
    /// [`super::super::output::schedule_default_device_follow`] when the
    /// platform listener in [`super::super::default_device`] reports that the
    /// system default moved. Four questions, in this order:
    ///
    /// 1. **is anything pinned?** A user who picked a device asked for
    ///    that device; following the system would undo the pin. The
    ///    answer here is advisory — a pick landing between this read and
    ///    the rebuild would slip through it — so the rebuild asks again
    ///    under the lock that installs pins, and that answer is the
    ///    binding one. This early read only keeps the common case from
    ///    doing any work.
    /// 2. **would the move change anything?** Interrupting playback to
    ///    reopen the endpoint we are already on is a gap in the music
    ///    for no reason, and a default that disappeared entirely is
    ///    nothing to follow. See [`should_follow_default`].
    /// 3. **exclusive?** The preference, minus the #322 session
    ///    suppression — exactly what [`Self::reopen_output_device`]
    ///    computes. The suppression is honoured but NOT reset: this is
    ///    the system's decision, not the fresh chance a re-toggle or a
    ///    device pick is. No flap is recorded either; a default change
    ///    is not a device resetting under us, and counting it would let
    ///    a few legitimate device switches disable exclusive for the
    ///    session.
    /// 4. **is a rebuild already under way?** Only then is the #365 gate
    ///    armed — and a `false` there is reported as
    ///    [`FollowOutcome::Deferred`] rather than swallowed, because the
    ///    system default and the open device would otherwise stay apart
    ///    for good.
    pub(in crate::audio) fn follow_os_default_output(&self) -> AppResult<FollowOutcome> {
        use std::sync::atomic::Ordering;

        let (opened, pinned) = self.current_output_devices();
        if let Some(pinned) = pinned {
            tracing::debug!(
                device = %pinned,
                "default output device changed, but a device is pinned; staying on it"
            );
            return Ok(FollowOutcome::Settled);
        }

        // Unplugging a device raises both signals at once: the stream
        // breaks and the default moves. They are not the same event — a
        // device that disappears is not a default that changes — and
        // without this, whichever rebuild armed first would decide, so
        // #617's pause would land or not land on a coin toss. The loss
        // recovery owns it; this one stands down rather than deferring,
        // because the recovery ends on the endpoint the system fell back
        // to anyway, which is exactly what this follow would have opened.
        if self.device_loss_in_flight() {
            tracing::debug!(
                "default output device changed while a device loss is being recovered; \
                 leaving it to the recovery"
            );
            return Ok(FollowOutcome::Settled);
        }

        let new_default = super::super::output::os_default_output_name();
        if !should_follow_default(opened.as_deref(), new_default.as_deref()) {
            tracing::debug!(
                opened = opened.as_deref().unwrap_or("<unnamed>"),
                new_default = new_default.as_deref().unwrap_or("<none>"),
                "default output device change needs no rebuild"
            );
            return Ok(FollowOutcome::Settled);
        }

        // Only now is the #365 gate armed, and only now is its release
        // owed: everything above is a cheap read, and arming for a
        // follow that then does nothing would open a settle window a
        // real device loss gets swallowed by — dropped, and never
        // retried, which is silence until the user intervenes. Arming
        // here still collapses a burst, since every notification of the
        // same change reaches this point and only the first one arms.
        if !self.try_arm_device_rebuild() {
            // Not "ignore": a rebuild in flight, or the quiet period after
            // one, would otherwise swallow the change for good and leave
            // the stream on a device the system no longer prefers. The
            // caller comes back once the window is over, and everything
            // above is re-read then, so the retry converges on whatever
            // the default is at that point rather than on this
            // notification's idea of it.
            tracing::debug!(
                "default-device follow: a rebuild is already armed or the settle \
                 window is open; deferring this default change"
            );
            return Ok(FollowOutcome::Deferred);
        }
        let _gate = RebuildGateGuard(&self.rebuild_gate);

        let exclusive = self.exclusive_output.load(Ordering::Relaxed)
            && !self.exclusive_suppressed.load(Ordering::Relaxed);
        tracing::info!(
            from = opened.as_deref().unwrap_or("<unnamed>"),
            to = new_default.as_deref().unwrap_or("<none>"),
            exclusive,
            "following the system's new default output device"
        );
        // Deliberate: the system default moved while the device we were
        // on is still there. Following it is the whole feature (#627),
        // and #617 has nothing to say about it — see the loss check at
        // the top of this method for the case where the two coincide.
        self.force_rebuild_output(
            RebuildDevice::OsDefaultIfUnpinned,
            exclusive,
            RebuildCause::Deliberate,
        )?;
        Ok(FollowOutcome::Settled)
    }

    /// Publish "no output thread at all" when a rebuild bailed out after
    /// the old handle was already released.
    ///
    /// The exclusive flag must not keep claiming a stream that no longer
    /// exists — a toggle describing one is the exact shape of #405 — and
    /// the event is what makes Settings re-read. No-ops when `guard`
    /// still holds a handle, i.e. the failure happened before the old
    /// stream was given up and the flag is still accurate.
    pub(super) fn publish_output_lost_if_gone(&self, guard: &Option<OutputHandle>) {
        if guard.is_none() {
            self.exclusive_output_active
                .store(false, std::sync::atomic::Ordering::Release);
            let _ = self.app.emit("player:audio-mode-changed", ());
        }
    }

    /// Open the #365 settle window around an output change WE are making
    /// on purpose (a mode toggle, a device switch).
    ///
    /// Replacing a stream makes the outgoing one fire
    /// `DeviceNotAvailable` — seizing the endpoint in exclusive mode
    /// kicks the shared client off it, and tearing a stream down can do
    /// the same. That error is a consequence of the change, not a device
    /// failure, but the cpal callback can't tell the difference and
    /// schedules a recovery rebuild that fights the mode the user just
    /// picked (visible in the #405 report as a second exclusive open
    /// ~300 ms after the first). Marking the gate finished starts the
    /// same quiet period a real rebuild would, so those echoes are
    /// ignored.
    pub(super) fn begin_deliberate_output_change(&self) {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(Instant::now());
    }

    /// Undo a [`begin_deliberate_output_change`] when the change ended up
    /// installing no stream (the spawn failed after the old handle was
    /// already released). The settle window was there to absorb the echo
    /// error from a successful swap; with no swap there is no echo, and
    /// leaving the window open would suppress the recovery rebuild we're
    /// about to schedule — the exact silent-no-output tail of #405.
    pub(super) fn cancel_deliberate_output_change(&self) {
        self.rebuild_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reopen();
    }

    /// Clear the #322 session-level exclusive suppression and its flap
    /// counter. Called when the user explicitly re-toggles WASAPI exclusive
    /// or switches output device — both are a fresh chance for exclusive to
    /// work, so a prior flap storm shouldn't keep it pinned to shared.
    pub(super) fn reset_exclusive_suppression(&self) {
        self.exclusive_suppressed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut flaps) = self.exclusive_flaps.lock() {
            *flaps = FlapWindow::default();
        }
    }
}
