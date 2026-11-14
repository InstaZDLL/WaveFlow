import { useTranslation } from "react-i18next";
import { LocateFixed } from "lucide-react";

/**
 * Shown while the user browses synced lyrics away from the sung line
 * (`useLyricsFollow`): takes the view back to that line and has it follow
 * again. Floats at the foot of the lyrics, centred, so it reads the same
 * in either direction.
 */
export function LyricsResumeButton({
  onClick,
  variant,
}: {
  onClick: () => void;
  /** `panel` on the app's surface, `immersive` over the dark takeover. */
  variant: "panel" | "immersive";
}) {
  const { t } = useTranslation();
  const label = t("lyrics.actions.backToPlayback");
  return (
    <div className="pointer-events-none absolute inset-x-0 bottom-4 flex justify-center px-4">
      <button
        type="button"
        onClick={onClick}
        data-lyrics-resume=""
        className={`pointer-events-auto inline-flex max-w-full items-center gap-2 rounded-full shadow-lg transition-colors focus:outline-none focus-visible:ring-2 ${
          variant === "panel"
            ? "px-3.5 py-1.5 text-xs font-semibold bg-zinc-900 text-white hover:bg-zinc-700 focus-visible:ring-emerald-500 dark:bg-white dark:text-zinc-900 dark:hover:bg-zinc-200"
            : "px-4 py-2 text-sm font-semibold bg-white/90 text-zinc-900 hover:bg-white focus-visible:ring-white/70"
        }`}
      >
        <LocateFixed
          size={variant === "panel" ? 14 : 16}
          className="shrink-0"
        />
        <span className="truncate">{label}</span>
      </button>
    </div>
  );
}
