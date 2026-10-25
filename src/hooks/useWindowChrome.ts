import { useCallback, useEffect, useRef, useState } from "react";

import {
  getWindowChrome,
  setWindowChrome,
  WINDOW_CHROME_EVENT,
  type WindowChrome,
  type WindowChromeState,
} from "../lib/tauri/preferences";
import { SETTINGS } from "../lib/settingsRegistry";

const FALLBACK: WindowChromeState = {
  chrome: SETTINGS["ui.window_chrome"].defaultValue,
  draw: "none",
  supported: false,
  options: ["system"],
};

const CHROMES: readonly WindowChrome[] = ["system", "adwaita", "app"];

const isChrome = (value: unknown): value is WindowChrome =>
  CHROMES.includes(value as WindowChrome);

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
      // A cache written before `options` existed still knows what to
      // draw, which is all the first paint needs; the card waits for the
      // round-trip before it lists anything.
      const state = {
        options: ["system"],
        ...parsed,
      } as WindowChromeState;
      // Validate rather than trust: this is page-writable storage, and a
      // bad `draw` would mean drawing a title bar over a framed window.
      const drawOk =
        state.draw === "none" ||
        state.draw === "titlebar" ||
        state.draw === "overlay";
      const optionsOk =
        Array.isArray(state.options) && state.options.every(isChrome);
      if (
        drawOk &&
        isChrome(state.chrome) &&
        optionsOk &&
        typeof state.supported === "boolean"
      ) {
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

/**
 * Apply and store a frame, then tell every consumer (the layout draws the
 * bar). Module-level so the reset of a Settings page takes the same path
 * as the card's choice. Rejects when the backend refuses the change.
 */
export async function applyWindowChrome(
  next: WindowChrome,
): Promise<WindowChromeState> {
  const applied = await setWindowChrome(next);
  writeCached(applied);
  window.dispatchEvent(
    new CustomEvent<WindowChromeState>(WINDOW_CHROME_EVENT, {
      detail: applied,
    }),
  );
  return applied;
}

export interface WindowChromePreference extends WindowChromeState {
  /** `false` until the stored choice has been read. */
  ready: boolean;
  /** A write is in flight; the surface should refuse a second click. */
  busy: boolean;
  choose: (next: WindowChrome) => Promise<void>;
}

/**
 * Who draws the frame around the window (issue #696): the desktop, the
 * desktop in a current GNOME app's style (`adwaita`, Linux on Wayland), or
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
 * top bar, `"none"` means the desktop already drew it -- restyled or not.
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
  // Read by `choose`, which is bound once and would otherwise close over
  // the first render's state.
  const stateRef = useRef(state);
  useEffect(() => {
    stateRef.current = state;
  }, [state]);
  // Bumped by every answer that is newer than the initial read: a write of
  // our own, or one another instance broadcast. Two instances are mounted
  // (the layout draws the bar, the Settings card offers the choice), so a
  // read still in flight when a choice lands would otherwise put the old
  // frame back on screen and write it to the cache. Same shape as the
  // revision counter in `useProfileScopedToggle` (#698), for the same
  // reason: comparing the VALUE is not enough, only "is my answer still
  // the current one" is.
  const revision = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const readRevision = revision.current;
    getWindowChrome()
      .then((next) => {
        if (cancelled) return;
        // Something newer landed while this was in flight; it already
        // describes the window, and this answer is history.
        if (revision.current !== readRevision) {
          setReady(true);
          return;
        }
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
      if (detail && typeof detail.draw === "string") {
        revision.current += 1;
        setState(detail);
      }
    };
    window.addEventListener(WINDOW_CHROME_EVENT, handler);
    return () => window.removeEventListener(WINDOW_CHROME_EVENT, handler);
  }, []);

  const choose = useCallback(async (next: WindowChrome) => {
    if (writing.current) return;
    // Clicking the option already shown is not a change. It matters after
    // a failed apply, where what is shown is the SESSION's answer and the
    // stored preference may still say otherwise: writing here would
    // silently replace the user's choice with the fallback they are
    // looking at.
    if (next === stateRef.current.chrome) return;
    // After the two refusals, so a click that changes nothing does not
    // throw away a read that is legitimately in flight.
    revision.current += 1;
    writing.current = true;
    setBusy(true);
    // No optimistic update: the frame either changed or it did not, and
    // the backend's answer is the one that says which — showing a title
    // bar for a window that kept its decorations would be two frames, and
    // a failed write now returns an error rather than reporting success.
    try {
      setState(await applyWindowChrome(next));
    } catch (err) {
      console.error("[useWindowChrome] write failed", err);
    } finally {
      writing.current = false;
      setBusy(false);
    }
  }, []);

  return { ...state, ready, busy, choose };
}
