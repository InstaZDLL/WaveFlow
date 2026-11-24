//! Follow the OS default output device while nothing is pinned (#627),
//! and bring a pinned device back once it returns (#1165).
//!
//! With no device pinned, the output opens whatever the system default
//! is *at that moment* and the stream stays bound to that endpoint for
//! its lifetime (cpal 0.18 can follow the default by itself, but
//! `output::default_output_endpoint` opens the fixed endpoint instead).
//! Nothing used to subscribe to default-device changes — neither
//! WaveFlow nor cpal 0.17, which exposed no notification API at all — so
//! plugging in a headset moved the system's default and left
//! playback on the old speakers. After #612 the picker at least told
//! the truth about which endpoint that was; "follow the OS default"
//! still followed nothing.
//!
//! This module is the subscription, one implementation per platform:
//!
//! - **Windows** — `IMMNotificationClient` registered on the
//!   `IMMDeviceEnumerator`: a default change filtered to the render /
//!   console role, plus an endpoint added or turned `ACTIVE`.
//! - **macOS** — HAL property listeners on
//!   `kAudioHardwarePropertyDefaultOutputDevice` and
//!   `kAudioHardwarePropertyDevices`.
//! - **Linux** — no follow: PipeWire and PulseAudio move a running
//!   stream to the new default sink themselves, and our stream is one
//!   of their clients (cpal opens the `default` ALSA alias). Measured on
//!   PipeWire 1.6 (#1158): a default picked in the system settings moves
//!   the stream at once, with nothing for us to do.
//!
//!   The same move is why a device going away never reaches us there:
//!   unplugging the sink under the stream moves it too, without a single
//!   error on the ALSA side, so the device-loss recovery never ran and
//!   `audio.pause_on_device_loss` did nothing. Linux therefore starts
//!   `audio::sink_watch` instead, which asks the sound server whether
//!   the device under our stream is gone.
//!
//! The second kind of event is for a pinned device. Every open path
//! falls back to the default when the pin is no longer enumerated, and
//! keeps the pin as the request; a Bluetooth headset that drops and
//! reconnects is the usual case, and it is not always made the default
//! again — nor listed yet when it is. So an endpoint arriving is reported
//! too, and the engine reopens the pin if it is listed and not the device
//! playing. With nothing pinned, an arrival is ignored: it moves no
//! default.
//!
//! **Neither listener rebuilds anything itself.** Both hand the event to
//! [`super::output::schedule_default_device_follow`], which puts it
//! through the same rebuild gate the device-loss recovery uses: one
//! physical change can fire several notifications, and our own reopen
//! makes the outgoing stream fail, which schedules yet another rebuild.
//! The gate is what keeps that from becoming a cascade.
//!
//! The registration lasts for the life of the process. Nothing here is
//! ever unregistered: there is no point in the app's life where we stop
//! caring which device the system prefers, and a shutdown that tore the
//! listener down would still race the notification already in flight.

use tauri::AppHandle;

/// Subscribe to OS default-output changes. Called once from `setup`,
/// after the engine is registered in Tauri state — a notification that
/// lands before then simply finds no engine and returns.
///
/// Never fails the caller: a platform that refuses the subscription
/// logs and leaves the app exactly as it was before this existed.
pub fn spawn(app: AppHandle) {
    #[cfg(target_os = "windows")]
    windows_impl::spawn(app);

    #[cfg(target_os = "macos")]
    macos_impl::spawn(app);

    // See the module doc: the sound server already moves the stream on
    // Linux, so what is watched there is the device going away.
    #[cfg(target_os = "linux")]
    super::sink_watch::spawn(app);

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        // Bind the parameter so the signature stays identical across
        // platforms.
        let _ = app;
        tracing::debug!("default-device watcher: not available on this platform");
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use tauri::AppHandle;
    use windows::core::{implement, Result as WinResult, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    };

    use crate::audio::engine::OutputEvent;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// The COM sink. Holds the `AppHandle` so a notification can reach
    /// the engine without a global.
    #[implement(IMMNotificationClient)]
    struct DefaultDeviceWatcher {
        app: AppHandle,
    }

    impl IMMNotificationClient_Impl for DefaultDeviceWatcher_Impl {
        /// The default moved.
        ///
        /// Windows fires this once **per role** — console, multimedia,
        /// communications — for a single physical change. We keep only
        /// `eConsole` because that is the role both of our open paths
        /// ask for: cpal's `default_output_device` calls
        /// `GetDefaultAudioEndpoint(flow, eConsole)`, and so does
        /// `wasapi::get_default_device` on the exclusive side. Reacting
        /// to the other two roles would rebuild for a default we would
        /// never have opened.
        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            role: ERole,
            _device_id: &PCWSTR,
        ) -> WinResult<()> {
            if flow == eRender && role == eConsole {
                tracing::debug!("default render endpoint changed (console role)");
                crate::audio::output::schedule_default_device_follow(
                    &self.app,
                    OutputEvent::DefaultChanged,
                );
            }
            Ok(())
        }

        /// An endpoint became usable — what a Bluetooth headset that
        /// reconnects sends, its endpoints going back to `ACTIVE`
        /// (#1165). Not filtered to render endpoints: telling them apart
        /// needs a COM call on the notification thread, and the engine
        /// already ignores an arrival unless a pinned device is listed
        /// and not the one playing, which a capture endpoint never is.
        fn OnDeviceStateChanged(&self, _device_id: &PCWSTR, state: DEVICE_STATE) -> WinResult<()> {
            if state == DEVICE_STATE_ACTIVE {
                tracing::debug!("an audio endpoint became active");
                crate::audio::output::schedule_default_device_follow(
                    &self.app,
                    OutputEvent::EndpointArrived,
                );
            }
            Ok(())
        }

        /// A new endpoint, for the same reason as the state change above.
        fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> WinResult<()> {
            tracing::debug!("an audio endpoint was added");
            crate::audio::output::schedule_default_device_follow(
                &self.app,
                OutputEvent::EndpointArrived,
            );
            Ok(())
        }

        // The rest of the interface. A device disappearing does not move
        // the default on its own — Windows sends `OnDefaultDeviceChanged`
        // when it does — and losing the device we are playing on already
        // arrives as a cpal `DeviceNotAvailable`, which owns its own
        // recovery (#175).
        fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> WinResult<()> {
            Ok(())
        }

        fn OnPropertyValueChanged(&self, _device_id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
            Ok(())
        }
    }

    /// Register the sink on a thread of its own, and keep that thread
    /// alive for the life of the process.
    ///
    /// The thread is not an implementation detail: the enumerator and
    /// the sink have to stay referenced for the subscription to keep
    /// existing, and the apartment they were created in has to outlive
    /// them. A dedicated thread gives both without borrowing the main
    /// thread's apartment, which Tauri owns — the notifications arrive
    /// on COM's own threads, so the sink must live in a multithreaded
    /// apartment.
    pub(super) fn spawn(app: AppHandle) {
        let spawned = std::thread::Builder::new()
            .name("wf-default-device".into())
            .spawn(move || {
                // SAFETY: called once, on a thread this closure owns and
                // which never initializes COM again.
                let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                if hr.is_err() {
                    tracing::warn!(
                        ?hr,
                        "default-device watcher: COM init failed; not following the OS default"
                    );
                    return;
                }

                // SAFETY: the CLSID and the requested interface match,
                // and COM is initialized on this thread just above.
                let enumerator: IMMDeviceEnumerator =
                    match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
                        Ok(enumerator) => enumerator,
                        Err(err) => {
                            tracing::warn!(
                                %err,
                                "default-device watcher: no device enumerator; \
                                 not following the OS default"
                            );
                            return;
                        }
                    };

                let client: IMMNotificationClient = DefaultDeviceWatcher { app }.into();
                // SAFETY: `client` is a live COM object, held below for
                // as long as the subscription lasts.
                if let Err(err) =
                    unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }
                {
                    tracing::warn!(
                        %err,
                        "default-device watcher: registration refused; \
                         not following the OS default"
                    );
                    return;
                }
                tracing::info!("following the OS default output device (WASAPI notifications)");

                // Hold both objects. Parking rather than returning is the
                // point: dropping them here would unregister the callback
                // we just installed.
                loop {
                    std::thread::park();
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "default-device watcher: thread spawn failed");
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr::NonNull;

    use objc2_core_audio::{
        kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
        AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
        AudioObjectPropertyListenerProc, AudioObjectPropertySelector,
    };
    use tauri::AppHandle;

    use crate::audio::engine::OutputEvent;

    pub(super) fn spawn(app: AppHandle) {
        // Leaked on purpose. The listeners are never removed (see the
        // module doc), so the client data they dereference has to
        // outlive every scope we could tie it to — anything else would
        // hand CoreAudio a dangling pointer somewhere in the shutdown.
        // Both listeners share it.
        let client: *mut c_void = Box::into_raw(Box::new(app)).cast();

        let default_status = add_listener(
            kAudioHardwarePropertyDefaultOutputDevice,
            Some(on_default_output_changed),
            client,
        );
        if default_status == 0 {
            tracing::info!("following the OS default output device (CoreAudio HAL listener)");
        } else {
            tracing::warn!(
                status = default_status,
                "default-device watcher: HAL listener refused; not following the OS default"
            );
        }
        // The device list, for a pinned device that comes back (#1165).
        let devices_status = add_listener(
            kAudioHardwarePropertyDevices,
            Some(on_devices_changed),
            client,
        );
        if devices_status != 0 {
            tracing::warn!(
                status = devices_status,
                "device-list watcher: HAL listener refused; a returning pinned device                  will not be reopened on its own"
            );
        }

        if default_status != 0 && devices_status != 0 {
            // Take the box back: no listener will ever be called, so
            // nothing needs the pointer.
            // SAFETY: the pointer comes from `Box::into_raw` just above
            // and was never handed out, both registrations having failed.
            drop(unsafe { Box::from_raw(client.cast::<AppHandle>()) });
        }
    }

    /// Register `listener` for one property of the system object.
    fn add_listener(
        selector: AudioObjectPropertySelector,
        listener: AudioObjectPropertyListenerProc,
        client: *mut c_void,
    ) -> i32 {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        // SAFETY: `address` is read during the call only; the listener
        // matches the signature CoreAudio expects; `client` points at a
        // leaked `AppHandle` that lives as long as the process.
        unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                listener,
                client,
            )
        }
    }

    /// Hand `event` to the engine from a HAL thread.
    ///
    /// Unwinding from here would cross back into CoreAudio's own C
    /// frames, so a panic is caught instead of propagated.
    fn schedule(client: *mut c_void, event: OutputEvent) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `client` is the leaked `AppHandle` from `spawn`,
            // which is never freed while a listener is registered.
            let app = unsafe { &*client.cast::<AppHandle>() };
            tracing::debug!(?event, "output device change from the HAL");
            crate::audio::output::schedule_default_device_follow(app, event);
        }));
    }

    /// Called by the HAL on one of its own threads.
    ///
    /// The addresses are ignored: the listener is registered for a
    /// single property, and the work it schedules re-reads the default
    /// anyway, so there is nothing to learn from the array.
    unsafe extern "C-unwind" fn on_default_output_changed(
        _object: AudioObjectID,
        _count: u32,
        _addresses: NonNull<AudioObjectPropertyAddress>,
        client: *mut c_void,
    ) -> i32 {
        schedule(client, OutputEvent::DefaultChanged);
        0
    }

    /// The device list changed: something arrived or left. Reported as
    /// an arrival either way — the engine checks whether the pin is
    /// listed, and a departure is the device-loss recovery's.
    unsafe extern "C-unwind" fn on_devices_changed(
        _object: AudioObjectID,
        _count: u32,
        _addresses: NonNull<AudioObjectPropertyAddress>,
        client: *mut c_void,
    ) -> i32 {
        schedule(client, OutputEvent::EndpointArrived);
        0
    }
}
