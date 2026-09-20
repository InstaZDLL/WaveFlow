import { useCallback, useEffect, useRef, useState } from "react";

import {
  getWindowChrome,
  setWindowChrome,
  WINDOW_CHROME_EVENT,
  type WindowChromeState,
} from "../lib/tauri/preferences";

const FALLBACK: WindowChromeState = {
  chrome: "system",
  draw: "none",
  supported: false,
};

/**
 * First-paint cache, the same bargain the theme, skin and contrast
 * bootstraps make: the database row is the source of truth, this only
 * decides what the very first render draws.
 *
 * It matters because the backend takes the frame off **before** the window
 * is revealed, while this hook's first answer is an IPC round-trip away.
 * Without the cache, a Linux user who chose `app` gets a window revealed
 * with no frame of its own and no title bar yet — a bare rectangle for a
 * few frames. With it, only the very first launch after the choice can
 * show that, and the round-trip corrects anything stale a moment later.
 */
const CACHE_KEY = "waveflow.windowChrome";

function readCached(): WindowChromeState {
  if (typeof window === "undefined") return FALLBACK;
  try {
    const raw = window.localStorage.getItem(CACHE_KEY);
    if (!raw) return FALLBACK;
    const parsed: unknown = JSON.parse(raw);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      "draw" in parsed &&
      "chrome" in parsed &&
      "supported" in parsed
    ) {
      const state = parsed as WindowChromeState;
      // Validate rather than trust: this is page-writable storage, and a
      // bad `draw` would mean drawing a title bar over a framed window.
      const drawOk =
        state.draw === "none" ||
        state.draw === "titlebar" ||
        state.draw === "overlay";
      const chromeOk = state.chrome === "system" || state.chrome === "app";
      if (drawOk && chromeOk && typeof state.supported === "boolean") {
        return state;
      }
    }
  } catch {
    // Unavailable (private mode, quota) or unparseable. The round-trip
    // below still lands; only the first paint is degraded.
  }
  return FALLBACK;
}

function writeCached(state: WindowChromeState) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(CACHE_KEY, JSON.stringify(state));
  } catch {
    // Same bargain as above.
  }
}

export interface WindowChromePreference extends WindowChromeState {
  /** `false` until the stored choice has been read. */
  ready: boolean;
  /** A write is in flight; the surface should refuse a second click. */
  busy: boolean;
  choose: (next: "system" | "app") => Promise<void>;
}

/**
 * Who draws the frame around the window (issue #696): the desktop, or
 * WaveFlow.
 *
 * **The backend owns both halves.** `set_window_chrome` persists the choice
 * and puts it on the window in the same call, so what is stored and what is
 * on screen cannot disagree — and on macOS they could not be split anyway,
 * where the title-bar style and the title itself are two calls that must
 * move together. This hook only reads the answer and rebroadcasts it.
 *
 * `draw` is the resolved instruction, decided per platform in Rust rather
 * than by three `if (platform)` branches here: `"titlebar"` means render
 * one, `"overlay"` means leave the macOS traffic lights room over our own
 * top bar, `"none"` means the desktop already drew it.
 */
export function useWindowChrome(): WindowChromePreference {
  const [state, setState] = useState<WindowChromeState>(readCached);
  const [ready, setReady] = useState(false);
  // One write at a time. Two fast clicks would otherwise race, and the
  // answer that lands last decides the state and the broadcast -- which
  // need not be the one the user clicked last. A ref, not state: the guard
  // has to hold from inside the click handler, before React re-renders.
  const writing = useRef(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getWindowChrome()
      .then((next) => {
        if (cancelled) return;
        setState(next);
        writeCached(next);
        setReady(true);
      })
      .catch((err) => {
        console.error("[useWindowChrome] read failed", err);
        // Stay on the system frame: it is what the window already has.
        if (!cancelled) setReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Every mounted consumer re-reads from one write, the way the zoom pair
  // does — the layout draws the bar, the Settings card shows the choice.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<WindowChromeState>).detail;
      if (detail && typeof detail.draw === "string") setState(detail);
    };
    window.addEventListener(WINDOW_CHROME_EVENT, handler);
    return () => window.removeEventListener(WINDOW_CHROME_EVENT, handler);
  }, []);

  const choose = useCallback(async (next: "system" | "app") => {
    if (writing.current) return;
    writing.current = true;
    setBusy(true);
    // No optimistic update: the frame either changed or it did not, and
    // the backend's answer is the one that says which — showing a title
    // bar for a window that kept its decorations would be two frames, and
    // a failed write now returns an error rather than reporting success.
    try {
      const applied = await setWindowChrome(next);
      setState(applied);
      writeCached(applied);
      window.dispatchEvent(
        new CustomEvent<WindowChromeState>(WINDOW_CHROME_EVENT, {
          detail: applied,
        }),
      );
    } catch (err) {
      console.error("[useWindowChrome] write failed", err);
    } finally {
      writing.current = false;
      setBusy(false);
    }
  }, []);

  return { ...state, ready, busy, choose };
}
