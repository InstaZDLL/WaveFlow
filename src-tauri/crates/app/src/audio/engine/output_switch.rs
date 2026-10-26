//! Rebuilding the output on request: a track that needs another rate or
//! DoP, a device pick, and the exclusive-mode toggle.

use crate::error::{AppError, AppResult};

use super::super::output::{spawn_output_with_mode, RequestedFormat};
use super::command::AudioCmd;
use super::notice::PlaybackNotice;
use super::rebuild::{
    endpoint_moved, must_release_before_reopening, rebuild_resume, track_needs_reopen,
    RebuildCause, RebuildDevice, RebuildResume,
};
use super::AudioEngine;

impl AudioEngine {
    /// Reconcile the output format with the track the decoder is about to
    /// play, for native DSD via DoP (#495). Unlike [`Self::force_rebuild_output`]
    /// this hands the fresh ring producer **directly back to the caller**
    /// (the decoder thread, mid-load) instead of pushing it through the
    /// `SwapProducer` channel — the decoder is about to write the new
    /// track's samples to it and there's no old stream to keep feeding.
    ///
    /// - `dop = Some(fmt)`: open the exclusive output at that exact DoP
    ///   rate / channels. If the DAC refuses, transparently fall back to a
    ///   normal PCM output so the caller can play the DSD → PCM path.
    /// - `dop = None`: restore the normal (preference-driven) PCM output
    ///   if the previous track left the device in DoP mode.
    ///
    /// Returns `(producer, dop_engaged)`. `producer` is `Some` only when a
    /// rebuild actually happened — an unchanged format returns `None` so
    /// an ordinary PCM-to-PCM track costs nothing. `dop_engaged` tells the
    /// caller whether to open the stream as DoP or DSD → PCM.
    pub(crate) fn switch_output_for_track(
        &self,
        requested: Option<RequestedFormat>,
    ) -> AppResult<(Option<rtrb::Producer<f32>>, bool)> {
        use std::sync::atomic::Ordering;

        // DoP needs exclusive device access. On Windows that's the
        // separate WASAPI Exclusive opt-in, so DoP rides it — if the user
        // hasn't enabled exclusive, drop the DoP request up-front rather
        // than tear the output down and rebuild it just to fall back to
        // PCM on every DSD track. On Linux / macOS the DoP toggle itself
        // engages the exclusive path (raw `hw:` / hog mode), so nothing
        // else gates it. On any other platform DoP can't run at all.
        #[cfg(target_os = "windows")]
        let exclusive_available = self.exclusive_output.load(Ordering::Relaxed);
        // Linux (raw `hw:`) and macOS (CoreAudio hog mode) engage the
        // exclusive path from the DoP toggle itself — no separate opt-in.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let exclusive_available = true;
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        let exclusive_available = false;
        // The gate is DoP's alone: a rate request (#600) is only ever
        // made when an exclusive stream is already open, and it is a
        // preference rather than a format the platform has to support.
        let requested = match requested {
            Some(r) if r.dop && !exclusive_available => None,
            other => other,
        };
        let dop = requested.and_then(RequestedFormat::as_dop);

        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        let has_output = guard.handle.is_some();
        // Compared as a whole `DopFormat`, not just the rate: an
        // exclusive DoP stream is opened for a fixed interleave, so two
        // consecutive DSD tracks at the same DoP rate but different
        // channel counts still need a re-open (reusing the stereo stream
        // for a multichannel one would tear frames across channels).
        let current_dop = guard.handle.as_ref().and_then(|h| h.dop);
        let want_dop = dop;

        // Already in the right shape: nothing to do. An ordinary PCM track
        // following another PCM track lands here and pays nothing.
        if !track_needs_reopen(
            has_output,
            current_dop,
            want_dop,
            self.last_requested_rate.load(Ordering::Relaxed),
            requested.map(|r| r.sample_rate),
        ) {
            return Ok((None, dop.is_some()));
        }

        // Capture the pinned device + exclusive preference before dropping
        // the old handle. A DoP (exclusive) open can't proceed while the
        // previous exclusive client still holds the device, so release it
        // first (#322 reasoning) — this path always replaces the stream.
        // Resolved from the slot, so a DoP re-open while no stream is
        // installed still targets the user's device rather than the OS
        // default (#630).
        let device = guard.pinned_device();
        let pref_exclusive = self.exclusive_output.load(Ordering::Relaxed);
        if let Some(old) = guard.handle.take() {
            old.stop();
        }

        // Try the requested DoP format first.
        if let Some(dop_fmt) = dop {
            match spawn_output_with_mode(
                self.shared.clone(),
                self.app.clone(),
                device.clone(),
                pref_exclusive,
                Some(RequestedFormat::dop(dop_fmt)),
            ) {
                Ok((producer, handle)) => {
                    guard.handle = Some(handle);
                    self.publish_output_mode(
                        Some(RequestedFormat::dop(dop_fmt)),
                        true,
                        pref_exclusive,
                        guard.handle.as_ref(),
                    );
                    tracing::info!(
                        rate = dop_fmt.sample_rate,
                        channels = dop_fmt.channels,
                        "DoP output engaged"
                    );
                    return Ok((Some(producer), true));
                }
                Err(err) => {
                    tracing::warn!(
                        %err,
                        rate = dop_fmt.sample_rate,
                        "DoP output refused by device; falling back to DSD -> PCM"
                    );
                    // Fall through to a normal PCM open.
                }
            }
        }

        // Normal PCM output: DoP wasn't requested, or was refused. A
        // rate request rides along — the backends treat it as a
        // preference and fall back to a rate the device does offer.
        let pcm_request = requested.filter(|r| !r.dop);
        match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device,
            pref_exclusive,
            pcm_request,
        ) {
            Ok((producer, handle)) => {
                guard.handle = Some(handle);
                // Two different facts, and they must not be conflated.
                // `dop.is_some()` here means a DoP open was tried and
                // refused — the track is DSD, the opt-in is on, and it is
                // about to be converted to PCM without a word (#597), so
                // the notice needs it. The *rate* this open asked for is
                // `pcm_request`, which in that case is none at all:
                // recording the DoP rate instead left every following
                // track disagreeing with the guard and rebuilding the
                // output for nothing.
                self.publish_output_mode(
                    pcm_request,
                    dop.is_some(),
                    pref_exclusive,
                    guard.handle.as_ref(),
                );
                Ok((Some(producer), false))
            }
            Err(err) => {
                // No output at all now — surface the loss like the other
                // rebuild paths so the UI doesn't think playback is live.
                self.publish_output_lost_if_gone(&guard.handle);
                Err(err)
            }
        }
    }

    /// Internal helper: rebuild the output stream against the given
    /// ([`RebuildDevice`], exclusive) pair, bypassing the same-device
    /// no-op check. Shared by [`Self::try_rebuild_after_device_error`],
    /// [`Self::reopen_output_device`] and the default-device follow.
    ///
    /// `cause` decides one thing only: whether ending up on a *different*
    /// endpoint parks playback rather than resuming it (#617).
    pub(super) fn force_rebuild_output(
        &self,
        device: RebuildDevice,
        exclusive: bool,
        cause: RebuildCause,
    ) -> AppResult<()> {
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        // Resolve the target under the very lock this is about to swap
        // the handle with, so nothing can move it in between (#612).
        let device_name = match device {
            RebuildDevice::Pinned => guard.pinned_device(),
            RebuildDevice::Explicit(name) => name,
            // Decided under the very lock a successful pick installs the
            // pin with (#627), which is what makes "only when nothing is
            // pinned" true rather than probable: a device chosen between
            // the notification and here must not be undone by a rebuild
            // aimed at the system default.
            RebuildDevice::OsDefaultIfUnpinned => {
                if let Some(pinned) = guard.pinned_device() {
                    tracing::debug!(
                        device = %pinned,
                        "default-device follow abandoned: a device was pinned meanwhile"
                    );
                    return Ok(());
                }
                None
            }
        };

        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Where the audio actually was, read before the handle goes: it
        // is the only half of "did we move?" that disappears with it
        // (#617).
        let previous_endpoint = guard
            .handle
            .as_ref()
            .and_then(|handle| handle.opened_device.clone());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

        // #322: a WASAPI *exclusive* client locks the device entirely — no
        // other client, exclusive OR shared, can open it until that client
        // is released. So when the OLD stream is exclusive, spawning the new
        // one first fails: a new exclusive open hits AUDCLNT_E_DEVICE_IN_USE
        // (0x8889000A), and even the shared fallback hits "device no longer
        // available" (both lines seen in the #322 report). Release the old
        // exclusive stream FIRST, whatever mode we're rebuilding into. This
        // path only runs after a DeviceNotAvailable error, so the old stream
        // is already dead — there's no working state to roll back to. When
        // the old stream is shared we keep the spawn-first order so a failed
        // spawn can still roll back — except where entering exclusive can't
        // evict it, see [`must_release_before_reopening`].
        let pre_release =
            must_release_before_reopening(guard.handle.as_ref().map(|h| h.exclusive), exclusive);
        if pre_release {
            if was_playing {
                self.stop_for_rebuild()
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
        }

        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device_name,
            exclusive,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // `pre_release` (an old exclusive stream) already took the
                // handle above, so this is the no-output-at-all case.
                self.publish_output_lost_if_gone(&guard.handle);
                return Err(err);
            }
        };

        // The freshly-spawned `handle` owns a live cpal output
        // thread. If either send below fails (decoder dead, channel
        // closed mid-recovery), `handle` would otherwise be dropped
        // without `stop()` being called — the cpal Stream lives on
        // a `!Send` thread that can't be reaped from Drop, so we'd
        // leak the thread until process exit. Same pattern as
        // `set_output_device`'s error rollback.
        let send_result = (|| {
            if was_playing && !pre_release {
                self.stop_for_rebuild()
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // No-op when `pre_release` already took the old handle above.
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }
        guard.handle = Some(handle);
        self.publish_output_mode(None, false, exclusive, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        if resume == RebuildResume::StayPaused {
            // The user had paused it (#611): the rebuild reopens the
            // output and hands the session to Play, it does not start
            // music nobody asked to hear.
            self.park_session(live, track_id);
        }
        if resume == RebuildResume::Play {
            // #617 — the device that went away is not the device we came
            // back on, so the music would be playing somewhere the user
            // did not choose. That is the reported case: headphones off,
            // Windows falls back to the speakers, the album keeps going
            // out loud. A device that flapped and came back is a
            // different matter, and lands here as "not moved".
            let landed_on = guard
                .handle
                .as_ref()
                .and_then(|handle| handle.opened_device.as_deref());
            let moved = endpoint_moved(previous_endpoint.as_deref(), landed_on);
            if cause == RebuildCause::DeviceLost && moved && self.pause_on_device_loss() {
                tracing::info!(
                    from = previous_endpoint.as_deref().unwrap_or("<unnamed>"),
                    to = landed_on.unwrap_or("<unnamed>"),
                    "the output device went away and the fallback is another one; parking playback"
                );
                // Playback stopping on its own needs a reason on screen,
                // not just in the log (#597). Not deduplicated: this one
                // is an event, not a state, and it answers a question the
                // user is asking right now.
                self.emit_playback_notice(PlaybackNotice::PausedDeviceLost);
                self.park_session(live, track_id);
            } else {
                self.resume_after_rebuild(live);
            }
        }

        Ok(())
    }

    /// `device_name = None` means "follow the OS default". Picking the
    /// already-active device is a no-op so spamming the menu doesn't
    /// glitch playback.
    ///
    /// **Threading note:** opening / tearing down a cpal stream on
    /// Linux ALSA can probe device hardware and block for hundreds of
    /// ms. Callers reaching into this from a tokio task should wrap
    /// the call in `tokio::task::spawn_blocking`.
    pub fn set_output_device(&self, device_name: Option<String>) -> AppResult<()> {
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;

        // Same device? Nothing to do. Compare both sides as `Option<&str>`
        // so an empty-string DB read can't masquerade as a real change.
        //
        // Two things about this comparison are deliberate. It reads the
        // handle rather than `pinned_device()`, so that with no stream
        // installed — an open that failed — picking the pinned device again
        // still reaches the rebuild instead of early-returning on the pin
        // (#612, #630). And it only short-circuits **while a handle
        // exists**: without one, `current` is `None`, so a user asking for
        // the OS default matched `None == None` and got a no-op in exactly
        // the state they were trying to recover from. Spamming the menu is
        // only worth a no-op when there is a stream to leave alone.
        let current = guard.handle.as_ref().and_then(|h| h.device_name.as_deref());
        let requested = device_name.as_deref();
        if guard.handle.is_some() && current == requested {
            return Ok(());
        }
        // Kept before the spawn below consumes `device_name`: this is the
        // pin to record once the pick succeeds (#630).
        let pinned_pick = device_name.clone();

        // A different device may support exclusive fine — clear any #322
        // flap-storm suppression tied to the previous device.
        self.reset_exclusive_suppression();

        // Snapshot what's playing so we can resume on the new device.
        // What this rebuild owes the session, by the same rule the
        // device-error path uses (#611): a session the user had paused
        // is parked, not started. Picking a device or flipping the mode
        // is a deliberate act on the *output*, never a request to play —
        // and this path resumed a paused track into audible playback.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

        // Step 2 — release the old stream first when the new one cannot be
        // opened alongside it, then open the replacement.
        //
        // Spawn-first is the order we want for a device *switch*: the two
        // streams target different endpoints, so a failed open costs
        // nothing and the stream the user is listening to survives. PipeWire,
        // PulseAudio and ALSA dmix all take concurrent streams, and Windows
        // has no quarrel with two clients on two endpoints.
        //
        // A vanished device breaks that premise, and #604 is what it looks
        // like. When the name we were asked for is no longer enumerated,
        // `pick_device` falls back to the **default** endpoint — which can
        // be the one the old stream is holding exclusively. The open is then
        // refused with `AUDCLNT_E_DEVICE_IN_USE` against ourselves, we drop
        // to shared mode, and nothing tells the user why.
        //
        // Measured on Windows 11 rather than assumed: an exclusive client
        // does block a second exclusive open of the same endpoint, a
        // *shared* client does not block one at all, and the block clears
        // about 19 ms after the client is dropped. So the conflict is real,
        // it is ours, and releasing first is enough to clear it.
        let entering_exclusive = self
            .exclusive_output
            .load(std::sync::atomic::Ordering::Relaxed);
        let pre_release = must_release_before_reopening(
            guard.handle.as_ref().map(|h| h.exclusive),
            entering_exclusive,
        );
        // Stop is sent here rather than twice: the step below skips its own
        // send when this one already happened.
        let stopped_early = pre_release && was_playing;
        // The device to put back if the replacement will not open at all.
        // Only a pre-release needs one: spawn-first leaves the old stream
        // installed when the new one fails.
        let mut previous_device = None;
        if pre_release {
            if was_playing {
                self.stop_for_rebuild()
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            if let Some(old) = guard.handle.take() {
                previous_device = Some(old.device_name.clone());
                old.stop();
            }
        }

        // Set when the replacement failed and the previous device came back
        // instead: playback carries on there, and the caller still gets the
        // error, so the new choice is not persisted.
        let mut switch_error = None;
        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            device_name,
            entering_exclusive,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // Releasing first gave up the rollback that spawn-first gets
                // for free, so buy it back by reopening the previous device.
                // A target that will not open at all, such as a headset still
                // listed but already gone, must not cost the user the output
                // they were listening to.
                //
                // Keyed on the release rather than on comparing endpoints: a
                // name is no identity (ALSA reaches one card as `default`,
                // `plughw:0,0` and `hw:0,0`), and a wrong "different" verdict
                // would put #604 back.
                let reopened = previous_device.clone().and_then(|previous| {
                    spawn_output_with_mode(
                        self.shared.clone(),
                        self.app.clone(),
                        previous,
                        entering_exclusive,
                        None,
                    )
                    .inspect_err(|reopen_err| {
                        tracing::warn!(
                            %reopen_err,
                            "set_output_device: the previous device did not reopen either"
                        );
                    })
                    .ok()
                });
                match reopened {
                    Some(pair) => {
                        tracing::warn!(
                            %err,
                            "set_output_device: the new device did not open, back on the previous one"
                        );
                        switch_error = Some(err);
                        pair
                    }
                    None => {
                        // No output at all now, and the toggle must stop
                        // claiming one (#405). Without a pre-release the old
                        // stream is still installed and still playing, and
                        // this no-ops.
                        self.publish_output_lost_if_gone(&guard.handle);
                        // Same recovery as `set_exclusive_output`'s: retry the
                        // previous device once the OS has settled, instead of
                        // leaving the engine with no output until the user
                        // picks again. Passed explicitly because the release
                        // emptied `self.output`, so a self-resolve would
                        // reopen the OS default instead of that device (#405).
                        if let Some(previous) = previous_device {
                            super::super::output::schedule_device_rebuild(
                                &self.app,
                                super::super::output::RebuildTarget::Device(previous),
                            );
                        }
                        return Err(err);
                    }
                }
            }
        };

        // Step 3 — interrupt any current playback. The decoder will
        // walk back out of `play_track` and start polling for fresh
        // commands at the top level. The crossbeam channel is FIFO,
        // so the SwapProducer we send next won't be picked up before
        // Stop is processed.
        //
        // If either Stop or SwapProducer fails to send, the decoder
        // has died (engine teardown / crash). Tear the freshly opened
        // output back down so it doesn't outlive the engine.
        let send_result = (|| {
            if was_playing && !stopped_early {
                self.stop_for_rebuild()
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // Step 4 — drop the old output thread (releases the
            // device). Done before SwapProducer so the decoder
            // doesn't briefly hold two ring producers; doing this
            // here also keeps the failure path tidy.
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
            // Step 5 — hand the fresh producer over to the decoder.
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            // Mirror force_rebuild_output / set_exclusive_output: if the
            // SwapProducer send failed the closure had already run
            // `guard.take()`, so no output thread remains — the exclusive
            // flag must stop claiming one and Settings must re-read (#405).
            // No-ops when an earlier Stop send failed before guard.take(),
            // where the old stream is still installed and the flag accurate.
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }

        guard.handle = Some(handle);
        // The pick becomes the pin, written under the same lock as the
        // handle it belongs to (#630). This is what lets the engine still
        // name the user's device after an open that fails and leaves no
        // handle behind: the picker keeps flagging its pinned row, a forced
        // reopen targets that device instead of the OS default, and MPD
        // stops answering "no device".
        //
        // Only on a switch that actually happened. `switch_error` is set
        // when the requested device refused to open and the previous one
        // was reopened in its place: the stream, and the persisted setting
        // the command leaves alone on `Err`, both still say the old device,
        // so recording the refused one would make `pinned_device()` name a
        // device nothing is using and send the next forced reopen straight
        // back at it.
        if switch_error.is_none() {
            guard.pinned = pinned_pick;
        }
        self.publish_output_mode(None, false, entering_exclusive, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Step 6 — put back whatever the decoder is on. Not necessarily
        // the track this method snapshotted: a pick made during the open
        // is what the decoder accepted, and resuming the older snapshot
        // instead is what left nothing playing at all (#634). A session
        // the user had paused is parked instead, so a device pick never
        // starts music (#611).
        match resume {
            RebuildResume::Play => self.resume_after_rebuild(live),
            RebuildResume::StayPaused => self.park_session(live, track_id),
            RebuildResume::Nothing => {}
        }

        // Back on the previous device after a failed switch: playback is
        // restored, but the switch itself did not happen.
        match switch_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Flip the WASAPI Exclusive Mode preference and re-open the
    /// output stream using the new mode. No-ops on non-Windows.
    /// Re-uses the active device name so the user keeps their pick.
    pub fn set_exclusive_output(&self, enabled: bool) -> AppResult<()> {
        let previous = self
            .exclusive_output
            .swap(enabled, std::sync::atomic::Ordering::Relaxed);
        if previous == enabled {
            return Ok(());
        }
        // Reuse `set_output_device` with the active device name — the
        // current/requested equality check inside it would short-circuit
        // a same-device call, so go straight to the rebuild path by
        // temporarily yielding `None` would change the device picker
        // semantics. Instead, the engine's existing teardown path is
        // what we need: snapshot the device, drop the handle, rebuild.
        let mut guard = self
            .output
            .lock()
            .map_err(|_| AppError::Audio("output mutex poisoned".into()))?;
        // Resolved under the very lock this method rebuilds with (#629).
        // Reading it before taking the lock left a window in which
        // `set_output_device` could install *and persist* device B; this
        // toggle then rebuilt on A, the name it had captured, and the
        // stream ended up contradicting the saved preference until
        // something else rebuilt. Same cure as `reopen_output_device`
        // took in #628: let the rebuild resolve its own target.
        let active = guard.pinned_device();
        // What this rebuild owes the session, by the same rule the
        // device-error path uses (#611): a session the user had paused
        // is parked, not started. Picking a device or flipping the mode
        // is a deliberate act on the *output*, never a request to play —
        // and this path resumed a paused track into audible playback.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // Both a session that was playing and one the user paused hold a
        // track the rebuild has to interrupt; only what follows differs.
        let was_playing = resume != RebuildResume::Nothing;
        let track_id = self
            .shared
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        // Read here, before the stop: opening the replacement writes its
        // own sample rate into the shared block, and a position derived
        // from the old rate's sample count against the new one is simply a
        // wrong number. What it is the position *of* is stamped onto it,
        // because the load to resume can only be read after the swap
        // (#634) — an exclusive open costs tens to hundreds of
        // milliseconds, and a track the user picks during it is what the
        // decoder will be holding by then.
        let live = self.live_position();

        // #405 — the stuck Settings toggle. Leaving exclusive mode re-opens
        // the SAME endpoint in shared mode, and a WASAPI exclusive client
        // owns its endpoint outright: no other client, shared OR exclusive,
        // can open it until that client is released (the #322 lesson, which
        // `force_rebuild_output` already applies). Spawning first — the
        // order that's correct for a device *switch*, where the two streams
        // target different endpoints — therefore fails every time here. The
        // command returned `Err`, so the preference was never persisted and
        // `exclusive_output_active` kept reporting the old mode: the toggle
        // sat latched on the very mode the user was trying to leave, with a
        // restart as the only way out. Release the old exclusive stream
        // FIRST so the new open finds a free device — and on macOS the
        // other direction needs it too, see
        // [`must_release_before_reopening`].
        let pre_release =
            must_release_before_reopening(guard.handle.as_ref().map(|h| h.exclusive), enabled);
        if pre_release {
            if was_playing {
                if let Err(e) = self.stop_for_rebuild() {
                    // Nothing has been torn down yet — the old exclusive
                    // stream is still installed and running. Roll the
                    // preference swap back so the in-memory pref keeps
                    // matching the live stream; without this a later
                    // device-error rebuild would read the new pref and
                    // silently flip to the mode this toggle failed to
                    // apply. (The spawn / send_result failure paths below
                    // deliberately keep the new pref instead, because by
                    // then the old stream is already gone.)
                    self.exclusive_output
                        .store(previous, std::sync::atomic::Ordering::Relaxed);
                    return Err(AppError::Audio(format!(
                        "audio command channel closed: {e}"
                    )));
                }
            }
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
        }

        // An explicit user toggle clears any #322 flap-storm suppression so
        // re-enabling exclusive actually retries it (and disabling resets
        // the counter for next time). Deferred until after the pre-release
        // teardown so a failed Stop send above — which leaves the old stream
        // intact and rolls the preference back — doesn't also wipe the
        // suppression state that still describes that surviving stream.
        self.reset_exclusive_suppression();

        // Both directions of this toggle disturb the outgoing stream on
        // purpose — don't let the resulting device error trigger a
        // recovery rebuild that undoes the switch.
        self.begin_deliberate_output_change();

        let (producer, handle) = match spawn_output_with_mode(
            self.shared.clone(),
            self.app.clone(),
            active.clone(),
            enabled,
            None,
        ) {
            Ok(pair) => pair,
            Err(err) => {
                // No replacement stream was installed, so the settle
                // window we opened has no swap-echo to absorb. Reopen the
                // gate either way, so it can't suppress a genuine device
                // error — on the surviving old stream (shared → exclusive
                // where even the shared fallback failed, `guard` still
                // holds it) or on the recovery we schedule below.
                self.cancel_deliberate_output_change();
                if guard.handle.is_none() {
                    // `pre_release` already released the old exclusive
                    // handle, so there is no output thread at all. Tell
                    // Settings the flag is stale (#405), then schedule a
                    // rebuild that re-opens in the mode now recorded in
                    // `exclusive_output`. Pass `active` explicitly: the
                    // teardown emptied `self.output`, so a self-resolve
                    // would reopen the OS default instead of the user's
                    // device.
                    self.publish_output_lost_if_gone(&guard.handle);
                    super::super::output::schedule_device_rebuild(
                        &self.app,
                        super::super::output::RebuildTarget::Device(active),
                    );
                } else {
                    // shared → exclusive where even the shared fallback
                    // failed: the old shared stream is untouched and still
                    // running. Roll the preference back to match it — same
                    // as the failed-Stop path — otherwise a later
                    // device-error rebuild would read the new pref and flip
                    // to the exclusive mode this toggle never applied.
                    self.exclusive_output
                        .store(previous, std::sync::atomic::Ordering::Relaxed);
                }
                return Err(err);
            }
        };

        // Group the whole hand-off so ANY failing step still runs the
        // `handle.stop()` below. `handle` owns a live output thread on a
        // `!Send` thread that can't be reaped from Drop, so an early
        // return here would leak it until process exit — and when the new
        // stream is the exclusive one, that leaked thread keeps the device
        // locked while `guard` still points at the old stream. Same shape
        // as `force_rebuild_output`.
        let send_result = (|| {
            if was_playing && !pre_release {
                self.stop_for_rebuild()
                    .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            }
            // No-op when `pre_release` already took the old handle above.
            if let Some(old) = guard.handle.take() {
                old.stop();
            }
            self.cmd_tx
                .send(AudioCmd::SwapProducer(producer))
                .map_err(|e| AppError::Audio(format!("audio command channel closed: {e}")))?;
            Ok::<(), AppError>(())
        })();
        if let Err(err) = send_result {
            handle.stop();
            // A failing `Stop` send bails before the old handle is taken,
            // so `guard` still describes a live stream and the flag stays
            // accurate; the later steps leave no output at all. The helper
            // tells those two apart and only publishes for the second.
            self.publish_output_lost_if_gone(&guard.handle);
            return Err(err);
        }
        guard.handle = Some(handle);
        // The event is redundant with the caller's own re-read after a
        // manual toggle (ExclusiveModeCard.tsx), and kept for every other
        // caller. The notice is not redundant: a toggle that lands in
        // shared mode is precisely the silent degradation #597 is about.
        self.publish_output_mode(None, false, enabled, guard.handle.as_ref());

        // Re-read the decision now rather than trust the one taken
        // before the open. An exclusive open costs tens to hundreds of
        // milliseconds, and a Pause the user hits during it reaches the
        // decoder first: resuming on the strength of a stale `Play`
        // would start a track they had just stopped. The `Stop` above
        // does not clear either input — it unloads the track without
        // touching the state or `paused_output` — so this reads what the
        // user last asked for.
        let resume = rebuild_resume(self.shared.state(), self.paused_for_rebuild());
        // The mode flip must not drop the user off what they are
        // listening to — radio included, which is why this re-dispatches
        // the decoder's own load rather than looking a `track` row up by
        // an id a stream does not have. And it must not start what they
        // had paused either: that one is parked (#611).
        match resume {
            RebuildResume::Play => self.resume_after_rebuild(live),
            RebuildResume::StayPaused => self.park_session(live, track_id),
            RebuildResume::Nothing => {}
        }

        Ok(())
    }

    /// Whether the current output stream really owns its device —
    /// `false` after a fallback to cpal shared mode, and on any
    /// platform with no exclusive PCM backend.
    pub fn exclusive_output(&self) -> bool {
        self.exclusive_output_active
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether the user asked for exclusive output — the persisted
    /// preference, whatever the current stream managed to engage.
    pub fn exclusive_output_requested(&self) -> bool {
        self.exclusive_output
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
