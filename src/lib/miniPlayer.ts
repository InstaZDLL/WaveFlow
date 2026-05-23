import {
  Window as TauriWindow,
  availableMonitors,
  currentMonitor,
} from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  getMiniPlayerBounds,
  type MiniPlayerBounds,
} from "./tauri/preferences";

const MINI_LABEL = "mini";
const DEFAULT_WIDTH = 280;
const DEFAULT_HEIGHT = 380;
const MIN_WIDTH = 240;
const MIN_HEIGHT = 320;
/** Margin from the screen edge (logical pixels). Matches Spotify's
 *  bottom-right offset, leaving room for the Windows taskbar / macOS
 *  Dock. Smaller offsets feel cramped against the corner. */
const EDGE_MARGIN = 24;
/** Minimum overlap (logical pixels) the saved window must keep with
 *  at least one monitor before we trust the position. A few pixels
 *  is enough — we just need to prevent restoring fully off-screen
 *  after a monitor disconnect or resolution change. */
const MIN_VISIBLE_OVERLAP = 80;

/**
 * Does the proposed bounds rectangle intersect any of the available
 * monitors by at least `MIN_VISIBLE_OVERLAP` on both axes? Used to
 * sanity-check restored positions — Windows is happy to place a
 * window at (1800, 200) even if the secondary monitor that owned
 * those coordinates has since been unplugged.
 */
async function boundsAreVisible(bounds: MiniPlayerBounds): Promise<boolean> {
  try {
    const monitors = await availableMonitors();
    for (const m of monitors) {
      const scale = m.scaleFactor || 1;
      // Convert monitor physical rect to logical so it matches the
      // logical-pixel bounds we persist.
      const mx = m.position.x / scale;
      const my = m.position.y / scale;
      const mw = m.size.width / scale;
      const mh = m.size.height / scale;
      const overlapX =
        Math.min(bounds.x + bounds.width, mx + mw) - Math.max(bounds.x, mx);
      const overlapY =
        Math.min(bounds.y + bounds.height, my + mh) - Math.max(bounds.y, my);
      if (overlapX >= MIN_VISIBLE_OVERLAP && overlapY >= MIN_VISIBLE_OVERLAP) {
        return true;
      }
    }
  } catch (err) {
    console.warn("[miniPlayer] availableMonitors query failed", err);
  }
  return false;
}

/**
 * Open the always-on-top mini-player window. If it already exists,
 * just bring it to the front instead of creating a duplicate. Hides
 * the main window so the user gets a clean swap.
 *
 * The mini-player loads the same bundle with `?mini=1` so
 * [`main.tsx`] can boot into a stripped-down provider tree.
 *
 * Position + size are restored from `app_setting['mini_player.bounds']`
 * when the saved rectangle still intersects an available monitor; we
 * fall back to a Spotify-style bottom-right corner anchor otherwise
 * (monitor disconnected, resolution change, or first launch).
 */
export async function openMiniPlayer(): Promise<void> {
  const existing = await TauriWindow.getByLabel(MINI_LABEL);
  if (existing) {
    await existing.show();
    await existing.unminimize();
    await existing.setFocus();
  } else {
    let x: number | undefined;
    let y: number | undefined;
    let width = DEFAULT_WIDTH;
    let height = DEFAULT_HEIGHT;

    // 1) Try the persisted bounds, but only if they still land on a
    //    real monitor. Stale coordinates (laptop docked at the office,
    //    undocked at home) silently fall through to the corner anchor.
    try {
      const saved = await getMiniPlayerBounds();
      if (saved && (await boundsAreVisible(saved))) {
        x = Math.round(saved.x);
        y = Math.round(saved.y);
        width = Math.max(MIN_WIDTH, Math.round(saved.width));
        height = Math.max(MIN_HEIGHT, Math.round(saved.height));
      }
    } catch (err) {
      console.warn("[miniPlayer] restore bounds failed", err);
    }

    // 2) No usable saved position → anchor to the bottom-right of the
    //    current monitor (Spotify-style default). `monitor.position`
    //    is the logical origin of the monitor in the virtual desktop
    //    space — non-zero on secondary monitors and negative when a
    //    monitor is placed to the left of (or above) the primary — so
    //    we must add it to land on the right monitor instead of
    //    snapping to the primary's bottom-right corner.
    if (x == null || y == null) {
      try {
        const monitor = await currentMonitor();
        if (monitor) {
          const scale = monitor.scaleFactor || 1;
          const logicalX = monitor.position.x / scale;
          const logicalY = monitor.position.y / scale;
          const logicalW = monitor.size.width / scale;
          const logicalH = monitor.size.height / scale;
          x = Math.round(logicalX + logicalW - width - EDGE_MARGIN);
          y = Math.round(logicalY + logicalH - height - EDGE_MARGIN);
        }
      } catch (err) {
        console.warn(
          "[miniPlayer] monitor query failed, falling back to centered",
          err,
        );
      }
    }

    const win = new WebviewWindow(MINI_LABEL, {
      url: "index.html?mini=1",
      title: "WaveFlow",
      width,
      height,
      minWidth: MIN_WIDTH,
      minHeight: MIN_HEIGHT,
      alwaysOnTop: true,
      // We render our own title bar (pin / drag-dots / close) inside
      // the webview so the widget stays compact and on-brand.
      decorations: false,
      transparent: false,
      resizable: true,
      x,
      y,
      // Only center when we couldn't compute a corner position.
      center: x == null || y == null,
      skipTaskbar: false,
    });
    // Surface init errors via the standard event channel — `new` is
    // sync but the underlying create is async on the Rust side.
    await new Promise<void>((resolve, reject) => {
      win.once("tauri://created", () => resolve());
      win.once("tauri://error", (e) => reject(e.payload));
    });
  }

  // Hide the main window so we don't have two players visible at
  // once — the mini-player has a Maximize button to restore it.
  const main = await TauriWindow.getByLabel("main");
  if (main) await main.hide();
}
