import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "ui.visualizer_style";

/** Broadcast after a successful write so every mounted consumer (the
 *  immersive view's cycle button + the visualizer itself) re-reads in one go. */
export const VISUALIZER_STYLE_EVENT = "waveflow:visualizer-style";

/**
 * How the spectrum visualizer draws its 48 bands (issue #699).
 *
 * - `wave`: one smooth curve through the band tops, filled underneath.
 * - `mirror`: that curve reflected around a centre line, so it reads as a
 *   waveform rather than a histogram.
 * - `bars`: the original rectangles, kept for whoever prefers them.
 */
export type VisualizerStyleId = "wave" | "mirror" | "bars";

/** Cycle order for the button — advancing past the last wraps to the first. */
export const VISUALIZER_STYLE_ORDER: VisualizerStyleId[] = [
  "wave",
  "mirror",
  "bars",
];

/**
 * `wave` rather than `bars`, which means the look changes under existing
 * users on upgrade. Deliberate: the complaint in #699 is the bars
 * themselves ("plain rectangles going up and down… dated next to the rest
 * of the immersive view"), so keeping them as the default would ship the
 * fix switched off. `bars` is one click away on the same button.
 */
const DEFAULT_STYLE: VisualizerStyleId = "wave";

function parseStyleId(raw: string | null): VisualizerStyleId {
  if (raw != null && (VISUALIZER_STYLE_ORDER as string[]).includes(raw)) {
    return raw as VisualizerStyleId;
  }
  return DEFAULT_STYLE;
}

export interface VisualizerStyle {
  /** Current selection. */
  styleId: VisualizerStyleId;
  /**
   * `true` only once the active profile's stored value has been read. Until
   * then `styleId` is the placeholder default; consumers must wait for this
   * before letting the user act (see `cycle`, which no-ops when not ready) so
   * an early click can't persist a default-derived value over the real one.
   */
  ready: boolean;
  /**
   * Advance to the next style in {@link VISUALIZER_STYLE_ORDER}, wrapping.
   * No-ops until {@link VisualizerStyle.ready} is `true`.
   */
  cycle: () => Promise<void>;
}

/**
 * Per-profile preference: the spectrum-visualizer drawing style (issue
 * #699), the sibling of [`useVisualizerColor`](./useVisualizerColor.ts) —
 * the two together are what the immersive view's controls cycle.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useVisualizerStyle(): VisualizerStyle {
  const {
    value: styleId,
    ready,
    setValue,
  } = useProfileSetting<VisualizerStyleId>({
    key: KEY,
    defaultValue: DEFAULT_STYLE,
    parse: parseStyleId,
    serialize: (value) => value,
    valueType: "string",
    event: VISUALIZER_STYLE_EVENT,
    label: "useVisualizerStyle",
  });

  const cycle = useCallback(async () => {
    // Refuse until the stored value has loaded for the ACTIVE profile —
    // otherwise we'd cycle from the placeholder default and clobber the
    // persisted style.
    if (!ready) return;
    // Functional update: the shared hook hands us the synchronously-current
    // value, so back-to-back clicks advance one step each instead of both
    // computing from the same render-lagged snapshot.
    await setValue((previous) => {
      const idx = VISUALIZER_STYLE_ORDER.indexOf(previous);
      return VISUALIZER_STYLE_ORDER[(idx + 1) % VISUALIZER_STYLE_ORDER.length];
    });
  }, [ready, setValue]);

  return { styleId, ready, cycle };
}
