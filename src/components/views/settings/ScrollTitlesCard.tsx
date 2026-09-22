import { useTranslation } from "react-i18next";
import { MoveHorizontal } from "lucide-react";
import { useScrollLongTitles } from "../../../hooks/useScrollLongTitles";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Appearance row toggling the marquee that scrolls long
 * titles (PlayerBar + immersive view). Default ON; turning it off
 * makes long titles truncate with an ellipsis instead.
 */
export function ScrollTitlesCard() {
  const { t } = useTranslation();
  const { enabled, setEnabled } = useScrollLongTitles();

  return (
    <section
      aria-label={t("settings.scrollTitles.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <MoveHorizontal
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.scrollTitles.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.scrollTitles.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => {
            void setEnabled(!enabled);
          }}
          label={t("settings.scrollTitles.title")}
        />
      </div>
    </section>
  );
}
