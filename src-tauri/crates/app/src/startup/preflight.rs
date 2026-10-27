//! What has to be decided or refused before the builder, the logger or
//! any window exists.

use crate::error::AppError;
#[cfg(target_os = "linux")]
use crate::external_open;
use crate::{backup_prompt, db, display_backend, paths, render_mode};

use super::fatal::{exit_on_request, report_fatal_and_exit};

/// Decide how the interface will be drawn, before anything can draw it.
///
/// Runs before the webview exists, because the environment variables it
/// sets are read by the web engine when *its* process starts and the
/// `main` webview is created by `Builder::build` — before `setup` runs
/// at all. And before logging, because it mutates the process
/// environment and the logger owns a thread.
///
/// Non-fatal in every direction: a missing app-data directory means no
/// marker can be armed, which is exactly where the app was before this
/// existed, and is not a reason to refuse to start.
pub(crate) fn preflight_render_mode(identifier: &str) {
    match paths::AppPaths::root_for_identifier(identifier) {
        Ok(root) => {
            // Before anything builds a window, like the renderer: GTK
            // reads `GDK_BACKEND` when it initialises (#894).
            display_backend::decide(&root);
            render_mode::decide(root);
        }
        Err(err) => {
            // Logging is not up yet, so this goes to stderr — the only
            // place there is. It is also the one case where nothing was
            // recorded at all, so silence would leave no trace anywhere.
            eprintln!("waveflow: no app-data dir ({err}); the renderer fallback is inactive");
        }
    }
}

/// Send GTK's file pickers through the desktop's portal (#696).
///
/// The dialog plugin draws its pickers with `rfd`'s GTK3 backend, which
/// opens a `GtkFileChooserNative`. GTK hands that to
/// `org.freedesktop.portal.FileChooser` when `GTK_USE_PORTAL=1` (and on
/// its own inside a Flatpak), so the desktop supplies its own dialog:
/// KDE's on Plasma, the current GNOME one on GNOME, instead of GTK3's
/// chooser everywhere. Where no portal answers — none installed, or one
/// without a file chooser — GTK shows its own dialog, as it did before.
/// Both measured on GTK 3.24.52 against a stand-in portal on a private
/// bus, folder pick included.
///
/// Not `rfd`'s `xdg-portal` backend: that one has no fallback but
/// zenity for the pickers, and hands every message dialog to zenity —
/// which a bare desktop or the Flatpak runtime may not have.
///
/// Set before anything builds a dialog (GTK reads the variable once) and
/// before the logger's thread exists. A value the user set wins, so
/// `GTK_USE_PORTAL=0` brings GTK3's chooser back. Withheld from the
/// programs the app starts: a browser opened for a link should keep the
/// pickers its own user chose.
#[cfg(target_os = "linux")]
pub(crate) fn preflight_file_dialogs() {
    const KEY: &str = "GTK_USE_PORTAL";
    if std::env::var_os(KEY).is_some() {
        return;
    }
    std::env::set_var(KEY, "1");
    external_open::note_own_variable(KEY);
}

/// Give the AppImage's bundled GStreamer a plugin registry of its own.
///
/// The AppImage ships its own `libgstreamer` and plugins
/// (`bundleMediaFramework`), but without `GST_REGISTRY` it caches what it
/// found in the host's `~/.cache/gstreamer-1.0/registry.<arch>.bin` — the
/// very file the system's GStreamer uses. The two are different versions
/// with different plugin sets, so each keeps rewriting the other's cache.
/// Same timing constraint as the renderer: the web process reads the
/// environment when it starts, so this has to run before any window
/// exists and before the logger's thread does. A user-set registry wins.
///
/// The plugins are also reached through a fixed symlink. The AppImage is
/// mounted under a new `/tmp/.mount_*` directory at every launch, and the
/// registry is keyed by plugin path: pointed at the mount itself, it went
/// stale each time and GStreamer rescanned every plugin the first time a
/// video was shown, freezing the web process for several seconds per
/// launch. Through the link the paths never change, so only an update of
/// the AppImage, whose plugin files are new, costs a rescan.
#[cfg(target_os = "linux")]
pub(crate) fn preflight_appimage_gstreamer(identifier: &str) {
    const KEY: &str = "GST_REGISTRY_1_0";
    if std::env::var_os("APPIMAGE").is_none()
        || std::env::var_os(KEY).is_some()
        || std::env::var_os("GST_REGISTRY").is_some()
    {
        return;
    }
    let Some(dir) = dirs::cache_dir().map(|cache| cache.join(identifier)) else {
        return;
    };
    std::env::set_var(KEY, dir.join("gstreamer-registry.bin"));
    external_open::note_own_variable(KEY);
    if let Err(err) = link_appimage_gstreamer_plugins(&dir) {
        // Logging is not up yet. Without the link, video still plays; the
        // first one of each launch is just slow to start.
        eprintln!("waveflow: no stable gstreamer plugin path ({err})");
    }
}

/// Point `<dir>/gstreamer-plugins` at the mounted AppImage's plugin
/// directory and rewrite the plugin-path variables the AppImage set to go
/// through it.
#[cfg(target_os = "linux")]
fn link_appimage_gstreamer_plugins(dir: &std::path::Path) -> std::io::Result<()> {
    let Some(appdir) = std::env::var_os("APPDIR") else {
        return Ok(());
    };
    let plugins = std::path::Path::new(&appdir).join("usr/lib/gstreamer-1.0");
    let Some(plugins_str) = plugins.to_str().filter(|_| plugins.is_dir()) else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)?;
    let link = dir.join("gstreamer-plugins");
    // A link into another mount that still exists belongs to an instance
    // that is running: this one may be about to lose the single-instance
    // check, and re-aiming the link would pull the plugins out from under
    // the survivor once this mount goes. Such a launch keeps its own paths.
    if let Ok(target) = std::fs::read_link(&link) {
        if target != plugins && target.is_dir() {
            return Ok(());
        }
    }
    // Staged and renamed over the old link, so a half-made one never shows.
    let staged = dir.join(format!(".gstreamer-plugins.{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(&plugins, &staged)?;
    std::fs::rename(&staged, &link)?;
    let Some(link_str) = link.to_str() else {
        return Ok(());
    };
    for key in ["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_PATH_1_0"] {
        if let Some(value) = std::env::var(key).ok().filter(|v| v.contains(plugins_str)) {
            std::env::set_var(key, value.replace(plugins_str, link_str));
            external_open::note_own_variable(key);
        }
    }
    Ok(())
}

/// Vet the databases startup is about to open, and stop with an
/// explanation if one of them came from a newer build — or, when one is
/// about to migrate and could not be copied first, ask the user whether
/// to go on, and stop if they say no.
///
/// Runs from [`run`] before `tauri::Builder` is even constructed, so a
/// refusal costs the user a dialog instead of a mystery. The detection
/// itself stays in [`db::schema_guard`], which the real opens call
/// again downstream; this is the early, best-effort pass whose only
/// privilege is being able to talk to the user.
///
/// Anything short of a verdict — no app-data dir, no database yet, a
/// file it can't read — proceeds to normal startup.
pub(crate) fn preflight_schema_guard(identifier: &str) {
    let root = match paths::AppPaths::root_for_identifier(identifier) {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(%err, "startup pre-flight: no app-data dir, skipping");
            return;
        }
    };

    let paths = paths::AppPaths::from_root(root, None);
    match tauri::async_runtime::block_on(db::schema_guard::preflight(
        &paths,
        &backup_prompt::ask_at_launch,
    )) {
        None => {}
        // The user was just asked and chose to stop: no second dialog.
        Some(AppError::PreMigrationBackupDeclined { .. }) => exit_on_request(),
        Some(err) => report_fatal_and_exit(&err),
    }
}
