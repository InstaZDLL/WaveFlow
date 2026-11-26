//! Shuffle and repeat on the Windows SMTC session, next to souvlaki.
//!
//! souvlaki drives the session's buttons, timeline and metadata but never
//! touches `ShuffleEnabled` or `AutoRepeatMode`, so the media flyout and
//! the quick settings player showed neither for WaveFlow. Unlike MPRIS,
//! nothing has to be replaced to add them: `GetForWindow` hands back the
//! window's one session object — measured, a second call on the same
//! window returns the very `IUnknown` souvlaki holds, and a value set
//! through either handle is the one the shell reads. So this module asks
//! for the window's session itself and handles only the two properties
//! souvlaki leaves alone.
//!
//! A change asked for in the shell arrives as `ShuffleEnabledChangeRequested`
//! / `AutoRepeatModeChangeRequested`, which leave the published value as it
//! was: the request is forwarded, and the new value is set by
//! [`Options::set`] once the player has saved it, as on Linux.

use windows::core::Ref;
use windows::Foundation::TypedEventHandler;
use windows::Media::{
    AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode,
    ShuffleEnabledChangeRequestedEventArgs, SystemMediaTransportControls,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

use crate::queue::RepeatMode;

/// What the shell asked for.
pub(super) enum Request {
    Shuffle(bool),
    Repeat(RepeatMode),
}

/// The window's SMTC session, as far as shuffle and repeat go. Dropping it
/// takes its two handlers off the session.
pub(super) struct Options {
    session: SystemMediaTransportControls,
    shuffle_token: i64,
    repeat_token: i64,
}

impl Options {
    /// The session of `hwnd` — the one souvlaki already drives — with a
    /// handler on both change requests.
    pub(super) fn attach(
        hwnd: *mut std::ffi::c_void,
        handler: impl Fn(Request) + Send + Sync + Clone + 'static,
    ) -> windows::core::Result<Self> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        let session: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd)) }?;

        let on_shuffle = handler.clone();
        let shuffle_token = session.ShuffleEnabledChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<'_, ShuffleEnabledChangeRequestedEventArgs>| {
                on_shuffle(Request::Shuffle(args.ok()?.RequestedShuffleEnabled()?));
                Ok(())
            },
        ))?;
        let repeat_token = session
            .AutoRepeatModeChangeRequested(&TypedEventHandler::new(
                move |_, args: Ref<'_, AutoRepeatModeChangeRequestedEventArgs>| {
                    // A value this build does not know is not read as off.
                    if let Some(mode) = repeat_mode(args.ok()?.RequestedAutoRepeatMode()?) {
                        handler(Request::Repeat(mode));
                    }
                    Ok(())
                },
            ))
            // No `Options` to drop yet: take the first handler back here,
            // or it stays on the session souvlaki keeps.
            .inspect_err(|_| {
                let _ = session.RemoveShuffleEnabledChangeRequested(shuffle_token);
            })?;
        Ok(Self {
            session,
            shuffle_token,
            repeat_token,
        })
    }

    /// The modes in force. Setting the value the session already holds is
    /// harmless, so there is no comparison here.
    pub(super) fn set(&self, shuffle: bool, repeat: RepeatMode) -> windows::core::Result<()> {
        self.session.SetShuffleEnabled(shuffle)?;
        self.session.SetAutoRepeatMode(auto_repeat_mode(repeat))
    }
}

impl Drop for Options {
    fn drop(&mut self) {
        let _ = self
            .session
            .RemoveShuffleEnabledChangeRequested(self.shuffle_token);
        let _ = self
            .session
            .RemoveAutoRepeatModeChangeRequested(self.repeat_token);
    }
}

/// WaveFlow's repeat modes as SMTC names them: one to one, as for MPRIS.
fn auto_repeat_mode(repeat: RepeatMode) -> MediaPlaybackAutoRepeatMode {
    match repeat {
        RepeatMode::Off => MediaPlaybackAutoRepeatMode::None,
        RepeatMode::All => MediaPlaybackAutoRepeatMode::List,
        RepeatMode::One => MediaPlaybackAutoRepeatMode::Track,
    }
}

fn repeat_mode(mode: MediaPlaybackAutoRepeatMode) -> Option<RepeatMode> {
    match mode {
        MediaPlaybackAutoRepeatMode::None => Some(RepeatMode::Off),
        MediaPlaybackAutoRepeatMode::List => Some(RepeatMode::All),
        MediaPlaybackAutoRepeatMode::Track => Some(RepeatMode::One),
        _ => None,
    }
}
