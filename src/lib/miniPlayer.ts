import { Window as TauriWindow, currentMonitor } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

const MINI_LABEL = "mini";
const WIDTH = 280;
const HEIGHT = 380;
/** Margin from the screen edge (logical pixels). Matches Spotify's
 *  bottom-right offset, leaving room for the Windows taskbar / macOS
 *  Dock. Smaller offsets feel cramped against the corner. */
const EDGE_MARGIN = 24;

/**
 * Open the always-on-top mini-player window. If it already exists,
 * just bring it to the front instead of creating a duplicate. Hides
 * the main window so the user gets a clean swap.
 *
 * The mini-player loads the same bundle with `?mini=1` so
 * [`main.tsx`] can boot into a stripped-down provider tree.
 */
export async function openMiniPlayer(): Promise<void> {
  const existing = await TauriWindow.getByLabel(MINI_LABEL);
  if (existing) {
    await existing.show();
    await existing.unminimize();
    await existing.setFocus();
  } else {
    // The URL is resolved by the Tauri bundler — `index.html` is the
    // single entry point Vite produces; the search param is what the
    // frontend branches on.
    // Anchor to the bottom-right of the primary monitor — Spotify
    // does the same, and it's the corner least likely to overlap the
    // user's active work area. `currentMonitor` returns physical size
    // + scale; convert back to logical pixels for `x` / `y`.
    let x: number | undefined;
    let y: number | undefined;
    try {
      const monitor = await currentMonitor();
      if (monitor) {
        const scale = monitor.scaleFactor || 1;
        const logicalW = monitor.size.width / scale;
        const logicalH = monitor.size.height / scale;
        x = Math.max(0, Math.round(logicalW - WIDTH - EDGE_MARGIN));
        y = Math.max(0, Math.round(logicalH - HEIGHT - EDGE_MARGIN));
      }
    } catch (err) {
      console.warn(
        "[miniPlayer] monitor query failed, falling back to centered",
        err,
      );
    }

    const win = new WebviewWindow(MINI_LABEL, {
      url: "index.html?mini=1",
      title: "WaveFlow",
      width: WIDTH,
      height: HEIGHT,
      minWidth: 240,
      minHeight: 320,
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
