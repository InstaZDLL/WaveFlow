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
