import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { TopGenreRow } from "../../../lib/tauri/stats";
import { formatListenTime } from "./formatters";

interface TopGenresProps {
  data: TopGenreRow[];
}

/**
 * Horizontal-bar breakdown of listening time per genre. Bars are
 * sized against the top genre's `listened_ms` so the leader fills the
 * track and the rest scale down proportionally. Pure CSS widths — no
 * SVG, since the layout is a simple stacked list rather than an axis
 * chart.
 */
export function TopGenres({ data }: TopGenresProps) {
  const { t } = useTranslation();

  const max = useMemo(
    () => data.reduce((m, g) => Math.max(m, g.listened_ms), 0),
    [data],
  );

  return (
    <section className="rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5">
      <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400 mb-4">
        {t("statistics.topGenres.title")}
      </h2>
      {data.length === 0 || max === 0 ? (
        <div className="flex items-center justify-center h-24 text-sm text-zinc-500 dark:text-zinc-400">
          {t("statistics.topGenres.empty")}
        </div>
      ) : (
        <ul className="space-y-3">
          {data.map((genre) => {
            const pct = max > 0 ? (genre.listened_ms / max) * 100 : 0;
            return (
              <li key={genre.genre_id}>
                <div className="flex items-baseline justify-between gap-3 mb-1">
                  <span className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                    {genre.name}
                  </span>
                  <span className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums shrink-0">
                    {formatListenTime(genre.listened_ms)} ·{" "}
                    {t("statistics.plays", { count: genre.plays })}
                  </span>
                </div>
                <div
                  className="h-2 rounded-full bg-zinc-100 dark:bg-zinc-800 overflow-hidden"
                  role="progressbar"
                  aria-valuenow={Math.round(pct)}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-label={genre.name}
                >
                  <div
                    className="h-full rounded-full bg-emerald-500/80"
                    style={{ width: `${Math.max(2, pct)}%` }}
                  />
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
