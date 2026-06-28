import { useTranslation } from "react-i18next";
import { BarChart2 } from "lucide-react";
import {
  STATS_KPI_IDS,
  useHiddenKpis,
  type StatsKpiId,
} from "../../../hooks/useHiddenKpis";

/** Maps each KPI id to the same i18n label the Statistics view uses. */
const KPI_LABEL_KEYS: Record<StatsKpiId, string> = {
  total_plays: "statistics.kpi.totalPlays",
  total_time: "statistics.kpi.totalTime",
  unique_tracks: "statistics.kpi.uniqueTracks",
  completion_rate: "statistics.kpi.completionRate",
};

/**
 * Settings → Appearance card letting the user hide individual KPI
 * cards on the Statistics view. Each checkbox = "show this card";
 * unchecking persists the id into `stats.hidden_kpis`. Motivated by
 * the "Full-listen rate" KPI feeling judgemental, but applies to
 * every KPI uniformly.
 */
export function StatsKpiVisibilityCard() {
  const { t } = useTranslation();
  const { isHidden, toggle, ready } = useHiddenKpis();

  return (
    <section
      aria-label={t("settings.statsKpis.title")}
      className="space-y-3"
    >
      <header className="px-4 flex items-start gap-3">
        <BarChart2
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3 className="text-sm font-semibold text-zinc-900 dark:text-white">
            {t("settings.statsKpis.title")}
          </h3>
          <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
            {t("settings.statsKpis.subtitle")}
          </p>
        </div>
      </header>

      <fieldset className="mx-4">
        <legend className="sr-only">{t("settings.statsKpis.legend")}</legend>
        <div className="space-y-1">
          {STATS_KPI_IDS.map((id) => {
            const checked = !isHidden(id);
            return (
              <label
                key={id}
                className={`flex items-center gap-3 px-3 py-2 rounded-lg transition-colors ${
                  ready
                    ? "cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/30"
                    : "cursor-not-allowed opacity-50"
                }`}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  // Block toggles until the per-profile preference has
                  // loaded — `hidden` is momentarily empty during the
                  // read, so a click here would persist a wrong state.
                  disabled={!ready}
                  onChange={() => {
                    toggle(id);
                  }}
                  className="w-4 h-4 accent-emerald-500 cursor-pointer disabled:cursor-not-allowed"
                />
                <span className="text-sm text-zinc-800 dark:text-zinc-200">
                  {t(KPI_LABEL_KEYS[id])}
                </span>
              </label>
            );
          })}
        </div>
      </fieldset>
    </section>
  );
}
