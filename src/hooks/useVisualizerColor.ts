import { useCallback, useEffect, useRef, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "ui.visualizer_color";

/** Broadcast after a successful write so every mounted consumer (the
 *  immersive view's cycle button + the visualizer itself) re-reads in one go. */
export const VISUALIZER_COLOR_EVENT = "waveflow:visualizer-color";

/**
 * The selectable spectrum-visualizer colours (issue #468). `white` is the
 * default and reproduces the pre-existing look; `rainbow` tints each bar by a
 * per-index hue instead of a solid fill. The button cycles through them in
 * this order and loops back to `white`.
 */
export type VisualizerColorId =
  | "white"
  | "emerald"
  | "orange"
  | "aqua"
  | "magenta"
  | "rainbow";

/** Cycle order for the button — advancing past the last wraps to the first. */
export const VISUALIZER_COLOR_ORDER: VisualizerColorId[] = [
  "white",
  "emerald",
  "orange",
  "aqua",
  "magenta",
  "rainbow",
];

/**
 * Resolved CSS fill per solid colour. `white` keeps the historical
 * `rgba(255,255,255,0.85)` so existing users see zero change on first run;
 * `rainbow` has no entry here — it's drawn per-bar by the visualizer.
 */
export const VISUALIZER_COLOR_CSS: Record<
  Exclude<VisualizerColorId, "rainbow">,
  string
> = {
  white: "rgba(255,255,255,0.85)",
  emerald: "#10b981",
  orange: "#f97316",
  aqua: "#22d3ee",
  magenta: "#d946ef",
};

const DEFAULT_COLOR: VisualizerColorId = "white";

function parseColorId(raw: string | null): VisualizerColorId {
  if (raw != null && (VISUALIZER_COLOR_ORDER as string[]).includes(raw)) {
    return raw as VisualizerColorId;
  }
  return DEFAULT_COLOR;
}

export interface VisualizerColor {
  /** Current selection. */
  colorId: VisualizerColorId;
  /** Solid CSS fill, or `undefined` when `rainbow` (drawn per-bar). */
  color: string | undefined;
  /** Whether the current selection is the per-bar rainbow. */
  rainbow: boolean;
  /**
   * `true` only once the active profile's stored value has been read. Until
   * then `colorId` is the placeholder default; consumers must wait for this
   * before letting the user act (see `cycle`, which no-ops when not ready) so
   * an early click can't persist a default-derived value over the real one.
   */
  ready: boolean;
  /**
   * Advance to the next colour in {@link VISUALIZER_COLOR_ORDER}, wrapping.
   * No-ops until {@link VisualizerColor.ready} is `true`.
   */
  cycle: () => Promise<void>;
}

/**
 * Per-profile preference: the spectrum-visualizer bar colour (issue #468).
 * Default `white` (identical to the previous fixed look). Read by the
 * immersive now-playing surface — both the visualizer (for the fill) and the
 * cycle button (for the current label). The write machinery mirrors
 * [`useCoverSlideshow`](./useCoverSlideshow.ts) — serialized writes,
 * profile-switch guards, and rollback to the last backend-confirmed value.
 */
export function useVisualizerColor(): VisualizerColor {
  const { activeProfile } = useProfile();
  const [colorId, setColorIdState] = useState<VisualizerColorId>(DEFAULT_COLOR);
  // Which profile the stored value has been loaded for. `ready` is derived by
  // comparing it to the active profile, so a profile switch makes the hook
  // not-ready again with no synchronous setState (the render just recomputes).
  const [readyProfileId, setReadyProfileId] = useState<number | null>(null);
  const readyProfileIdRef = useRef<number | null>(null);
  const colorIdRef = useRef(colorId);
  const confirmedRef = useRef(colorId);
  const writeChainRef = useRef<Promise<void>>(Promise.resolve());
  const writeSeqRef = useRef(0);
  const activeProfileIdRef = useRef<number | null>(activeProfile?.id ?? null);
  useEffect(() => {
    colorIdRef.current = colorId;
  }, [colorId]);
  useEffect(() => {
    activeProfileIdRef.current = activeProfile?.id ?? null;
  }, [activeProfile?.id]);

  useEffect(() => {
    let cancelled = false;
    const profileId = activeProfile?.id ?? null;
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        const parsed = parseColorId(raw);
        colorIdRef.current = parsed;
        confirmedRef.current = parsed;
        setColorIdState(parsed);
        // Mark ready for THIS profile only after a successful read+parse; a
        // failure leaves the previous readiness untouched (stays not-ready on
        // first load, so `cycle` refuses until the stored value is known).
        readyProfileIdRef.current = profileId;
        setReadyProfileId(profileId);
      } catch (err) {
        console.error("[useVisualizerColor] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(VISUALIZER_COLOR_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(VISUALIZER_COLOR_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const setColorId = useCallback(async (next: VisualizerColorId) => {
    const seq = ++writeSeqRef.current;
    const profileId = activeProfileIdRef.current;
    colorIdRef.current = next;
    setColorIdState(next);
    const write = writeChainRef.current.then(async () => {
      if (activeProfileIdRef.current !== profileId) return;
      await setProfileSetting(KEY, next, "string");
      if (activeProfileIdRef.current !== profileId) return;
      confirmedRef.current = next;
    });
    writeChainRef.current = write.catch(() => undefined);
    try {
      await write;
      if (activeProfileIdRef.current !== profileId) return;
      if (seq !== writeSeqRef.current) return;
      window.dispatchEvent(new CustomEvent(VISUALIZER_COLOR_EVENT));
    } catch (err) {
      console.error("[useVisualizerColor] write failed", err);
      if (activeProfileIdRef.current !== profileId) return;
      if (seq !== writeSeqRef.current) return;
      const rollback = confirmedRef.current;
      colorIdRef.current = rollback;
      setColorIdState(rollback);
    }
  }, []);

  const cycle = useCallback(async () => {
    // Refuse until the stored value has loaded for the ACTIVE profile —
    // otherwise we'd cycle from the placeholder default and clobber the
    // persisted colour (and a stale read from the previous profile mustn't
    // authorize a write into the new one).
    if (
      readyProfileIdRef.current === null ||
      readyProfileIdRef.current !== activeProfileIdRef.current
    ) {
      return;
    }
    const idx = VISUALIZER_COLOR_ORDER.indexOf(colorIdRef.current);
    const next =
      VISUALIZER_COLOR_ORDER[(idx + 1) % VISUALIZER_COLOR_ORDER.length];
    await setColorId(next);
  }, [setColorId]);

  const rainbow = colorId === "rainbow";
  const ready =
    readyProfileId !== null && readyProfileId === (activeProfile?.id ?? null);
  return {
    colorId,
    color: rainbow ? undefined : VISUALIZER_COLOR_CSS[colorId],
    rainbow,
    ready,
    cycle,
  };
}
