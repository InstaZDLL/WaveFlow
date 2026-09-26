import { useTranslation } from "react-i18next";
import { Film } from "lucide-react";
import { useMotionCovers } from "../../../hooks/useMotionCovers";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Appearance row showing or hiding animated album covers on
 * every now-playing surface — plugin-supplied and hand-set alike (#766).
 * Default ON. A setting rather than a player button, to keep the immersive
 * top bar from growing on small screens.
 */
export function MotionCoversCard() {
  const { t } = useTranslation();
  const { enabled, setEnabled } = useMotionCovers();

  return (
    <section
      aria-label={t("settings.motionCovers.title")}
      className="px-4 py-3"
    >
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Film
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.motionCovers.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.motionCovers.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => {
            void setEnabled(!enabled);
          }}
          label={t("settings.motionCovers.title")}
        />
      </div>
    </section>
  );
}
