import { useCallback, useSyncExternalStore } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

/** Per-profile `profile_setting` key gating every Hi-Res / DSD pill. */
const KEY = "ui.show_hi_res_badge";

/**
 * Window event broadcast by the Settings toggle after a write so every
 * mounted `HiResBadge` + the player-bar quality label re-read in one
 * go. Also re-fired by other callers (tests, future profile-switch
 * code) when the underlying setting changes outside this module.
 */
export const HI_RES_BADGE_EVENT = "waveflow:hi-res-badge-visibility";

const DEFAULT_VISIBLE = true;

function parseVisible(raw: string | null): boolean {
  if (raw == null) return DEFAULT_VISIBLE;
  return raw !== "false" && raw !== "0";
}

// ─── Module-level store ───────────────────────────────────────────────
//
// The previous implementation called `useEffect` with its own window
// listener + Tauri fetch inside every `HiResBadge` instance. On a
// virtualised library view ~20-50 badges are mounted at once → 20-50
// `getProfileSetting` calls plus 20-50 listeners on the window event
// for what is really a single boolean. The shared store collapses that
// to one Tauri fetch on the first subscriber + one window listener
// attached lazily, with React-level subscriptions handled through
// `useSyncExternalStore` (cheap — just adds a callback to a Set).

let currentVisible: boolean = DEFAULT_VISIBLE;
let hydrated = false;
let windowListenerAttached = false;
// Monotonic token for in-flight backend reads. Bumped before each
// fetch; the resolver only commits if its captured token still
// matches — protects against an older async response overwriting a
// newer one if two `hydrateFromBackend` calls race (e.g. settings
// toggle dispatching the window event right next to a profile-switch
// `refreshHiResBadgeVisibility`).
let hydrateToken = 0;
const listeners: Set<() => void> = new Set();

function notify(): void {
  for (const listener of listeners) listener();
}

async function hydrateFromBackend(): Promise<void> {
  const token = ++hydrateToken;
  try {
    const raw = await getProfileSetting(KEY);
    if (token !== hydrateToken) return; // a newer read superseded us
    const next = parseVisible(raw);
    if (next !== currentVisible) {
      currentVisible = next;
      notify();
    }
  } catch (err) {
    console.error("[useHiResBadgeVisibility] read failed", err);
  }
}

function ensureWindowListener(): void {
  if (windowListenerAttached || typeof window === "undefined") return;
  windowListenerAttached = true;
  window.addEventListener(HI_RES_BADGE_EVENT, () => {
    void hydrateFromBackend();
  });
}

function subscribe(listener: () => void): () => void {
  ensureWindowListener();
  if (!hydrated) {
    // First subscriber kicks off the one-time backend read. Marked
    // hydrated even before the fetch resolves so subsequent
    // subscribers don't fire a duplicate request — they'll get the
    // value via `notify` once the first fetch lands.
    hydrated = true;
    void hydrateFromBackend();
  }
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot(): boolean {
  return currentVisible;
}

// ─── Test / profile-switch hook ───────────────────────────────────────
//
// Exported so that `ProfileContext` (or unit tests) can force a re-
// hydrate when the active profile changes — `profile_setting` is
// scoped per profile, so the cached `currentVisible` becomes stale
// the moment the user switches.

/** Force the module-level store to re-read from the backend. */
export function refreshHiResBadgeVisibility(): void {
  hydrated = true;
  void hydrateFromBackend();
}

export interface HiResBadgeVisibility {
  visible: boolean;
  setVisible: (next: boolean) => Promise<void>;
}

/**
 * Hook returning the user's preference for the Hi-Res / DSD pill that
 * decorates track lists, album grids, and the player bar. Default
 * **on** because the badge is part of WaveFlow's audiophile identity;
 * the toggle lets users who find it noisy turn it off in one click.
 *
 * Backed by `profile_setting['ui.show_hi_res_badge']` (per-profile so
 * a kid's profile can stay clean while the audiophile profile keeps
 * the pills) and a module-level cache so a virtualised view with
 * dozens of mounted badges only triggers one Tauri fetch + one window
 * listener.
 */
export function useHiResBadgeVisibility(): HiResBadgeVisibility {
  const visible = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);

  const setVisible = useCallback(async (next: boolean) => {
    const previous = currentVisible;
    if (next === previous) return;
    // Optimistic update — flips every mounted consumer immediately.
    currentVisible = next;
    notify();
    try {
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      // Broadcast for other tabs / mini-player webview so they
      // re-hydrate from the same store. Guarded so callers running
      // in a non-DOM environment (Node tests, future SSR) don't
      // crash here — `ensureWindowListener` applies the same guard.
      if (typeof window !== "undefined") {
        window.dispatchEvent(new CustomEvent(HI_RES_BADGE_EVENT));
      }
    } catch (err) {
      console.error("[useHiResBadgeVisibility] write failed", err);
      // Roll back so the UI stays consistent with the persisted
      // setting on failure (Tauri command rejected, profile pool
      // unavailable, etc.).
      currentVisible = previous;
      notify();
    }
  }, []);

  return { visible, setVisible };
}
