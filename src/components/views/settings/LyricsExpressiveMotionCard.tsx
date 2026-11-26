import { useTranslation } from "react-i18next";
import { MoveVertical } from "lucide-react";
import { useLyricsExpressiveMotionSetting } from "../../../hooks/useLyricsExpressiveMotion";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Lyrics row: the lyrics move on line by line and each word
 * rises as it is sung (`useLyricsExpressiveMotion`). Default OFF.
 */
export function LyricsExpressiveMotionCard() {
  const { t } = useTranslation();
  const { value: expressive, setValue: setExpressive } =
    useLyricsExpressiveMotionSetting();

  return (
    <section
      aria-label={t("settings.lyricsExpressiveMotion.title")}
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
              {t("settings.lyricsExpressiveMotion.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.lyricsExpressiveMotion.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={expressive}
          onToggle={() => {
            void setExpressive(!expressive);
          }}
          label={t("settings.lyricsExpressiveMotion.title")}
        />
      </div>
    </section>
  );
}
