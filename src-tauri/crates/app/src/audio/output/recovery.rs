//! What happens when the device under the stream goes away or the system
//! default moves: the loss report, and the deferred rebuild or follow.

use std::sync::Arc;

use cpal::traits::HostTrait;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use super::super::state::{PlayerState, SharedPlayback};
#[cfg(not(target_os = "linux"))]
use super::devices::device_display_name;
#[cfg(target_os = "linux")]
use super::{devices::device_key, silence_alsa_stderr};

/// Surface a lost audio device to the user: park the player, tell the
/// UI, and keep the OS media controls in sync.
///
/// Shared by the cpal error callback and the WASAPI-exclusive event
/// loop ([`super::super::wasapi_exclusive`]) so both backends report a device
/// loss identically — the exclusive thread used to just exit, leaving
/// the UI convinced playback was still running.
pub(in crate::audio) fn notify_device_lost(
    app: &AppHandle,
    shared: &Arc<SharedPlayback>,
    message: String,
) {
    // Stamped here, at the error itself, rather than in the recovery that
    // follows 300 ms later: the same unplug also moves the system default,
    // and the follow that reacts to that (#627) has to know this is a
    // device going away, not a preference changing (#617). The engine can
    // be missing while `AudioEngine::new` is still running, which is the
    // one window where no recovery is scheduled either.
    if let Some(engine) = app.try_state::<std::sync::Arc<super::super::AudioEngine>>() {
        engine.note_device_loss();
    }
    shared.set_state(PlayerState::Paused);
    let _ = app.emit(
        "player:state",
        json!({ "state": "paused", "track_id": null }),
    );
    // The message is technical on purpose — it is what a bug report
    // needs — and `kind` is what the UI turns into a sentence the user
    // can act on (#597).
    let _ = app.emit(
        "player:error",
        json!({ "message": message, "kind": "device-lost" }),
    );
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_playback(PlayerState::Paused, shared.current_position_ms());
    }
}

/// Which device a scheduled rebuild should reopen.
///
/// The two callers differ in whether the engine can still resolve the
/// device from its live handle at rebuild time:
/// - the cpal error callback and the WASAPI-exclusive `DeviceLost` exit
///   both fire while the (now-dead) handle is still parked in
///   `self.output`, so its `device_name` is still readable → [`Resolve`];
/// - the `set_exclusive_output` failure path has already `take()`n the
///   old handle, emptying `self.output`, so a self-resolve would return
///   `None` and reopen the OS default instead of the user's pick (#405)
///   → [`Device`] carries the device captured before the teardown.
///
/// [`Resolve`]: RebuildTarget::Resolve
/// [`Device`]: RebuildTarget::Device
pub(in crate::audio) enum RebuildTarget {
    /// Resolve the device from the engine's live handle at rebuild time.
    Resolve,
    /// Reopen this specific device (`None` = OS default).
    Device(Option<String>),
}

/// Schedule a same-device rebuild of the output stream after a device
/// loss (#175). Returns immediately — the work happens on a tokio task.
///
/// 300 ms backoff first (covers the OS settling a USB replug / driver
/// restart / Windows session reset), then a SAME-DEVICE rebuild through
/// the engine. Re-querying the OS default here would land the user on a
/// different output every time their pinned device flapped.
///
/// The engine is resolved AFTER the delay on purpose — an error can fire
/// while `AudioEngine::new` is still running, i.e. before the engine has
/// been registered in Tauri's managed state, and looking it up eagerly
/// would silently drop the recovery for that window.
pub(in crate::audio) fn schedule_device_rebuild(app: &AppHandle, target: RebuildTarget) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let Some(engine) = app.try_state::<std::sync::Arc<super::super::AudioEngine>>() else {
            return;
        };
        // #365: one rebuild per burst of device errors. A DAC that
        // resets on every exclusive grab makes our own re-open
        // trigger the next error, so without a gate the passes
        // chase each other until one opens exclusive while the
        // previous stream is still settling and collides with
        // AUDCLNT_E_DEVICE_IN_USE. Gating here (rather than before
        // the sleep) still collapses the burst: whichever task
        // wakes first arms, and the others hit either `armed` or
        // the settle window.
        if !engine.try_arm_device_rebuild() {
            tracing::debug!(
                "device rebuild already armed or within the settle window; \
                 ignoring this device loss"
            );
            return;
        }
        // The rebuild is synchronous and genuinely slow: it joins
        // the old output thread — which, in WASAPI exclusive mode,
        // can sit in `wait_for_event` up to its 2 s timeout — and
        // then opens the device inline (COM init + the #174 format
        // negotiation ladder). Running that directly on a tokio
        // worker would park the worker for the whole duration, so
        // hand it to the blocking pool.
        let engine = engine.inner().clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(err) = engine.try_rebuild_after_device_error(target) {
                tracing::warn!(%err, "auto-rebuild after device loss failed");
            }
        })
        .await;
    });
}

/// Name of the OS default output device, as the pickers and the open
/// paths see it.
///
/// Same accessor the device listing flags its default row with, so a
/// name from here is comparable with `OutputHandle::opened_device`
/// (#627) — the shared cpal path fills that field from
/// [`device_display_name`] too. `None` means the host reports no
/// default output at all.
pub(in crate::audio) fn os_default_output_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // Wrapped for the same reason the listing wraps it: opening the
        // `default` alias can make ALSA chatter on stderr.
        silence_alsa_stderr(|| {
            cpal::default_host()
                .default_output_device()
                .and_then(|d| device_key(&d))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| device_display_name(&d))
    }
}

/// How many times a deferred default-device follow comes back before
/// giving up. Three attempts spread over ~6 s outlast one rebuild and
/// its settle window; past that, a gate this busy means device errors
/// are storming, and the recovery — which reopens the OS default when
/// nothing is pinned — is the better judge of where to land.
const FOLLOW_DEFAULT_ATTEMPTS: usize = 3;

/// Added to the settle window before a retry, so the retry wakes just
/// *after* it rather than racing its boundary.
const FOLLOW_RETRY_MARGIN: std::time::Duration = std::time::Duration::from_millis(100);

/// Schedule a move onto the system's new default output (#627).
/// Returns immediately — the work happens on a tokio task.
///
/// Called from the platform listeners in
/// [`super::super::default_device`], which run on an OS thread and must not
/// block. Everything that decides *whether* to move lives in
/// [`super::super::AudioEngine::follow_os_default_output`]; this function is
/// only the hop onto our own runtime, plus the two delays that make the
/// storm survivable:
///
/// - the same 300 ms backoff the device-loss recovery takes, because a
///   default change arrives while the OS is still settling — Windows
///   fires the notification per role, and a freshly connected endpoint
///   is not necessarily openable the instant it becomes the default;
/// - the #365 rebuild gate, which collapses the burst to one rebuild
///   and then holds a settle window over the device error our own
///   reopen provokes on the outgoing stream.
///
/// A gate that is busy defers rather than drops: the follow is retried
/// after the settle window, a bounded number of times. Without that, a
/// default moved twice inside two seconds — or moved while the recovery
/// was rebuilding — would leave the stream on the intermediate device
/// for good, because each change is one notification and not a
/// repeating signal. Each retry re-reads the pin and the default, so it
/// converges on the current state rather than replaying a stale one.
pub(in crate::audio) fn schedule_default_device_follow(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let Some(engine) = app.try_state::<std::sync::Arc<super::super::AudioEngine>>() else {
            // A notification during boot, before the engine reached
            // Tauri state. Nothing to move: the open that follows will
            // read the new default anyway.
            return;
        };
        // The #365 gate is NOT armed here. `follow_os_default_output`
        // arms it itself, once its cheap checks say a rebuild is really
        // going to happen: arming for a follow that turns out to be a
        // no-op — a pinned device, a default that is already the endpoint
        // we are on — would open a settle window for nothing, and a real
        // `DeviceNotAvailable` landing inside it is dropped and never
        // retried, leaving the user with no output at all.
        //
        // Synchronous and genuinely slow — it joins the old output
        // thread and opens the device inline — so it goes to the
        // blocking pool rather than parking a tokio worker.
        let engine = engine.inner().clone();
        for attempt in 1..=FOLLOW_DEFAULT_ATTEMPTS {
            let engine = engine.clone();
            let joined =
                tokio::task::spawn_blocking(move || engine.follow_os_default_output()).await;
            let outcome = match joined {
                Ok(outcome) => outcome,
                Err(err) => {
                    // The blocking task panicked or was cancelled. Say so
                    // rather than reporting a follow that never ran as a
                    // settled one, and stop: a retry would most likely
                    // panic in the same place.
                    tracing::warn!(%err, "default-device follow task did not finish");
                    return;
                }
            };
            match outcome {
                Ok(super::super::engine::FollowOutcome::Settled) => return,
                Ok(super::super::engine::FollowOutcome::Deferred) => {
                    if attempt == FOLLOW_DEFAULT_ATTEMPTS {
                        tracing::warn!(
                            attempts = FOLLOW_DEFAULT_ATTEMPTS,
                            "gave up following the new default output device: the rebuild \
                             gate stayed busy"
                        );
                        return;
                    }
                    // Wake just past the settle window the busy rebuild
                    // opened, so the retry finds the gate free.
                    tokio::time::sleep(
                        super::super::engine::REBUILD_SETTLE_WINDOW + FOLLOW_RETRY_MARGIN,
                    )
                    .await;
                }
                Err(err) => {
                    tracing::warn!(%err, "following the new default output device failed");
                    return;
                }
            }
        }
    });
}
