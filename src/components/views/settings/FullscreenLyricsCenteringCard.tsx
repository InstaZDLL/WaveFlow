import { useTranslation } from "react-i18next";
import { AlignCenter } from "lucide-react";
import { useFullscreenLyricsCentering } from "../../../hooks/useFullscreenLyricsCentering";

/**
 * Settings → Appearance row toggling the centering of synced lyrics
 * in the fullscreen overlay (#168). Default OFF so existing users
 * see no change unless they opt in.
 */
export function FullscreenLyricsCenteringCard() {
  const { t } = useTranslation();
  const { centered, setCentered } = useFullscreenLyricsCentering();

  return (
    <section
      aria-label={t("settings.fullscreenLyricsCentering.title")}
      className="px-4 py-3"
    >
      <label className="flex items-start justify-between gap-3 cursor-pointer">
        <span className="flex items-start gap-3 min-w-0">
          <AlignCenter
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.fullscreenLyricsCentering.title")}
            </span>
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.fullscreenLyricsCentering.subtitle")}
            </span>
          </span>
        </span>
        <input
          type="checkbox"
          checked={centered}
          onChange={(e) => {
            void setCentered(e.target.checked);
          }}
          className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0"
          aria-label={t("settings.fullscreenLyricsCentering.title")}
        />
      </label>
    </section>
  );
}
