import { useTranslation } from "react-i18next";
import { SquarePlay } from "lucide-react";

/**
 * "Show Canvas" toggle (issue #442) — the Spotify-style button that reveals
 * or hides the looping Canvas behind the now-playing view. The surface only
 * renders it when the current track actually has a Canvas (and motion isn't
 * reduced), so it never appears as a dead control. Drives the global
 * `useCanvasEnabled` preference.
 */
export function CanvasToggleButton({
  enabled,
  onToggle,
  className,
  size = 22,
}: {
  enabled: boolean;
  onToggle: () => void;
  className?: string;
  size?: number;
}) {
  const { t } = useTranslation();
  const label = enabled ? t("canvas.hide") : t("canvas.show");
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-label={label}
      aria-pressed={enabled}
      title={label}
      className={className}
    >
      <SquarePlay size={size} />
    </button>
  );
}
