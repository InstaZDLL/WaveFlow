import { useSyncExternalStore } from "react";

/**
 * The playback position, kept outside `PlayerContext`.
 *
 * The backend emits `player:position` four times a second. While the
 * position lived in the context value, every one of those ticks re-rendered
 * each of the context's consumers — the whole library view among them — to
 * redraw what a handful of components show. Here only the components that
 * call {@link usePlayerPosition} re-render on a tick; everything else that
 * reads the player sees a context that changes when the player does.
 *
 * Module state, one per webview: the main window, the mini-player and the
 * lyrics overlay each run their own `PlayerProvider`, which is the only
 * writer of its window's copy.
 */
let positionMs = 0;
const listeners = new Set<() => void>();

/** Written by `PlayerProvider` only: the backend's ticks, seeks, resets. */
export function setPlayerPosition(ms: number): void {
  if (ms === positionMs) return;
  positionMs = ms;
  for (const listener of listeners) listener();
}

/** The position now, for code that needs it in a handler, not a render. */
export function getPlayerPosition(): number {
  return positionMs;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The playback position in ms; re-renders the caller on every tick. */
export function usePlayerPosition(): number {
  return useSyncExternalStore(subscribe, getPlayerPosition);
}
