import { useTranslation } from "react-i18next";
import { AudioLines, AudioWaveform, BarChart2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { VisualizerStyleId } from "../../hooks/useVisualizerStyle";

/** One glyph per style, so the button shows what it will draw. */
const ICONS: Record<VisualizerStyleId, LucideIcon> = {
  wave: AudioWaveform,
  mirror: AudioLines,
  bars: BarChart2,
};

interface VisualizerStyleButtonProps {
  /** Current selection — drives the icon and the localized label. */
  styleId: VisualizerStyleId;
  /** Advance to the next style (loops). */
  onCycle: () => void;
  size?: number;
}

/**
 * Immersive-view control that cycles the spectrum-visualizer style
 * (issue #699): smooth wave → mirrored wave → bars. The sibling of
 * [`VisualizerColorButton`](./VisualizerColorButton.tsx), and the same
 * "advance through a fixed list" shape as the repeat-mode button — the two
 * sit next to each other, because between them they are the whole of what
 * the visualizer looks like.
 */
export function VisualizerStyleButton({
  styleId,
  onCycle,
  size = 20,
}: VisualizerStyleButtonProps) {
  const { t } = useTranslation();
  const label = t("settings.visualizer.cycleStyle", {
    style: t(`settings.visualizer.styles.${styleId}`),
  });
  const Icon = ICONS[styleId];
  return (
    <button
      type="button"
      onClick={onCycle}
      aria-label={label}
      title={label}
      className="p-2 rounded-full text-white/60 transition-colors hover:text-white"
    >
      <Icon size={size} aria-hidden="true" />
    </button>
  );
}
