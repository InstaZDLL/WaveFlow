//! Follow the OS default output device while nothing is pinned (#627).
//!
//! With no device pinned, the output opens whatever the system default
//! is *at that moment* and the stream stays bound to that endpoint for
//! its lifetime. Nothing used to subscribe to default-device changes —
//! neither WaveFlow nor cpal 0.17, which exposes no notification API at
//! all — so plugging in a headset moved the system's default and left
//! playback on the old speakers. After #612 the picker at least told
//! the truth about which endpoint that was; "follow the OS default"
//! still followed nothing.
//!
//! This module is the subscription, one implementation per platform:
//!
//! - **Windows** — `IMMNotificationClient` registered on the
//!   `IMMDeviceEnumerator`, filtered to the render / console role.
//! - **macOS** — a HAL property listener on
//!   `kAudioHardwarePropertyDefaultOutputDevice`.
//! - **Linux** — nothing: PipeWire and PulseAudio migrate a running
//!   stream to the new default sink themselves, and our stream is one
//!   of their clients (cpal opens the `default` ALSA alias). No
//!   measurement says otherwise, so this stays a documented no-op
//!   rather than speculative plumbing.
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

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // See the module doc: the sound server already migrates the
        // stream on Linux. Bind the parameter so the signature stays
        // identical across platforms.
        let _ = app;
        tracing::debug!(
            "default-device watcher: not needed on this platform \
             (the sound server migrates the stream)"
        );
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use tauri::AppHandle;
    use windows::core::{implement, Result as WinResult, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE,
    };
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
        /// The one notification we act on.
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
                crate::audio::output::schedule_default_device_follow(&self.app);
            }
            Ok(())
        }

        // The rest of the interface. A device appearing, disappearing or
        // changing state does not move the default on its own — Windows
        // sends `OnDefaultDeviceChanged` when it does — and losing the
        // device we are playing on already arrives as a cpal
        // `DeviceNotAvailable`, which owns its own recovery (#175).
        fn OnDeviceStateChanged(&self, _device_id: &PCWSTR, _state: DEVICE_STATE) -> WinResult<()> {
            Ok(())
        }

        fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> WinResult<()> {
            Ok(())
        }

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
        kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, AudioObjectAddPropertyListener,
        AudioObjectID, AudioObjectPropertyAddress,
    };
    use tauri::AppHandle;

    pub(super) fn spawn(app: AppHandle) {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        // Leaked on purpose. The listener is never removed (see the
        // module doc), so the client data it dereferences has to outlive
        // every scope we could tie it to — anything else would hand
        // CoreAudio a dangling pointer somewhere in the shutdown.
        let client: *mut c_void = Box::into_raw(Box::new(app)).cast();

        // SAFETY: `address` is read during the call only; the listener
        // matches the signature CoreAudio expects; `client` points at a
        // leaked `AppHandle` that lives as long as the process.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                Some(on_default_output_changed),
                client,
            )
        };
        if status == 0 {
            tracing::info!("following the OS default output device (CoreAudio HAL listener)");
        } else {
            tracing::warn!(
                status,
                "default-device watcher: HAL listener refused; not following the OS default"
            );
            // Take the box back: the listener will never be called, so
            // nothing needs the pointer.
            // SAFETY: the pointer comes from `Box::into_raw` just above
            // and was never handed out, the registration having failed.
            drop(unsafe { Box::from_raw(client.cast::<AppHandle>()) });
        }
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
        // Unwinding from here would cross back into CoreAudio's own C
        // frames, so a panic is caught instead of propagated.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `client` is the leaked `AppHandle` from `spawn`,
            // which is never freed while the listener is registered.
            let app = unsafe { &*client.cast::<AppHandle>() };
            tracing::debug!("default output device changed");
            crate::audio::output::schedule_default_device_follow(app);
        }));
        0
    }
}
