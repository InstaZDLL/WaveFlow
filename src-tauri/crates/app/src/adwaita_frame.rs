//! The desktop's own frame, dressed as a current GNOME app (B1).
//!
//! On Linux the window is GTK3: wry binds the GTK3 build of WebKitGTK and
//! there is no GTK4 path in the stack (`docs/upstream-blockers.md`, B1). On
//! a Wayland session GTK3 draws the frame itself, as client-side
//! decorations, and with the stock theme that frame is the GTK3 Adwaita
//! title bar: compact, a gradient, square buttons — next to libadwaita
//! apps it is the one thing that dates the window.
//!
//! Since GTK draws it, GTK's stylesheet decides how it looks. This module
//! adds one, scoped to the main window by a class, that gives the frame a
//! libadwaita header bar's measurements (46 px, round 24 px buttons, a
//! bold title, 12 px corners, the soft shadow) in WaveFlow's own colours.
//! Everything else stays the toolkit's: the button order the desktop asks
//! for (`gtk-decoration-layout`), the window menu on a right click, the
//! double-click to maximise, the resize edges and the shadow region.
//! Those are exactly what a frame drawn by the page would have to imitate.
//!
//! **Only where GTK draws the frame.** Under X11 (XWayland included) the
//! window manager draws it — Mutter's own on GNOME, KWin's on KDE — and a
//! GTK stylesheet does not reach it; the choice is not offered there. On
//! KDE's Wayland session GTK asks KWin for its frame too, and the rules,
//! all scoped to `.csd`, simply match nothing.
//!
//! **Only the main window.** Every rule sits under [`WINDOW_CLASS`], put on
//! that window alone, so the file pickers, the tray menu, the mini-player
//! and the desktop lyrics keep the desktop's look. The stylesheet is added
//! at `STYLE_PROVIDER_PRIORITY_APPLICATION`, under the user's own
//! `gtk.css`, which keeps the last word.

use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::sync::{Arc, OnceLock};

use tauri::AppHandle;

#[cfg(target_os = "linux")]
use crate::error::AppError;
use crate::error::AppResult;

/// Put on the main GTK window while the style is on. Every selector of
/// [`stylesheet`] starts with it.
#[cfg(target_os = "linux")]
pub const WINDOW_CLASS: &str = "waveflow-adwaita";

/// The colours the frame takes: the background of WaveFlow's own page,
/// so the bar reads as the top of the window rather than a strip above
/// it, and which side of light and dark that background sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameColors {
    pub background: [u8; 3],
    pub dark: bool,
}

impl FrameColors {
    /// `#rrggbb` only. The value ends up inside a stylesheet, so anything
    /// richer is refused rather than escaped.
    ///
    /// Dark or light is read from the colour itself, not from the theme's
    /// mode: a skin paints its own page (Editorial's paper is light under
    /// a dark theme), and the title has to stay readable on what is there.
    pub fn parse(background: &str) -> Option<Self> {
        let hex = background.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let background = [channel(0)?, channel(2)?, channel(4)?];
        let [r, g, b] = background.map(f32::from);
        // Rec. 601 luma, on the 0-255 scale.
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        Some(Self {
            background,
            dark: luma < 128.0,
        })
    }

    /// Before the interface has said anything: a libadwaita header bar's
    /// own colours, on the side GTK was told to prefer.
    #[cfg(target_os = "linux")]
    fn fallback(dark: bool) -> Self {
        Self {
            background: if dark {
                [0x2e, 0x2e, 0x32]
            } else {
                [0xff, 0xff, 0xff]
            },
            dark,
        }
    }
}

/// The last colours the interface sent. Kept here rather than only in the
/// stylesheet: the style can be turned on long after they arrive, and it
/// must come up in them, not in the fallback.
static COLORS: Mutex<Option<FrameColors>> = Mutex::new(None);

/// The stylesheet for `colors`. Pure, so what it promises — every rule
/// scoped to [`WINDOW_CLASS`] and a client-side frame — is testable
/// without a window.
///
/// The measurements are libadwaita's (`headerbar`, `windowcontrols`,
/// `window.csd`): a 46 px bar, 24 px round buttons whose fill is the text
/// colour at 10 % (15 % hovered, 30 % pressed), 12 px top corners. The
/// bottom corners stay square: GTK3 does not clip the web view to the
/// frame's radius, so rounding them would round the shadow and nothing
/// else.
#[cfg(target_os = "linux")]
pub fn stylesheet(colors: FrameColors) -> String {
    let [r, g, b] = colors.background;
    let bg = format!("rgb({r}, {g}, {b})");
    // libadwaita's window foreground for each side.
    let fg = if colors.dark {
        "#ffffff"
    } else {
        "rgba(0, 0, 6, 0.8)"
    };
    // A dark window gets a faint light edge on top and a dark outline,
    // a light one a faint dark outline; the line under the bar is the
    // header-bar shade.
    let (top_edge, outline, shade) = if colors.dark {
        (
            "inset 0 1px rgba(255, 255, 255, 0.07), ",
            "rgba(0, 0, 0, 0.75)",
            "rgba(0, 0, 6, 0.36)",
        )
    } else {
        ("", "rgba(0, 0, 0, 0.05)", "rgba(0, 0, 6, 0.12)")
    };
    let w = format!("window.{WINDOW_CLASS}.csd");
    let bar = format!("{w} > headerbar.titlebar.default-decoration");
    format!(
        "{w} decoration {{
  border-radius: 12px 12px 0 0;
  box-shadow: 0 1px 3px 3px transparent, 0 2px 8px 2px rgba(0, 0, 0, 0.13),
    0 3px 20px 10px rgba(0, 0, 0, 0.09), 0 6px 32px 16px rgba(0, 0, 0, 0.04),
    0 0 0 1px {outline};
}}
{w} decoration:backdrop {{
  box-shadow: 0 1px 3px 3px transparent, 0 2px 6px 2px rgba(0, 0, 0, 0.07),
    0 0 0 1px {outline};
}}
{bar},
{bar}:backdrop {{
  min-height: 46px;
  padding: 0 7px;
  border: none;
  border-radius: 12px 12px 0 0;
  background: {bg};
  color: {fg};
  box-shadow: {top_edge}inset 0 -1px {shade};
}}
{w}.maximized decoration,
{w}.fullscreen decoration,
{w}.tiled decoration,
{w}.tiled-top decoration,
{w}.tiled-bottom decoration,
{w}.tiled-left decoration,
{w}.tiled-right decoration,
{w}.maximized > headerbar.titlebar.default-decoration,
{w}.fullscreen > headerbar.titlebar.default-decoration,
{w}.tiled > headerbar.titlebar.default-decoration,
{w}.tiled-top > headerbar.titlebar.default-decoration,
{w}.tiled-bottom > headerbar.titlebar.default-decoration,
{w}.tiled-left > headerbar.titlebar.default-decoration,
{w}.tiled-right > headerbar.titlebar.default-decoration {{
  border-radius: 0;
}}
{bar} .title {{
  font-weight: bold;
}}
{bar} button.titlebutton,
{bar} button.titlebutton:backdrop {{
  min-width: 24px;
  min-height: 24px;
  margin: 0 3px;
  padding: 0;
  border: none;
  border-radius: 9999px;
  background: alpha(currentColor, 0.1);
  box-shadow: none;
  color: inherit;
  -gtk-icon-shadow: none;
}}
{bar} button.titlebutton:hover {{
  background: alpha(currentColor, 0.15);
}}
{bar} button.titlebutton:active {{
  background: alpha(currentColor, 0.3);
}}
{bar}:backdrop .title,
{bar}:backdrop button.titlebutton {{
  color: alpha({fg}, 0.5);
}}
"
    )
}

/// Whether this launch's frame is one GTK draws: a Linux window on GTK's
/// Wayland backend. Read from the display GTK actually opened, not from
/// `GDK_BACKEND`, which may name X11 and fall back to Wayland (#894).
///
/// Asked of GDK on the main thread, once per launch: the backend cannot
/// change while the process lives. Not through `AppHandle::display_handle`,
/// which under X11 opens a new X connection on every call and never closes
/// it (tao 0.37) -- one per Settings visit, until the server refuses more.
/// A failed ask is not cached, and answers `false`: the desktop's frame.
pub fn available(app: &AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    {
        static ON_WAYLAND: OnceLock<bool> = OnceLock::new();
        if let Some(known) = ON_WAYLAND.get() {
            return *known;
        }
        match on_main_thread(app, |_| gtk_side::display_is_wayland()) {
            Ok(on_wayland) => *ON_WAYLAND.get_or_init(|| on_wayland),
            Err(err) => {
                tracing::warn!(?err, "could not ask GTK for its display backend");
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        false
    }
}

/// Turn the style on or off for the main window. Returns whether it was
/// on before, so a caller whose next step fails can put it back.
///
/// Waits for the main thread, where GTK lives: what the window wears is
/// settled when this returns, the contract [`set_window_chrome`] relies on
/// to persist only a frame that was really applied.
///
/// [`set_window_chrome`]: crate::commands::preferences::set_window_chrome
#[cfg(target_os = "linux")]
pub fn apply(app: &AppHandle, enabled: bool) -> AppResult<bool> {
    on_main_thread(app, move |app| gtk_side::set_enabled(app, enabled))
}

/// Record the interface's colours and repaint the frame with them if the
/// style is in use. Cosmetic by nature — the frame works whatever its
/// colour — so the interface only logs a failure.
pub fn set_colors(app: &AppHandle, colors: FrameColors) -> AppResult<()> {
    *COLORS.lock().unwrap_or_else(|p| p.into_inner()) = Some(colors);
    #[cfg(target_os = "linux")]
    {
        on_main_thread(app, |_| gtk_side::reload())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Ok(())
    }
}

/// Run `f` on the main thread and wait for its answer. Tauri runs it in
/// place when this already is the main thread, so there is no deadlock to
/// fear from either side.
///
/// **A call that times out never runs.** Its error tells the caller
/// nothing changed, and a task still queued would make that a lie later:
/// the window would take a style that neither the store nor the interface
/// knows about. So the task checks, under the lock the waiter takes to
/// give up, whether anyone is still waiting -- and sends its answer while
/// holding it, so the waiter either finds that answer or leaves the task
/// nothing to do.
#[cfg(target_os = "linux")]
fn on_main_thread<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&AppHandle) -> Result<T, String> + Send + 'static,
) -> AppResult<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(Mutex::new(false));
    let task_abandoned = Arc::clone(&abandoned);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let gone = task_abandoned.lock().unwrap_or_else(|p| p.into_inner());
        if !*gone {
            let _ = tx.send(f(&handle));
        }
    })
    .map_err(|err| AppError::Other(format!("window frame style: {err}")))?;
    let answer = match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(answer) => answer,
        Err(_) => {
            let mut gone = abandoned.lock().unwrap_or_else(|p| p.into_inner());
            match rx.try_recv() {
                // It ran between the timeout and the lock.
                Ok(answer) => answer,
                Err(_) => {
                    *gone = true;
                    return Err(AppError::Other(
                        "window frame style: the main thread did not answer in time".to_string(),
                    ));
                }
            }
        }
    };
    answer.map_err(|err| AppError::Other(format!("window frame style: {err}")))
}

#[cfg(target_os = "linux")]
mod gtk_side {
    //! The part that touches GTK, main thread only.

    use std::cell::RefCell;

    use gtk::prelude::*;
    use tauri::{AppHandle, Manager};

    use super::{stylesheet, FrameColors, COLORS, WINDOW_CLASS};

    thread_local! {
        /// Created on first use and added to the screen once; later colour
        /// changes reload it in place.
        static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    }

    pub(super) fn display_is_wayland() -> Result<bool, String> {
        let display = gtk::gdk::Display::default().ok_or("no default display")?;
        Ok(display.backend().is_wayland())
    }

    fn current_colors() -> FrameColors {
        let stored = *COLORS.lock().unwrap_or_else(|p| p.into_inner());
        stored.unwrap_or_else(|| {
            let dark =
                gtk::Settings::default().is_some_and(|s| s.is_gtk_application_prefer_dark_theme());
            FrameColors::fallback(dark)
        })
    }

    fn load(provider: &gtk::CssProvider) -> Result<(), String> {
        provider
            .load_from_data(stylesheet(current_colors()).as_bytes())
            .map_err(|err| format!("stylesheet refused: {err}"))
    }

    fn ensure_provider() -> Result<(), String> {
        PROVIDER.with(|cell| {
            if cell.borrow().is_some() {
                return Ok(());
            }
            let provider = gtk::CssProvider::new();
            load(&provider)?;
            let screen = gtk::gdk::Screen::default().ok_or("no default screen")?;
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            *cell.borrow_mut() = Some(provider);
            Ok(())
        })
    }

    pub(super) fn set_enabled(app: &AppHandle, enabled: bool) -> Result<bool, String> {
        let window = app
            .get_webview_window("main")
            .ok_or("no main window")?
            .gtk_window()
            .map_err(|err| err.to_string())?;
        let context = window.style_context();
        let was = context.has_class(WINDOW_CLASS);
        if enabled {
            // The sheet before the class: a class with no sheet behind it
            // would be a promise nothing keeps.
            ensure_provider()?;
            context.add_class(WINDOW_CLASS);
        } else {
            context.remove_class(WINDOW_CLASS);
        }
        Ok(was)
    }

    /// Repaint with the stored colours. Nothing to do while the sheet has
    /// never been needed: it is built from the stored colours when it is.
    pub(super) fn reload() -> Result<(), String> {
        PROVIDER.with(|cell| match cell.borrow().as_ref() {
            Some(provider) => load(provider),
            None => Ok(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_only_a_plain_hex_triplet() {
        assert_eq!(
            FrameColors::parse("#1a2B3c"),
            Some(FrameColors {
                background: [0x1a, 0x2b, 0x3c],
                dark: true
            })
        );
        // Everything else is refused, not escaped: the value is pasted
        // into a stylesheet.
        for bad in [
            "1a2b3c",
            "#1a2b3",
            "#1a2b3c4d",
            "#gg0000",
            "red",
            "#000000; } window { background: red",
            "",
        ] {
            assert_eq!(FrameColors::parse(bad), None, "{bad:?}");
        }
    }

    /// Read from the colour, not the theme: the pages the skins paint.
    #[test]
    fn dark_or_light_follows_the_background() {
        for (page, dark) in [
            ("#0b0b0e", true),  // Fluid
            ("#000000", true),  // Pulse
            ("#14070c", true),  // Lounge
            ("#121212", true),  // the default dark ambient
            ("#f0ebd8", false), // Editorial's paper
            ("#eceef5", false), // Liquid, light
            ("#e0e7ff", false), // an indigo-100 ambient
            ("#ffffff", false),
        ] {
            assert_eq!(
                FrameColors::parse(page).map(|c| c.dark),
                Some(dark),
                "{page}"
            );
        }
    }

    /// The promise of the module header: the file pickers, the tray menu
    /// and the other windows keep the desktop's look. Every selector has
    /// to start with the main window's class, on a client-side frame.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_rule_is_scoped_to_the_main_window() {
        let prefix = format!("window.{WINDOW_CLASS}.csd");
        for dark in [false, true] {
            let css = stylesheet(FrameColors::fallback(dark));
            let mut selectors = 0;
            for block in css.split('}').filter(|b| b.contains('{')) {
                let head = block.split('{').next().unwrap();
                for selector in head.split(',').map(str::trim) {
                    assert!(selector.starts_with(&prefix), "{selector:?}");
                    selectors += 1;
                }
            }
            assert!(selectors > 5, "the sheet should not be empty");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_bar_takes_the_colours_it_was_given() {
        let css = stylesheet(FrameColors {
            background: [18, 18, 18],
            dark: true,
        });
        assert!(css.contains("background: rgb(18, 18, 18);"));
        assert!(css.contains("color: #ffffff;"));
        let css = stylesheet(FrameColors {
            background: [224, 231, 255],
            dark: false,
        });
        assert!(css.contains("background: rgb(224, 231, 255);"));
        assert!(css.contains("color: rgba(0, 0, 6, 0.8);"));
    }
}
