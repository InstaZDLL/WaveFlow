import { useTranslation } from "react-i18next";
import { Images } from "lucide-react";
import { useCoverSlideshow } from "../../../hooks/useCoverSlideshow";
import { ToggleSwitch } from "../../common/ToggleSwitch";

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
      <div className="flex items-center justify-between gap-3">
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
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.coverSlideshow.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => {
            void setEnabled(!enabled);
          }}
          label={t("settings.coverSlideshow.title")}
        />
      </div>
    </section>
  );
}
