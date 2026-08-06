import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

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
  "white" | "emerald" | "orange" | "aqua" | "magenta" | "rainbow";

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
 * cycle button (for the current label).
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useVisualizerColor(): VisualizerColor {
  const {
    value: colorId,
    ready,
    setValue,
  } = useProfileSetting<VisualizerColorId>({
    key: KEY,
    defaultValue: DEFAULT_COLOR,
    parse: parseColorId,
    serialize: (value) => value,
    valueType: "string",
    event: VISUALIZER_COLOR_EVENT,
    label: "useVisualizerColor",
  });

  const cycle = useCallback(async () => {
    // Refuse until the stored value has loaded for the ACTIVE profile —
    // otherwise we'd cycle from the placeholder default and clobber the
    // persisted colour.
    if (!ready) return;
    // Functional update: the shared hook hands us the synchronously-current
    // value, so back-to-back clicks advance one step each instead of both
    // computing from the same render-lagged snapshot.
    await setValue((previous) => {
      const idx = VISUALIZER_COLOR_ORDER.indexOf(previous);
      return VISUALIZER_COLOR_ORDER[(idx + 1) % VISUALIZER_COLOR_ORDER.length];
    });
  }, [ready, setValue]);

  const rainbow = colorId === "rainbow";
  return {
    colorId,
    color: rainbow ? undefined : VISUALIZER_COLOR_CSS[colorId],
    rainbow,
    ready,
    cycle,
  };
}
