import { useTranslation } from "react-i18next";
import { useRadioTimeshift } from "../../hooks/useRadioTimeshift";
import { formatDuration } from "../../lib/tauri/track";

/**
 * Where a live station stands against its live edge, in the place the
 * progress row takes for a track: "Live", or how far behind it the
 * listener is after a pause. Read only — going back to live is in the
 * overflow menu (`MoreActionsMenu`), like every new player-bar action.
 */
export function LiveStatus({ compact = false }: { compact?: boolean }) {
  const { t } = useTranslation();
  const { isRadio, isBehind, behindMs } = useRadioTimeshift();
  if (!isRadio) return null;
  const label = isBehind
    ? t("player.live.behind", { time: formatDuration(behindMs) })
    : t("player.live.live");
  return (
    <div
      className={`flex items-center justify-center ${compact ? "" : "w-full py-1"}`}
      data-live-status={isBehind ? "behind" : "live"}
    >
      <span
        role="status"
        className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[10px] font-bold uppercase tracking-widest tabular-nums ${
          isBehind
            ? "bg-zinc-500/15 text-zinc-600 dark:text-zinc-300"
            : "bg-red-500/15 text-red-600 dark:text-red-400"
        }`}
      >
        <span
          aria-hidden="true"
          className={`w-1.5 h-1.5 rounded-full ${isBehind ? "bg-zinc-400" : "bg-red-500"}`}
        />
        {label}
      </span>
    </div>
  );
}
