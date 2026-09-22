import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useHiResBadgeVisibility } from "../../../hooks/useHiResBadgeVisibility";
import { ToggleSwitch } from "../../common/ToggleSwitch";

/**
 * Settings → Appearance row toggling the Hi-Res / DSD pill that
 * decorates track lists, album grids, and the compact label under
 * the artist name in the player bar. Default ON because the pill is
 * part of WaveFlow's audiophile identity; the toggle lets users who
 * find it noisy turn it off without touching every list view.
 */
export function HiResBadgeCard() {
  const { t } = useTranslation();
  const { visible, setVisible } = useHiResBadgeVisibility();

  return (
    <section aria-label={t("settings.hiResBadge.title")} className="px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <span className="flex items-start gap-3 min-w-0">
          <Sparkles
            size={20}
            className="text-zinc-400 mt-0.5 shrink-0"
            aria-hidden="true"
          />
          <span className="min-w-0">
            <span className="block text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.hiResBadge.title")}
            </span>
            <span className="block text-xs mt-0.5 settings-description">
              {t("settings.hiResBadge.subtitle")}
            </span>
          </span>
        </span>
        <ToggleSwitch
          enabled={visible}
          onToggle={() => {
            void setVisible(!visible);
          }}
          label={t("settings.hiResBadge.title")}
        />
      </div>
    </section>
  );
}
