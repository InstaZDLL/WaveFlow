import { useTranslation } from "react-i18next";
import { Images } from "lucide-react";
import { useCoverSlideshow } from "../../../hooks/useCoverSlideshow";

/**
 * Settings → Appearance row toggling the cover ↔ artist crossfade slideshow
 * on the now-playing surfaces (issue #466). Default OFF; turning it on gently
 * alternates the album cover with the artist photo behind the immersive view
 * and the Now Playing panel.
 */
export function CoverSlideshowCard() {
  const { t } = useTranslation();
  const { enabled, setEnabled } = useCoverSlideshow();

  return (
    <section
      aria-label={t("settings.coverSlideshow.title")}
      className="px-4 py-3"
    >
      <label className="flex items-start justify-between gap-3 cursor-pointer">
        <span className="flex items-start gap-3 min-w-0">
          <Images
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.coverSlideshow.title")}
            </span>
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.coverSlideshow.subtitle")}
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
          aria-label={t("settings.coverSlideshow.title")}
        />
      </label>
    </section>
  );
}
