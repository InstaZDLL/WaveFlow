import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { setMainWindowBounds } from "../lib/tauri/preferences";

/**
 * Persist the main window's size and position across sessions.
 *
 * Restoration happens on the Rust side before the window is made visible
 * (see the `app://ready` handler in lib.rs) so there is no visible jump
 * on launch. This hook only handles the write path: it listens to
 * `onMoved` and `onResized` events and debounces saves at 300 ms so rapid
 * drag/resize gestures do not hammer SQLite at 60 Hz.
 *
 * Mirrors the mini-player's bounds-persistence logic in MiniPlayer.tsx.
 */
export function useMainWindowBounds(): void {
  useEffect(() => {
    const win = getCurrentWindow();
    let timer: number | null = null;
    let disposed = false;

    const scheduleSave = () => {
      if (disposed) return;
      if (timer != null) window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        try {
          const scale = await win.scaleFactor();
          const pos = await win.outerPosition();
          const size = await win.outerSize();
          await setMainWindowBounds({
            x: pos.x / scale,
            y: pos.y / scale,
            width: size.width / scale,
            height: size.height / scale,
          });
        } catch (err) {
          console.error("[useMainWindowBounds] persist bounds failed", err);
        }
      }, 300);
    };

    let unlistenMoved: (() => void) | null = null;
    let unlistenResized: (() => void) | null = null;

    win
      .onMoved(scheduleSave)
      .then((fn) => {
        if (disposed) {
          fn(); // already unmounted — unlisten immediately
        } else {
          unlistenMoved = fn;
        }
      })
      .catch((err) =>
        console.error("[useMainWindowBounds] onMoved listen failed", err),
      );
    win
      .onResized(scheduleSave)
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlistenResized = fn;
        }
      })
      .catch((err) =>
        console.error("[useMainWindowBounds] onResized listen failed", err),
      );

    return () => {
      disposed = true;
      if (timer != null) window.clearTimeout(timer);
      unlistenMoved?.();
      unlistenResized?.();
    };
  }, []);
}
