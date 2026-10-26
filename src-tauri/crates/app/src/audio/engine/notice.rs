//! What the output really is (#597) and what the user is told when it is
//! not what they asked for.

use tauri::Emitter;

use crate::error::AppError;

use super::super::output::{OutputHandle, RequestedFormat};
use super::AudioEngine;

/// What the output really is right now (#597).
///
/// Computed by the engine rather than by the UI, so the badge in the
/// player, the notice the user gets when something silently degrades and
/// the Settings card cannot drift apart — they all read the same rule.
/// Serialised in kebab-case, the shape the frontend switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    /// The system mixer is in the path, which is the normal case.
    Shared,
    /// The stream owns the device: WASAPI Exclusive, a raw ALSA `hw:`,
    /// CoreAudio hog mode.
    Exclusive,
    /// Native DSD is reaching the DAC over DoP (#495). It implies
    /// exclusive, and says more, so it is reported instead.
    Dop,
    /// Exclusive was asked for and the device would not give it. Nothing
    /// failed — playback is fine, in shared mode — but the user chose
    /// otherwise and nothing told them (#597).
    ExclusiveRefused,
}

/// The rule behind [`AudioEngine::output_mode`], pure so it can be
/// tested without a sound card.
///
/// DoP first: on Linux and macOS the DoP toggle engages the exclusive
/// path by itself, so `requested_exclusive` can be false while the DAC
/// is being handed native DSD.
pub(super) fn output_mode_of(
    requested_exclusive: bool,
    engaged_exclusive: bool,
    engaged_dop: bool,
) -> OutputMode {
    if engaged_dop {
        OutputMode::Dop
    } else if engaged_exclusive {
        OutputMode::Exclusive
    } else if requested_exclusive {
        OutputMode::ExclusiveRefused
    } else {
        OutputMode::Shared
    }
}

/// Why an exclusive output was not granted, as far as the backend could
/// tell. Stored on [`SharedPlayback::exclusive_refusal`](super::super::state::SharedPlayback)
/// by the output layer when it falls back to shared mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusiveRefusal {
    /// Another application holds the device (WASAPI `AUDCLNT_E_DEVICE_IN_USE`,
    /// ALSA `EBUSY` that no reservation released, CoreAudio hog mode taken).
    Busy,
    /// The system forbids exclusive use of this device (Windows: "Allow
    /// applications to take exclusive control" is off,
    /// `AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED`).
    NotAllowed,
    /// The device offers no format the stream could use.
    Format,
    /// Something else, or a backend that could not say.
    Unknown,
}

impl ExclusiveRefusal {
    /// `0` is reserved for "nothing was refused".
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Busy => 1,
            Self::NotAllowed => 2,
            Self::Format => 3,
            Self::Unknown => 4,
        }
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Busy),
            2 => Some(Self::NotAllowed),
            3 => Some(Self::Format),
            4 => Some(Self::Unknown),
            _ => None,
        }
    }

    /// `err` as a refusal for this reason. The detail keeps the backend's
    /// own message, without the `audio error:` prefix the variant adds back.
    pub fn refuse(self, err: AppError) -> AppError {
        let detail = match err {
            AppError::Audio(text) => text,
            AppError::ExclusiveRefused { detail, .. } => detail,
            other => other.to_string(),
        };
        AppError::ExclusiveRefused {
            reason: self,
            detail,
        }
    }

    /// Whether `self` says more than `other` about what to do: another
    /// app holding the device first, then a system policy, then a format,
    /// then nothing.
    pub fn outranks(self, other: Self) -> bool {
        fn rank(reason: ExclusiveRefusal) -> u8 {
            match reason {
                ExclusiveRefusal::Busy => 3,
                ExclusiveRefusal::NotAllowed => 2,
                ExclusiveRefusal::Format => 1,
                ExclusiveRefusal::Unknown => 0,
            }
        }
        rank(self) > rank(other)
    }

    /// The reason a backend put on its error, or `Unknown`.
    pub fn of(err: &AppError) -> Self {
        match err {
            AppError::ExclusiveRefused { reason, .. } => *reason,
            _ => Self::Unknown,
        }
    }

    fn notice(self) -> PlaybackNotice {
        match self {
            Self::Busy => PlaybackNotice::ExclusiveRefusedBusy,
            Self::NotAllowed => PlaybackNotice::ExclusiveRefusedNotAllowed,
            Self::Format => PlaybackNotice::ExclusiveRefusedFormat,
            Self::Unknown => PlaybackNotice::ExclusiveRefused,
        }
    }
}

/// Something the user should know about playback that is **not** a
/// failure (#597).
///
/// Deliberately a different register from `player:error`: losing the
/// device is a fault, falling back to shared mode is normal operation
/// that happens to contradict a choice the user made. Announced once per
/// transition — a DAC that refuses exclusive refuses it at every track,
/// and saying so every time is nagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackNotice {
    /// Exclusive was requested, the device opened shared, for no reason
    /// the backend could name.
    ExclusiveRefused,
    /// … because another application holds the device.
    ExclusiveRefusedBusy,
    /// … because the system forbids exclusive use of the device.
    ExclusiveRefusedNotAllowed,
    /// … because the device offers no usable format.
    ExclusiveRefusedFormat,
    /// A DSD track was offered over DoP and the DAC would not take it;
    /// it is being converted to PCM instead.
    DopRefused,
    /// The output device went away and playback was parked rather than
    /// moved onto whatever the system fell back to (#617).
    PausedDeviceLost,
}

impl AudioEngine {
    /// What the output really is right now (#597) — what the badge in
    /// the player shows, and what the Settings card means by "engaged".
    ///
    /// One acquisition for both halves of the answer: read separately, a
    /// rebuild landing in between could pair the old stream's exclusive
    /// flag with the new stream's DoP one.
    pub fn output_mode(&self) -> OutputMode {
        let (engaged_exclusive, engaged_dop) = self
            .output
            .lock()
            .ok()
            .and_then(|guard| {
                guard
                    .handle
                    .as_ref()
                    .map(|handle| (handle.exclusive, handle.dop.is_some()))
            })
            .unwrap_or((false, false));
        output_mode_of(
            self.exclusive_output
                .load(std::sync::atomic::Ordering::Relaxed),
            engaged_exclusive,
            engaged_dop,
        )
    }

    /// Record what the output actually opened as, and tell the user when
    /// that is not what they asked for (#405, #597).
    ///
    /// The single place `exclusive_output_active` is written on a
    /// successful open, because three things have to stay in step: the
    /// flag the Settings card reads, the `player:audio-mode-changed`
    /// event that makes it re-read — a rebuild can flip the engaged mode
    /// behind its back — and the notice that says so in words.
    ///
    /// `handle` is `None` only when a caller has already given the stream
    /// up, which the loss path reports separately.
    pub(super) fn publish_output_mode(
        &self,
        requested: Option<RequestedFormat>,
        dop_requested: bool,
        requested_exclusive: bool,
        handle: Option<&OutputHandle>,
    ) {
        let engaged_exclusive = handle.is_some_and(|handle| handle.exclusive);
        let engaged_dop = handle.is_some_and(|handle| handle.dop.is_some());
        // What **this** open asked for, which is what the next track
        // compares against (#600) — see `last_requested_rate`. Recorded
        // here rather than at each call site so an open that asks for
        // nothing in particular, like a device switch, clears it: leaving
        // a stale rate there would make the next track think its rate was
        // already installed.
        //
        // Deliberately not `dop_requested`, which is a separate argument
        // for exactly this reason: the PCM open that follows a *refused*
        // DoP asked for no rate at all. Recording the DoP rate there left
        // every following track disagreeing with it, and rebuilding the
        // output — an audible gap per track — to install nothing new.
        self.last_requested_rate.store(
            requested.map_or(0, |r| r.sample_rate),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.exclusive_output_active
            .store(engaged_exclusive, std::sync::atomic::Ordering::Release);
        // Settings' exclusive-mode toggle only re-reads its state on
        // mount and after a manual click (issue #405) — this is the one
        // signal that tells it a rebuild just happened behind its back,
        // whether that landed in exclusive or fell back to shared.
        let _ = self.app.emit("player:audio-mode-changed", ());

        // What to say about it, in the order of what the user loses
        // most: a DSD stream converted to PCM, then an exclusive path
        // they asked for and did not get.
        let notice = if dop_requested && !engaged_dop {
            Some(PlaybackNotice::DopRefused)
        } else if requested_exclusive && !engaged_exclusive && handle.is_some() {
            Some(
                self.exclusive_refusal()
                    .unwrap_or(ExclusiveRefusal::Unknown)
                    .notice(),
            )
        } else {
            None
        };
        self.announce_output_notice(notice);
    }

    /// Why the last exclusive open was refused, when it was.
    pub fn exclusive_refusal(&self) -> Option<ExclusiveRefusal> {
        ExclusiveRefusal::from_u8(
            self.shared
                .exclusive_refusal
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// Emit `notice` only when it changes what the user was last told
    /// (#597). `None` clears the memory without saying anything, so a
    /// device that starts accepting exclusive again can be reported when
    /// it next refuses.
    fn announce_output_notice(&self, notice: Option<PlaybackNotice>) {
        let changed = match self.last_output_notice.lock() {
            Ok(mut guard) => {
                let changed = *guard != notice;
                *guard = notice;
                changed
            }
            // A poisoned lock costs a repeat, never a silence.
            Err(_) => true,
        };
        if let (true, Some(notice)) = (changed, notice) {
            self.emit_playback_notice(notice);
        }
    }

    /// Tell the UI about `notice`, unconditionally. For the one-shot
    /// events that are not a persisting state — see
    /// [`Self::announce_output_notice`] for the ones that are.
    pub(in crate::audio) fn emit_playback_notice(&self, notice: PlaybackNotice) {
        tracing::info!(?notice, "playback notice");
        let _ = self
            .app
            .emit("player:notice", serde_json::json!({ "kind": notice }));
    }
}
