import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

import {
  getUiZoom,
  setUiZoom,
  UI_ZOOM_MAX,
  UI_ZOOM_MIN,
  UI_ZOOM_STEP,
  UI_ZOOM_CHANGED_EVENT,
} from "../lib/tauri/preferences";

/**
 * Hydrate the persisted UI zoom level on mount, apply it through
 * Tauri's `setZoom` so the WebView scales natively (text stays crisp
 * — this is not a CSS `transform: scale`), and listen for the global
 * keyboard shortcuts `Ctrl+=` / `Ctrl+-` / `Ctrl+0` so power users can
 * tune density without opening Settings.
 *
 * Mounted once at the AppLayout level. The Settings card reads its
 * own state independently and rebroadcasts via
 * [`UI_ZOOM_CHANGED_EVENT`] whenever the user nudges the slider, so
 * the two surfaces stay in sync.
 */
export function useUiZoom() {
  const [zoom, setZoomState] = useState(1);

  // Initial hydration: read once from app_setting, apply to the
  // webview, mirror in state. Any failure leaves the default 1.0
  // zoom so the app stays usable.
  useEffect(() => {
    let cancelled = false;
    getUiZoom()
      .then(async (z) => {
        if (cancelled) return;
        setZoomState(z);
        try {
          await getCurrentWebviewWindow().setZoom(z);
        } catch (err) {
          console.error("[useUiZoom] setZoom failed", err);
        }
      })
      .catch((err) => console.error("[useUiZoom] getUiZoom failed", err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Keep in sync with the Settings card (or any other surface) that
  // rebroadcasts a zoom change. The event carries the new level as
  // `detail` so we don't re-fetch from the backend on every tick.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<number>).detail;
      if (typeof detail === "number") setZoomState(detail);
    };
    window.addEventListener(UI_ZOOM_CHANGED_EVENT, handler);
    return () => window.removeEventListener(UI_ZOOM_CHANGED_EVENT, handler);
  }, []);

  // Keyboard shortcuts. Bound at the window level so they work
  // regardless of which view has focus. `Ctrl+=` (often the same
  // physical key as `Ctrl++`) zooms in, `Ctrl+-` zooms out, `Ctrl+0`
  // resets to 100 %. Mirrors VS Code / Discord / Slack / browser
  // conventions.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Don't fight an input — let typing land normally.
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }
      if (!(e.ctrlKey || e.metaKey)) return;
      let next: number | null = null;
      if (e.key === "+" || e.key === "=") {
        next = clamp(zoom + UI_ZOOM_STEP);
      } else if (e.key === "-" || e.key === "_") {
        next = clamp(zoom - UI_ZOOM_STEP);
      } else if (e.key === "0") {
        next = 1;
      }
      if (next == null) return;
      e.preventDefault();
      apply(next).catch((err) =>
        console.error("[useUiZoom] shortcut apply failed", err),
      );
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [zoom]);

  return zoom;
}

/**
 * Imperative setter exported so the Settings slider can drive the
 * zoom directly (rather than dispatching through a state mutation
 * chain). Clamps, calls Tauri's `setZoom`, persists, and broadcasts.
 */
export async function applyUiZoom(zoom: number): Promise<number> {
  return apply(clamp(zoom));
}

function clamp(v: number): number {
  if (!Number.isFinite(v)) return 1;
  // Round to one decimal so a long chain of `+0.1` doesn't drift to
  // `0.99999…` floating-point noise that the user can't easily reset
  // to 1.0 by eye.
  const stepped = Math.round(v * 10) / 10;
  return Math.min(UI_ZOOM_MAX, Math.max(UI_ZOOM_MIN, stepped));
}

async function apply(zoom: number): Promise<number> {
  await getCurrentWebviewWindow().setZoom(zoom);
  await setUiZoom(zoom);
  window.dispatchEvent(
    new CustomEvent<number>(UI_ZOOM_CHANGED_EVENT, { detail: zoom }),
  );
  return zoom;
}
