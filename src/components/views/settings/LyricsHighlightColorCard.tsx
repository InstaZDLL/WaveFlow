import { useTranslation } from "react-i18next";
import { Palette } from "lucide-react";
import { useLyricsHighlightColor } from "../../../hooks/useLyricsHighlightColor";

/** What the picker shows while no colour is chosen: the white every view
 *  draws its sung line in by default. */
const DEFAULT_SWATCH = "#ffffff";

/**
 * Settings → Lyrics row for the colour of the line being sung in the
 * immersive lyrics and the mini-player (#751). The desktop lyrics window
 * has its own colours, in its own card, and keeps them.
 */
export function LyricsHighlightColorCard() {
  const { t } = useTranslation();
  const { color, ready, setColor } = useLyricsHighlightColor();

  return (
    <section
      aria-label={t("settings.lyricsHighlightColor.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Palette
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.lyricsHighlightColor.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.lyricsHighlightColor.subtitle")}
            </span>
          </span>
        </span>
        <span className="flex items-center gap-2 shrink-0">
          {color != null && (
            <button
              type="button"
              onClick={() => void setColor(null)}
              disabled={!ready}
              className="text-xs font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-white transition-colors disabled:opacity-40"
            >
              {t("settings.lyricsHighlightColor.reset")}
            </button>
          )}
          <input
            type="color"
            value={color ?? DEFAULT_SWATCH}
            onChange={(e) => void setColor(e.target.value)}
            disabled={!ready}
            aria-label={t("settings.lyricsHighlightColor.title")}
            className="h-8 w-12 cursor-pointer rounded border border-zinc-200 dark:border-zinc-700 bg-transparent disabled:cursor-not-allowed disabled:opacity-40"
          />
        </span>
      </div>
    </section>
  );
}
