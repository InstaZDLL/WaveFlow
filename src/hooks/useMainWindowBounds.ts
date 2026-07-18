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
 * Structurally close to the mini-player's bounds persistence
 * (MiniPlayer.tsx), but deliberately saves `innerSize` rather than
 * `outerSize`: the main window carries OS decorations while the mini is
 * `decorations: false` (inner === outer), so only this window needs the
 * inner/outer distinction to round-trip through `set_size`. See #379.
 *
 * Only a window in the NORMAL state is persisted — maximized, minimized
 * and fullscreen geometries are skipped, so what comes back on the next
 * launch is always the last real user-sized window.
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
          // Only persist a NORMAL-state window. While maximized the
          // geometry describes the full work area, so restoring it on the
          // next launch reopens the window at maximized *size* without the
          // maximized *state* — a window that looks maximized but isn't,
          // and that can no longer be restored to its previous size.
          // Minimized is worse: Windows reports an off-screen position
          // (typically -32000), which would park the window outside every
          // monitor. Fullscreen has the same problem as maximized.
          // Skipping the write keeps the last known normal geometry, which
          // is what the user gets back.
          const [maximized, minimized, fullscreen] = await Promise.all([
            win.isMaximized(),
            win.isMinimized(),
            win.isFullscreen(),
          ]);
          if (maximized || minimized || fullscreen) return;

          const scale = await win.scaleFactor();
          const pos = await win.outerPosition();
          // Save the INNER (client-area) size, not the outer size. The
          // Rust restore path uses `window.set_size(LogicalSize)`, which
          // in Tauri 2 sets the inner size. The main window has OS
          // decorations (title bar + borders), so persisting `outerSize`
          // here and restoring it as the inner size grew the window by
          // one title-bar height (down) + border width (right) on every
          // launch (issue #379). Position stays on `outerPosition` /
          // `set_position`, which are already an outer↔outer pair.
          const size = await win.innerSize();
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
