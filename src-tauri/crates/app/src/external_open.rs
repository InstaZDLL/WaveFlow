//! Opening a folder or a link with the desktop's own handler.
//!
//! Everything goes through `tauri_plugin_opener`, except inside an
//! AppImage. Its launcher points `LD_LIBRARY_PATH`, `GIO_MODULE_DIR`,
//! `GTK_PATH`, `PATH`, `XDG_DATA_DIRS` and the rest into the image, so that
//! the bundled WebKitGTK finds its own libraries — and every program we
//! start inherits that. `xdg-open` then runs the host's `gio`, file
//! manager or browser against the image's libraries, and they die on the
//! first missing symbol: on a Fedora laptop `gio` stopped at
//! `undefined symbol: g_unix_mount_entry_get_options` and Firefox at
//! `version NSS_3.126 not found`, so "Open log folder" did nothing. The
//! launched program gets the host's environment back instead.
//!
//! Revealing a file in its folder is not routed here: the plugin asks
//! the file manager over D-Bus, which starts it with its own environment.

use std::path::Path;

use crate::error::{AppError, AppResult};

/// Open a folder or a file with the handler the desktop associates with it.
pub fn open_path(path: impl AsRef<Path>) -> AppResult<()> {
    let path = path.as_ref();
    #[cfg(target_os = "linux")]
    if let Some(result) = appimage::open(path.as_os_str()) {
        return result;
    }
    tauri_plugin_opener::open_path(path, None::<&str>)
        .map_err(|err| AppError::Other(format!("open_path: {err}")))
}

/// Open a link in the user's browser, or a `mailto:` in their mail client.
pub fn open_url(url: &str) -> AppResult<()> {
    #[cfg(target_os = "linux")]
    if let Some(result) = appimage::open(std::ffi::OsStr::new(url)) {
        return result;
    }
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|err| AppError::Other(format!("open_url: {err}")))
}

/// Links a caller outside the app — a plugin — may ask to open. The same
/// schemes the opener plugin's default scope allowed before this module
/// took over the frontend's calls.
pub fn is_openable_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https" | "mailto"))
}

/// Variables the app itself set at startup for the image's own benefit:
/// the GStreamer registry and plugin paths, which go through a link in
/// the cache directory and so never name the mount. A host program handed
/// them would load the image's plugins — or share a registry written by
/// another GStreamer — as surely as through `LD_LIBRARY_PATH`.
#[cfg(target_os = "linux")]
static SET_FOR_APPIMAGE: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

/// Record a variable set for the image, to be withheld from programs it
/// starts. Called by the startup preflight, before any thread exists.
#[cfg(target_os = "linux")]
pub fn note_appimage_variable(key: &'static str) {
    let mut keys = SET_FOR_APPIMAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !keys.contains(&key) {
        keys.push(key);
    }
}

/// The value a variable should carry for a program started from inside
/// the image mounted at `appdir`: its entries under `appdir` dropped,
/// `None` when nothing is left, and `None` outright for a variable the
/// app set for the image (`set_for_image`). Values that never mention the
/// image come back unchanged.
#[cfg(any(target_os = "linux", test))]
fn host_value(value: &str, appdir: &str, set_for_image: bool) -> Option<String> {
    if set_for_image {
        return None;
    }
    if !value.contains(appdir) {
        return Some(value.to_string());
    }
    let kept: Vec<&str> = value
        .split(':')
        .filter(|entry| !entry.is_empty() && !entry.starts_with(appdir))
        .collect();
    (!kept.is_empty()).then(|| kept.join(":"))
}

#[cfg(target_os = "linux")]
mod appimage {
    use std::ffi::OsStr;
    use std::process::{Command, Stdio};

    use crate::error::{AppError, AppResult};

    /// `None` outside an AppImage, so the caller keeps the plugin's path.
    pub(super) fn open(target: &OsStr) -> Option<AppResult<()>> {
        std::env::var_os("APPIMAGE")?;
        let appdir = std::env::var("APPDIR").ok().filter(|dir| !dir.is_empty())?;
        let appdir = appdir.trim_end_matches('/');
        Some(spawn_first(target, appdir))
    }

    /// `xdg-open` first, then `gio open` — the opener plugin's order —
    /// both with the host's environment.
    ///
    /// `gio open` takes over when `xdg-open` cannot be started, and also
    /// when it starts but reports that it found no tool (3) or that the
    /// open failed (4). It is not retried on a bad argument (1) or a
    /// missing file (2), which `gio` would refuse too. The exit status is
    /// read off-thread: `xdg-open` can stay in the foreground for as long
    /// as the browser it started, so the caller only learns whether a
    /// launcher could be started at all.
    fn spawn_first(target: &OsStr, appdir: &str) -> AppResult<()> {
        let set_for_image = super::SET_FOR_APPIMAGE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut xdg = host_command(&["xdg-open"], target, appdir, &set_for_image);
        let mut gio = host_command(&["gio", "open"], target, appdir, &set_for_image);
        match xdg.spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let code = child.wait().ok().and_then(|status| status.code());
                    if !matches!(code, Some(3 | 4)) {
                        return;
                    }
                    match gio.spawn() {
                        Ok(mut child) => {
                            let _ = child.wait();
                        }
                        Err(err) => {
                            tracing::warn!(%err, ?code, "xdg-open failed and gio could not start");
                        }
                    }
                });
                Ok(())
            }
            Err(xdg_err) => match gio.spawn() {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    Ok(())
                }
                Err(gio_err) => Err(AppError::Other(format!(
                    "no launcher could open {}: xdg-open: {xdg_err}; gio: {gio_err}",
                    target.to_string_lossy()
                ))),
            },
        }
    }

    /// `launcher` opening `target`, with the environment a host program
    /// needs: see [`super::host_value`].
    fn host_command(
        launcher: &[&str],
        target: &OsStr,
        appdir: &str,
        set_for_image: &[&str],
    ) -> Command {
        let mut cmd = Command::new(launcher[0]);
        cmd.args(&launcher[1..])
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in std::env::vars_os() {
            let Some(value) = value.to_str() else {
                continue;
            };
            let ours = key.to_str().is_some_and(|key| set_for_image.contains(&key));
            match super::host_value(value, appdir, ours) {
                Some(host) if host == value => {}
                Some(host) => {
                    cmd.env(&key, host);
                }
                None => {
                    cmd.env_remove(&key);
                }
            }
        }
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::{host_value, is_openable_url};

    const APPDIR: &str = "/tmp/.mount_WaveFlkLdcgI";

    #[test]
    fn entries_inside_the_image_are_dropped_and_the_host_ones_kept() {
        let path = "/tmp/.mount_WaveFlkLdcgI/usr/bin/:/usr/local/bin:/usr/bin";
        assert_eq!(
            host_value(path, APPDIR, false).as_deref(),
            Some("/usr/local/bin:/usr/bin")
        );
    }

    #[test]
    fn a_variable_that_only_points_into_the_image_is_removed() {
        // The launcher writes some of them with a doubled slash.
        assert_eq!(
            host_value(
                "/tmp/.mount_WaveFlkLdcgI//usr/lib/gio/modules",
                APPDIR,
                false
            ),
            None
        );
        assert_eq!(
            host_value(
                "/tmp/.mount_WaveFlkLdcgI/usr/lib/:/tmp/.mount_WaveFlkLdcgI/usr/lib32/",
                APPDIR,
                false
            ),
            None
        );
    }

    #[test]
    fn a_path_the_app_aimed_at_the_image_through_a_link_is_removed() {
        // The link lives in the cache directory, so nothing in the value
        // names the mount: only knowing the app set it gives it away.
        let link = "/home/nayeon/.cache/app.waveflow/gstreamer-plugins";
        assert_eq!(host_value(link, APPDIR, true), None);
        assert_eq!(host_value(link, APPDIR, false).as_deref(), Some(link));
    }

    #[test]
    fn values_that_never_mention_the_image_are_left_alone() {
        assert_eq!(host_value("GNOME", APPDIR, false).as_deref(), Some("GNOME"));
        assert_eq!(host_value("", APPDIR, false).as_deref(), Some(""));
    }

    #[test]
    fn only_web_and_mail_links_are_openable() {
        assert!(is_openable_url("https://www.last.fm/api/account/create"));
        assert!(is_openable_url("http://example.com"));
        assert!(is_openable_url("mailto:someone@example.com"));
        assert!(!is_openable_url("file:///etc/passwd"));
        assert!(!is_openable_url("/home/user"));
        assert!(!is_openable_url("javascript:alert(1)"));
    }
}
