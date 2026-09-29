//! Ask WebView2 to give memory back while a window is out of sight.
//!
//! A minimized or hidden window keeps its whole WebView2 renderer
//! resident. Measured on a Release build, minimizing the main window to
//! keep only the mini-player freed nothing: the main renderer stayed at
//! about 115 MB private and 180 MB working set, more than the whole
//! mini-player costs. And closing the mini-player only hides it, so its
//! renderer stays resident too.
//!
//! WebView2 has a switch for exactly this: `MemoryUsageTargetLevel`.
//! Set to `Low`, the runtime trims what it can — caches, and pages it can
//! swap out — while the page keeps running, so playback, lyrics sync and
//! events carry on. Back to `Normal` as soon as the window is on screen
//! again, since a visible page at `Low` would pay for it in jank.
//!
//! Driven from the window events in `lib.rs`: `Resized` fires on minimize
//! and restore, `Focused` when a window is hidden or shown (the
//! mini-player hides itself from the frontend, which emits nothing else
//! on the Rust side). Close-to-tray calls [`sync`] after its `hide`, and
//! every place the backend shows the main window again (tray, second
//! launch, end of the splash) calls it after its `show`, so coming back
//! never waits on a focus event to be back at `Normal`.
//! Each call re-reads the window's real state, so a missed or extra event
//! costs nothing, and a level already applied is not sent again.
//!
//! Windows only: WebKitGTK and WKWebView have no equivalent control.
//! `ICoreWebView2_19` needs WebView2 runtime 1.0.2210 or newer; an older
//! runtime simply keeps its memory, as before.

/// Windows the switch applies to: the two that stay alive while unseen.
/// The desktop lyrics overlay is destroyed when closed.
pub const LABELS: [&str; 2] = ["main", "mini"];

/// Per window label: present once the window has been on screen, holding
/// the last level sent (`Some(idle)`), or `None` when the last call failed
/// and the level is unknown. The level spares a burst of `Resized` events
/// during a drag-resize a COM call each; forgetting only the level on a
/// failure lets the next event retry either way, while the window still
/// counts as shown.
// The bookkeeping is platform-free so its tests run on the Linux CI job,
// the only one that runs this crate's tests; only Windows calls it.
#[cfg_attr(not(windows), allow(dead_code))]
static APPLIED: std::sync::Mutex<Option<std::collections::HashMap<String, Option<bool>>>> =
    std::sync::Mutex::new(None);

/// Record `idle` as about to be sent for `label`, and say whether it needs
/// sending. `None` records that the last call failed.
#[cfg_attr(not(windows), allow(dead_code))]
fn remember(label: &str, idle: Option<bool>) -> bool {
    let mut applied = APPLIED.lock().unwrap_or_else(|e| e.into_inner());
    let applied = applied.get_or_insert_with(std::collections::HashMap::new);
    match idle {
        // A window that has never been on screen is left alone: the main
        // window is created hidden behind the splash, and a page loading
        // at `Low` would pay for it in a slower launch.
        Some(true) if !applied.contains_key(label) => false,
        Some(idle) => applied.insert(label.to_string(), Some(idle)) != Some(Some(idle)),
        None => {
            if let Some(level) = applied.get_mut(label) {
                *level = None;
            }
            true
        }
    }
}

/// Re-evaluate `window` and set its WebView2 memory target to match:
/// `Low` while minimized or hidden, `Normal` otherwise.
#[cfg(windows)]
pub fn sync(window: &tauri::WebviewWindow) {
    let idle = window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true);
    if !remember(window.label(), Some(idle)) {
        return;
    }

    let label = window.label().to_string();
    let result = window.with_webview(move |webview| match apply(&webview, idle) {
        Ok(()) => tracing::debug!(window = %label, idle, "webview memory target applied"),
        Err(e) => {
            remember(&label, None);
            tracing::debug!(window = %label, idle, error = %e, "webview memory target not applied");
        }
    });
    if let Err(e) = result {
        remember(window.label(), None);
        tracing::debug!(window = %window.label(), error = %e, "webview unavailable for memory target");
    }
}

#[cfg(not(windows))]
pub fn sync(_window: &tauri::WebviewWindow) {}

#[cfg(windows)]
fn apply(webview: &tauri::webview::PlatformWebview, idle: bool) -> windows::core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    };
    use windows::core::Interface;

    let level = if idle {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    // SAFETY: plain COM calls on interfaces wry hands us, made on the
    // thread `with_webview` runs its closure on — the one that owns them.
    unsafe {
        let core = webview.controller().CoreWebView2()?;
        // Fails with E_NOINTERFACE on a runtime older than the API.
        let core = core.cast::<ICoreWebView2_19>()?;
        core.SetMemoryUsageTargetLevel(level)
    }
}

#[cfg(test)]
mod tests {
    use super::remember;

    // Each test uses its own label: the cache is process-wide.

    #[test]
    fn a_window_never_shown_is_not_lowered() {
        assert!(!remember("t-never-shown", Some(true)));
        assert!(remember("t-never-shown", Some(false)));
    }

    #[test]
    fn a_shown_window_is_lowered_once_then_restored() {
        assert!(remember("t-cycle", Some(false)));
        assert!(remember("t-cycle", Some(true)));
        assert!(!remember("t-cycle", Some(true)), "same level is not resent");
        assert!(remember("t-cycle", Some(false)));
    }

    #[test]
    fn a_failed_call_is_retried_by_the_next_event() {
        assert!(remember("t-retry", Some(false)));
        remember("t-retry", None);
        assert!(remember("t-retry", Some(false)));
    }

    #[test]
    fn a_failed_lowering_is_retried_on_the_next_hide() {
        assert!(remember("t-retry-low", Some(false)));
        assert!(remember("t-retry-low", Some(true)));
        remember("t-retry-low", None);
        assert!(
            remember("t-retry-low", Some(true)),
            "a failure must not make the window count as never shown"
        );
    }
}
