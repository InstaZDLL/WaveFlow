import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useHiResBadgeVisibility } from "../../../hooks/useHiResBadgeVisibility";

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
      <label className="flex items-start justify-between gap-3 cursor-pointer">
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
            <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
              {t("settings.hiResBadge.subtitle")}
            </span>
          </span>
        </span>
        <input
          type="checkbox"
          checked={visible}
          onChange={(e) => {
            void setVisible(e.target.checked);
          }}
          className="mt-1.5 w-4 h-4 accent-emerald-500 cursor-pointer shrink-0"
          aria-label={t("settings.hiResBadge.title")}
        />
      </label>
    </section>
  );
}
