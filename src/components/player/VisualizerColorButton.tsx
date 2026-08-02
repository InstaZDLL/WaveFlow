import { useTranslation } from "react-i18next";
import type { VisualizerColorId } from "../../hooks/useVisualizerColor";

/** A representative rainbow swatch for the button when `rainbow` is active. */
const RAINBOW_SWATCH =
  "conic-gradient(from 0deg, #ef4444, #f97316, #eab308, #22c55e, #22d3ee, #6366f1, #d946ef, #ef4444)";

interface VisualizerColorButtonProps {
  /** Current selection — drives the localized label. */
  colorId: VisualizerColorId;
  /** Solid CSS fill for the swatch, or `undefined` when `rainbow`. */
  color: string | undefined;
  /** Whether the current selection is the per-bar rainbow. */
  rainbow: boolean;
  /** Advance to the next colour (loops). */
  onCycle: () => void;
  size?: number;
}

/**
 * Immersive-view control that cycles the spectrum-visualizer colour
 * (issue #468). The swatch shows the current colour (a conic gradient for
 * rainbow); clicking advances to the next and loops back to the default —
 * same "advance-through-a-fixed-list" shape as the repeat-mode button, so the
 * user can find a tint that reads well over the album-derived backdrop.
 */
export function VisualizerColorButton({
  colorId,
  color,
  rainbow,
  onCycle,
  size = 20,
}: VisualizerColorButtonProps) {
  const { t } = useTranslation();
  const label = t("settings.visualizer.cycleColor", {
    color: t(`settings.visualizer.colors.${colorId}`),
  });
  return (
    <button
      type="button"
      onClick={onCycle}
      aria-label={label}
      title={label}
      className="p-2 rounded-full text-white/60 transition-colors hover:text-white"
    >
      <span
        className="block rounded-full border border-white/40 shadow-sm"
        style={{
          width: size,
          height: size,
          background: rainbow ? RAINBOW_SWATCH : color,
        }}
      />
    </button>
  );
}
