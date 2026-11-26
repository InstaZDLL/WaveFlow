import { useTranslation } from "react-i18next";
import { MoveVertical } from "lucide-react";
import { useLyricsStaggeredScrollSetting } from "../../../hooks/useLyricsStaggeredScroll";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Lyrics row: the lyrics move on line by line rather than in
 * one block (`useLyricsFollow`). Default OFF, since every line in view
 * glides on a layer of its own.
 */
export function LyricsStaggeredScrollCard() {
  const { t } = useTranslation();
  const { value: staggered, setValue: setStaggered } =
    useLyricsStaggeredScrollSetting();

  return (
    <section
      aria-label={t("settings.lyricsStaggeredScroll.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-4 min-w-0">
          <MoveVertical
            size={16}
            className="settings-row-icon"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.lyricsStaggeredScroll.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.lyricsStaggeredScroll.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={staggered}
          onToggle={() => {
            void setStaggered(!staggered);
          }}
          label={t("settings.lyricsStaggeredScroll.title")}
        />
      </div>
    </section>
  );
}
