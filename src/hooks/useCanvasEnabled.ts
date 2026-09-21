import { useSyncExternalStore } from "react";

/**
 * Global "Show Canvas" preference (issue #442) — the Spotify-style toggle
 * that reveals/hides the looping Canvas behind the now-playing view. It's a
 * pure display preference (no library data), so it lives in `localStorage`
 * rather than a per-profile DB setting, shared reactively across every
 * surface (immersive top bar + NowPlayingPanel) through a tiny external
 * store. Default OFF: the static cover shows first, and the clip only takes
 * over once the user clicks "Show Canvas".
 */
const STORAGE_KEY = "waveflow.canvas.show";

function read(): boolean {
  try {
    // Default OFF: only an explicit "true" turns it on.
    return localStorage.getItem(STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

let enabled = read();
const listeners = new Set<() => void>();

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

// The mini-player is a second webview on the same origin, so it shares
// this `localStorage` but not the in-memory copy above: without this, a
// toggle in the main window would not reach an open mini-player until it
// was reopened (#717). `storage` fires in every OTHER window of the origin,
// never in the one that wrote, so this cannot loop.
if (typeof window !== "undefined") {
  window.addEventListener("storage", (event) => {
    if (event.key !== STORAGE_KEY) return;
    const next = event.newValue === "true";
    if (next === enabled) return;
    enabled = next;
    for (const cb of listeners) cb();
  });
}

function getSnapshot(): boolean {
  return enabled;
}

/** Flip the global Show-Canvas preference and notify every subscriber. */
export function setCanvasEnabled(next: boolean): void {
  if (next === enabled) return;
  enabled = next;
  try {
    localStorage.setItem(STORAGE_KEY, next ? "true" : "false");
  } catch {
    // Private-mode / quota failure — keep the in-memory value so the toggle
    // still works this session.
  }
  for (const cb of listeners) cb();
}

/** Reactive read of the global Show-Canvas preference. */
export function useCanvasEnabled(): boolean {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
