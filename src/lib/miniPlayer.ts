import { Window as TauriWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

const MINI_LABEL = "mini";

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
    const win = new WebviewWindow(MINI_LABEL, {
      url: "index.html?mini=1",
      title: "WaveFlow",
      width: 320,
      height: 460,
      minWidth: 280,
      minHeight: 380,
      alwaysOnTop: true,
      decorations: true,
      resizable: true,
      center: true,
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
