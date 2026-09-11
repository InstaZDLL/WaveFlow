//! Previous / play-pause / next under the main window's taskbar thumbnail
//! (Windows, #583).
//!
//! Hovering WaveFlow's taskbar icon shows a preview of the window, and
//! `ITaskbarList3` lets an app hang up to seven buttons under it. Three
//! transport buttons there drive playback without bringing the window
//! back — the tray menu's job, one hover closer.
//!
//! The API is small. What makes it less direct than it looks:
//!
//! - **A click is a window message.** The taskbar sends `WM_COMMAND` with
//!   `THBN_CLICKED` to the window procedure, which tao owns. Tauri exposes
//!   no hook for it — tao's `msg_hook` is claimed by Tauri for menu
//!   accelerators, and only sees *posted* messages anyway — so the window
//!   is subclassed with `SetWindowSubclass`, the chain tao and
//!   `tauri-runtime-wry` already hang their own procedures on.
//! - **The toolbar can only go on once the taskbar button exists**, which
//!   the taskbar announces with the registered `TaskbarButtonCreated`
//!   message. `main` starts hidden, so that is the splash handoff.
//! - **All COM and icon work stays on the window's thread**, where the
//!   subclass procedure runs. Other threads — the `player:state` listener,
//!   the label command — only update [`Shared`] and post a refresh.
//!
//! Play/pause is one button whose icon follows `player:state`, the event
//! the in-app button follows, rather than the last click: a click the
//! engine ignores (nothing loaded) must not flip it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use tauri::{App, AppHandle, Listener, Manager};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, TaskbarList,
    THBF_ENABLED, THBN_CLICKED, THB_FLAGS, THB_ICON, THB_TOOLTIP, THUMBBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateIconIndirect, DestroyIcon, GetSystemMetrics, PostMessageW,
    RegisterWindowMessageW, HICON, ICONINFO, MSGFLT_ALLOW, SM_CXSMICON, WM_COMMAND, WM_NCDESTROY,
    WM_SETTINGCHANGE,
};

use crate::audio::{AudioEngine, PlayerState};
use crate::player_actions;

const ID_PREVIOUS: u32 = 1;
const ID_PLAY_PAUSE: u32 = 2;
const ID_NEXT: u32 = 3;

/// Our link in the window's subclass chain. A link is identified by its
/// procedure *and* this id, so it cannot collide with tao's or wry's.
const SUBCLASS_ID: usize = 583;

/// Tooltip texts. Seeded in English because the toolbar can go up before
/// the frontend has loaded i18next; the frontend then pushes the localised
/// set through `set_tray_labels`, alongside the tray menu's.
#[derive(Clone)]
pub struct Labels {
    pub previous: String,
    pub play: String,
    pub pause: String,
    pub next: String,
}

impl Default for Labels {
    fn default() -> Self {
        Self {
            previous: "Previous".into(),
            play: "Play".into(),
            pause: "Pause".into(),
            next: "Next".into(),
        }
    }
}

/// What other threads may touch. Everything else lives in [`WindowState`],
/// on the window's thread.
struct Shared {
    /// The main window, as an integer so this struct is `Send`.
    hwnd: isize,
    refresh_msg: u32,
    playing: AtomicBool,
    labels: Mutex<Labels>,
}

impl Shared {
    /// Ask the window's thread to push the current state to the toolbar.
    fn refresh(&self) {
        // Only fails once the window is gone, when there is nothing to refresh.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut c_void)),
                self.refresh_msg,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

/// Managed as Tauri state so `set_tray_labels` can reach the tooltips.
pub struct TaskbarButtons(Arc<Shared>);

impl TaskbarButtons {
    pub fn set_labels(&self, labels: Labels) {
        *self.0.labels.lock().unwrap_or_else(PoisonError::into_inner) = labels;
        self.0.refresh();
    }
}

/// Subclass the main window and start following the player state.
///
/// Must run on the thread that created the window, which `setup` does.
/// `None` when a step fails: the buttons are a convenience, and playback
/// never depends on them.
pub fn init(app: &App) -> Option<TaskbarButtons> {
    let window = app.get_webview_window("main")?;
    let hwnd = match window.hwnd() {
        // Tauri's `HWND` comes from another `windows` release; only the raw
        // handle crosses over.
        Ok(handle) => HWND(handle.0),
        Err(err) => {
            tracing::warn!(%err, "taskbar buttons: HWND lookup failed");
            return None;
        }
    };

    let (taskbar_created_msg, refresh_msg) = unsafe {
        (
            RegisterWindowMessageW(w!("TaskbarButtonCreated")),
            RegisterWindowMessageW(w!("WaveFlow.TaskbarButtons.Refresh")),
        )
    };
    if taskbar_created_msg == 0 || refresh_msg == 0 {
        tracing::warn!("taskbar buttons: RegisterWindowMessageW failed");
        return None;
    }
    // An elevated process only hears the unelevated taskbar if it lets these
    // two messages through. Without elevation this changes nothing.
    unsafe {
        let _ = ChangeWindowMessageFilterEx(hwnd, taskbar_created_msg, MSGFLT_ALLOW, None);
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COMMAND, MSGFLT_ALLOW, None);
    }

    let playing = app
        .try_state::<Arc<AudioEngine>>()
        .is_some_and(|engine| matches!(engine.shared().state(), PlayerState::Playing));
    let shared = Arc::new(Shared {
        hwnd: hwnd.0 as isize,
        refresh_msg,
        playing: AtomicBool::new(playing),
        labels: Mutex::new(Labels::default()),
    });

    let state = Box::into_raw(Box::new(WindowState {
        app: app.handle().clone(),
        shared: shared.clone(),
        taskbar_created_msg,
        taskbar: RefCell::new(None),
        added: Cell::new(false),
        icons: RefCell::new(None),
    }));
    // SAFETY: we are on the window's thread. `state` is reclaimed on
    // `WM_NCDESTROY`, or right here if the subclass never went in.
    if !unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, state as usize) }
        .as_bool()
    {
        drop(unsafe { Box::from_raw(state) });
        tracing::warn!("taskbar buttons: SetWindowSubclass failed");
        return None;
    }

    let listener = shared.clone();
    app.listen("player:state", move |event| {
        let Ok(payload) = serde_json::from_str::<StateEvent>(event.payload()) else {
            return;
        };
        let playing = match payload.state.as_str() {
            "playing" => true,
            // A brief step between two tracks; "play" would flicker in.
            "loading" => return,
            _ => false,
        };
        if listener.playing.swap(playing, Ordering::AcqRel) != playing {
            listener.refresh();
        }
    });

    Some(TaskbarButtons(shared))
}

#[derive(Deserialize)]
struct StateEvent {
    state: String,
}

/// Lives on the window's thread, reached through the subclass's reference
/// data. Only ever borrowed shared: the COM calls below can dispatch
/// messages back into [`subclass_proc`], so no `RefCell` borrow is held
/// across one.
struct WindowState {
    app: AppHandle,
    shared: Arc<Shared>,
    taskbar_created_msg: u32,
    taskbar: RefCell<Option<ITaskbarList3>>,
    /// Whether the toolbar is on the taskbar button.
    added: Cell<bool>,
    icons: RefCell<Option<Icons>>,
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the `WindowState` handed to `SetWindowSubclass`,
    // alive until `WM_NCDESTROY` frees it below.
    let state = unsafe { &*(data as *const WindowState) };
    match msg {
        WM_COMMAND if high_word(wparam) == THBN_CLICKED => {
            if state.clicked(low_word(wparam)) {
                return LRESULT(0);
            }
        }
        WM_SETTINGCHANGE => state.follow_theme(hwnd),
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
            drop(Box::from_raw(data as *mut WindowState));
        },
        _ if msg == state.taskbar_created_msg => state.add_toolbar(hwnd),
        _ if msg == state.shared.refresh_msg => {
            state.update_toolbar(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn low_word(wparam: WPARAM) -> u32 {
    (wparam.0 & 0xFFFF) as u32
}

fn high_word(wparam: WPARAM) -> u32 {
    ((wparam.0 >> 16) & 0xFFFF) as u32
}

impl WindowState {
    /// `TaskbarButtonCreated`: the button exists, put the toolbar on it.
    fn add_toolbar(&self, hwnd: HWND) {
        let Some(taskbar) = self.taskbar() else {
            return;
        };
        match unsafe { taskbar.ThumbBarAddButtons(hwnd, &self.buttons()) } {
            Ok(()) => self.added.set(true),
            Err(err) => tracing::warn!(%err, "taskbar buttons: ThumbBarAddButtons failed"),
        }
    }

    /// Push the current state and labels to the toolbar, if it is up.
    fn update_toolbar(&self, hwnd: HWND) {
        // Not up yet: `add_toolbar` reads the current state itself.
        if !self.added.get() {
            return;
        }
        let Some(taskbar) = self.taskbar() else {
            return;
        };
        if let Err(err) = unsafe { taskbar.ThumbBarUpdateButtons(hwnd, &self.buttons()) } {
            tracing::warn!(%err, "taskbar buttons: ThumbBarUpdateButtons failed");
        }
    }

    /// Returns whether `id` was one of ours.
    fn clicked(&self, id: u32) -> bool {
        match id {
            ID_PREVIOUS => {
                let app = self.app.clone();
                tauri::async_runtime::spawn(async move {
                    player_actions::previous(&app, "taskbar").await;
                });
            }
            ID_PLAY_PAUSE => player_actions::toggle_play_pause(&self.app, "taskbar"),
            ID_NEXT => {
                let app = self.app.clone();
                tauri::async_runtime::spawn(async move {
                    player_actions::next(&app, "taskbar").await;
                });
            }
            _ => return false,
        }
        true
    }

    /// Redraw the glyphs when the taskbar switched between light and dark.
    fn follow_theme(&self, hwnd: HWND) {
        let light = taskbar_is_light();
        let stale = self
            .icons
            .borrow()
            .as_ref()
            .is_some_and(|icons| icons.light != light);
        if !stale {
            return;
        }
        // The old icons stay alive until the toolbar holds the new ones.
        let old = self.icons.replace(Icons::new(light));
        self.update_toolbar(hwnd);
        drop(old);
    }

    fn taskbar(&self) -> Option<ITaskbarList3> {
        if let Some(taskbar) = self.taskbar.borrow().as_ref() {
            return Some(taskbar.clone());
        }
        let created = unsafe {
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        }
        .and_then(|taskbar| unsafe { taskbar.HrInit() }.map(|()| taskbar));
        match created {
            Ok(taskbar) => {
                *self.taskbar.borrow_mut() = Some(taskbar.clone());
                Some(taskbar)
            }
            Err(err) => {
                tracing::warn!(%err, "taskbar buttons: ITaskbarList3 unavailable");
                None
            }
        }
    }

    fn buttons(&self) -> [THUMBBUTTON; 3] {
        let icons = self.icon_set();
        let labels = self
            .shared
            .labels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (play_pause_icon, play_pause_tip) = if self.shared.playing.load(Ordering::Acquire) {
            (icons.pause, &labels.pause)
        } else {
            (icons.play, &labels.play)
        };
        [
            button(ID_PREVIOUS, icons.previous, &labels.previous),
            button(ID_PLAY_PAUSE, play_pause_icon, play_pause_tip),
            button(ID_NEXT, icons.next, &labels.next),
        ]
    }

    fn icon_set(&self) -> IconSet {
        if let Some(icons) = self.icons.borrow().as_ref() {
            return icons.set;
        }
        let icons = Icons::new(taskbar_is_light());
        let set = icons
            .as_ref()
            .map_or_else(IconSet::default, |icons| icons.set);
        *self.icons.borrow_mut() = icons;
        set
    }
}

fn button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
    let mut button = THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        hIcon: icon,
        dwFlags: THBF_ENABLED,
        ..Default::default()
    };
    // A fixed 260-unit buffer that must stay NUL-terminated.
    for (slot, unit) in button.szTip.iter_mut().zip(tip.encode_utf16().take(259)) {
        *slot = unit;
    }
    button
}

#[derive(Clone, Copy, Default)]
struct IconSet {
    previous: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
}

/// The four glyphs, drawn for one taskbar theme and destroyed with it.
struct Icons {
    light: bool,
    set: IconSet,
}

impl Icons {
    fn new(light: bool) -> Option<Self> {
        // Thumbnail buttons take small-icon metrics, which follow the
        // display scale.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as u32;
        let rgb = if light {
            [0x1f, 0x1f, 0x1f]
        } else {
            [0xff, 0xff, 0xff]
        };
        // An early return drops `icons`, destroying the ones already made.
        let mut icons = Self {
            light,
            set: IconSet::default(),
        };
        icons.set.previous = make_icon(Glyph::Previous, size, rgb)?;
        icons.set.play = make_icon(Glyph::Play, size, rgb)?;
        icons.set.pause = make_icon(Glyph::Pause, size, rgb)?;
        icons.set.next = make_icon(Glyph::Next, size, rgb)?;
        Some(icons)
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        let IconSet {
            previous,
            play,
            pause,
            next,
        } = self.set;
        for icon in [previous, play, pause, next] {
            if !icon.is_invalid() {
                let _ = unsafe { DestroyIcon(icon) };
            }
        }
    }
}

/// Whether the taskbar, and the thumbnail flyout above it, is light. It
/// follows Windows' own mode rather than the apps' one; the value is
/// missing on builds that predate the light taskbar, which was dark.
fn taskbar_is_light() -> bool {
    let mut value = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut len),
        )
    };
    status.is_ok() && value != 0
}

#[derive(Clone, Copy)]
enum Glyph {
    Previous,
    Play,
    Pause,
    Next,
}

fn make_icon(glyph: Glyph, size: u32, rgb: [u8; 3]) -> Option<HICON> {
    let pixels = render_glyph(glyph, size, rgb)?;
    match icon_from_bgra(size, &pixels) {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(%err, "taskbar buttons: icon creation failed");
            None
        }
    }
}

/// Draw `glyph` in `rgb` as straight-alpha BGRA rows, top row first.
///
/// Shapes are laid out on a 16-unit grid — the size the taskbar asks for at
/// 100 % scale, where every edge lands on a whole pixel — then scaled.
fn render_glyph(glyph: Glyph, size: u32, [r, g, b]: [u8; 3]) -> Option<Vec<u8>> {
    let mut path = PathBuilder::new();
    match glyph {
        Glyph::Previous => {
            path.push_rect(Rect::from_xywh(3.0, 3.0, 2.0, 10.0)?);
            push_triangle(&mut path, [(13.0, 3.0), (6.0, 8.0), (13.0, 13.0)]);
        }
        Glyph::Play => push_triangle(&mut path, [(5.0, 3.0), (13.0, 8.0), (5.0, 13.0)]),
        Glyph::Pause => {
            path.push_rect(Rect::from_xywh(4.0, 3.0, 3.0, 10.0)?);
            path.push_rect(Rect::from_xywh(9.0, 3.0, 3.0, 10.0)?);
        }
        Glyph::Next => {
            push_triangle(&mut path, [(3.0, 3.0), (10.0, 8.0), (3.0, 13.0)]);
            path.push_rect(Rect::from_xywh(11.0, 3.0, 2.0, 10.0)?);
        }
    }
    let path = path.finish()?;

    let mut pixmap = Pixmap::new(size, size)?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    let scale = size as f32 / 16.0;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::from_scale(scale, scale),
        None,
    );
    Some(
        pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let color = pixel.demultiply();
                [color.blue(), color.green(), color.red(), color.alpha()]
            })
            .collect(),
    )
}

fn push_triangle(path: &mut PathBuilder, [(x0, y0), (x1, y1), (x2, y2)]: [(f32, f32); 3]) {
    path.move_to(x0, y0);
    path.line_to(x1, y1);
    path.line_to(x2, y2);
    path.close();
}

/// A 32-bit icon from straight-alpha BGRA rows, top row first.
fn icon_from_bgra(size: u32, bgra: &[u8]) -> windows::core::Result<HICON> {
    let side = size as i32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side,
            // Negative: rows run top-down, the order `bgra` is in.
            biHeight: -side,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let color = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }?;
    // The alpha channel carries the shape. The AND mask is still required,
    // and must be clear; monochrome rows are padded to 16 bits.
    let mask_bits = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
    let mask = unsafe { CreateBitmap(side, side, 1, 1, Some(mask_bits.as_ptr().cast())) };
    let icon = if bits.is_null() || mask.is_invalid() {
        Err(windows::core::Error::from_thread())
    } else {
        let len = bgra.len().min(size as usize * size as usize * 4);
        unsafe {
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), len);
            CreateIconIndirect(&ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            })
        }
    };
    // `CreateIconIndirect` copies both bitmaps.
    unsafe {
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
    }
    icon
}
