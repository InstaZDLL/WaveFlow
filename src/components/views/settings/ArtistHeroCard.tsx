import { useTranslation } from "react-i18next";
import { PanelTop } from "lucide-react";
import { useArtistHero } from "../../../hooks/useArtistHero";

/**
 * Renders an appearance setting for enabling or disabling the artist detail hero backdrop.
 *
 * @returns A settings row containing a controlled checkbox for the hero backdrop.
 */
export function ArtistHeroCard() {
  const { t } = useTranslation();
  const { enabled, setEnabled } = useArtistHero();

  return (
    <section aria-label={t("settings.artistHero.title")} className="px-4 py-3">
      <label className="flex items-start justify-between gap-3 cursor-pointer">
        <span className="flex items-start gap-3 min-w-0">
          <PanelTop
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.artistHero.title")}
            </span>
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.artistHero.subtitle")}
            </span>
          </span>
        </span>
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => {
            void setEnabled(e.target.checked);
          }}
          className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0"
          aria-label={t("settings.artistHero.title")}
        />
      </label>
    </section>
  );
}
