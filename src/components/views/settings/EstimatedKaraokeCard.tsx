import { useTranslation } from "react-i18next";
import { Mic2 } from "lucide-react";
import { useEstimatedKaraokeSetting } from "../../../hooks/useEstimatedKaraoke";

/**
 * Settings → Lyrics row for the estimated word-by-word highlight on
 * line-synced lyrics (#716). Default off; the subtitle says what it is —
 * an estimate, drawn and never saved — so nobody mistakes it for real
 * word timing.
 */
export function EstimatedKaraokeCard() {
  const { t } = useTranslation();
  const { value, ready, setValue } = useEstimatedKaraokeSetting();

  return (
    <section
      aria-label={t("settings.lyricsEstimateWords.title")}
      className="px-4 py-3"
    >
      <label className="flex items-start justify-between gap-3 cursor-pointer">
        <span className="flex items-start gap-3 min-w-0">
          <Mic2
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.lyricsEstimateWords.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.lyricsEstimateWords.subtitle")}
            </span>
          </span>
        </span>
        <input
          type="checkbox"
          checked={value}
          disabled={!ready}
          onChange={(e) => {
            void setValue(e.target.checked);
          }}
          className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0 disabled:cursor-not-allowed"
          aria-label={t("settings.lyricsEstimateWords.title")}
        />
      </label>
    </section>
  );
}
