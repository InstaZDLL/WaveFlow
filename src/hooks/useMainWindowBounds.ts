import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { setMainWindowBounds } from "../lib/tauri/preferences";
import { mainWindowBoundsWritesSuppressed } from "../lib/mainWindowBoundsGuard";

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
    // Incremented on every new scheduled save and on cleanup. Any in-flight
    // async save whose generation doesn't match the current value is stale
    // (either superseded by a newer gesture or invalidated by unmount) and
    // must not call setMainWindowBounds.
    let saveGeneration = 0;
    // Promise chain that serializes writes so a slow preceding IPC call
    // cannot overwrite a newer one that already completed.
    let writeChain: Promise<void> = Promise.resolve();

    const scheduleSave = () => {
      if (disposed) return;
      const generation = ++saveGeneration;
      if (timer != null) window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        try {
          const scale = await win.scaleFactor();
          const pos = await win.outerPosition();
          const size = await win.outerSize();
          // Re-check after awaits: unmount or a later gesture may have
          // invalidated this save while the async calls were in flight.
          if (disposed || generation !== saveGeneration) return;
          const bounds = {
            x: pos.x / scale,
            y: pos.y / scale,
            width: size.width / scale,
            height: size.height / scale,
          };
          // Enqueue the write so concurrent saves cannot complete out of
          // order and persist stale bounds (single-threaded JS allows two
          // in-flight IPC calls to resolve in unpredictable order).
          writeChain = writeChain
            .catch(() => {})
            .then(() => {
              if (disposed || generation !== saveGeneration) return;
              // A manual "Reset window position" opens a short suppression
              // window before deleting the row; drop the write so this
              // pending debounced save can't resurrect the reset bounds.
              if (mainWindowBoundsWritesSuppressed()) return;
              return setMainWindowBounds(bounds).catch((err: unknown) => {
                console.error(
                  "[useMainWindowBounds] persist bounds failed",
                  err,
                );
              });
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
      saveGeneration += 1; // invalidate any in-flight save
      writeChain = Promise.resolve(); // reset chain so no queued write can fire
      if (timer != null) window.clearTimeout(timer);
      unlistenMoved?.();
      unlistenResized?.();
    };
  }, []);
}
