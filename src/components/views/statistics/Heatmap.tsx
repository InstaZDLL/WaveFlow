import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ListeningByDayRow } from "../../../lib/tauri/stats";
import { formatListenTime } from "./formatters";

interface HeatmapProps {
  /** Daily activity rows. Days outside `weeks` worth are ignored. */
  data: ListeningByDayRow[];
  /** How many full ISO-style weeks (7-day columns) to render. Default 53. */
  weeks?: number;
  /** Localized empty-state copy. */
  emptyText?: string;
}

interface CellDatum {
  /** "YYYY-MM-DD" local date key. Empty string for padding cells. */
  key: string;
  date: Date | null;
  plays: number;
  listenedMs: number;
  /** 0 = no activity, 1..4 = intensity bucket. */
  level: 0 | 1 | 2 | 3 | 4;
}

const LEVEL_CLASSES: Record<CellDatum["level"], string> = {
  0: "bg-zinc-200/70 dark:bg-zinc-800/60",
  1: "bg-emerald-200 dark:bg-emerald-900/60",
  2: "bg-emerald-400 dark:bg-emerald-700/80",
  3: "bg-emerald-500 dark:bg-emerald-500/90",
  4: "bg-emerald-600 dark:bg-emerald-400",
};

function startOfDay(d: Date): Date {
  const c = new Date(d);
  c.setHours(0, 0, 0, 0);
  return c;
}

function dayKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${dd}`;
}

/**
 * GitHub-style "contributions" heatmap. 7 rows (weekdays) × N columns
 * (weeks). Each cell intensity is bucketed against the period max so
 * the visualisation stays readable whether the user listens for 5 min
 * or 5 h a day. Sunday-first to match the GitHub original; locale day
 * names are pulled from `Intl.DateTimeFormat` so RTL/CJK locales get
 * their own labels.
 */
export function Heatmap({ data, weeks = 53, emptyText }: HeatmapProps) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;
  const [hovered, setHovered] = useState<CellDatum | null>(null);

  const { columns, weekdayLabels, monthLabels, totalMs, totalPlays, maxMs } =
    useMemo(() => {
      const today = startOfDay(new Date());
      // Anchor the rightmost column to the week containing today, so
      // the grid always finishes on the current weekday — matches the
      // GitHub layout convention.
      const lastColumnEnd = new Date(today);
      lastColumnEnd.setDate(today.getDate() + (6 - today.getDay()));
      const totalDays = weeks * 7;
      const firstDay = new Date(lastColumnEnd);
      firstDay.setDate(lastColumnEnd.getDate() - (totalDays - 1));

      const lookup = new Map<string, ListeningByDayRow>();
      for (const row of data) lookup.set(row.day, row);

      const cells: CellDatum[] = [];
      let max = 0;
      let totMs = 0;
      let totPlays = 0;
      for (let i = 0; i < totalDays; i += 1) {
        const d = new Date(firstDay);
        d.setDate(firstDay.getDate() + i);
        const key = dayKey(d);
        const row = lookup.get(key);
        const isFuture = d.getTime() > today.getTime();
        const plays = row?.plays ?? 0;
        const ms = row?.listened_ms ?? 0;
        if (!isFuture) {
          if (ms > max) max = ms;
          totMs += ms;
          totPlays += plays;
        }
        cells.push({
          key: isFuture ? "" : key,
          date: isFuture ? null : d,
          plays,
          listenedMs: ms,
          level: 0,
        });
      }

      // 4 quartile-style buckets against the local max — keeps the
      // gradient meaningful for both light and heavy listeners.
      const t1 = max * 0.15;
      const t2 = max * 0.35;
      const t3 = max * 0.6;
      for (const cell of cells) {
        if (cell.listenedMs <= 0 || max <= 0) {
          cell.level = 0;
        } else if (cell.listenedMs <= t1) {
          cell.level = 1;
        } else if (cell.listenedMs <= t2) {
          cell.level = 2;
        } else if (cell.listenedMs <= t3) {
          cell.level = 3;
        } else {
          cell.level = 4;
        }
      }

      const cols: CellDatum[][] = [];
      for (let c = 0; c < weeks; c += 1) {
        cols.push(cells.slice(c * 7, c * 7 + 7));
      }

      // Locale-aware short weekday names (Sun..Sat). Anchor on a known
      // Sunday so we don't depend on the runtime's week-start setting.
      const weekdayFmt = new Intl.DateTimeFormat(locale, { weekday: "short" });
      const sunday = new Date(2024, 0, 7); // 2024-01-07 = Sunday
      const wdLabels: string[] = [];
      for (let i = 0; i < 7; i += 1) {
        const d = new Date(sunday);
        d.setDate(sunday.getDate() + i);
        wdLabels.push(weekdayFmt.format(d));
      }

      // Month labels: place a label above each column where the first
      // visible day's month differs from the previous column's.
      const monthFmt = new Intl.DateTimeFormat(locale, { month: "short" });
      const mLabels: { col: number; label: string }[] = [];
      let prevMonth = -1;
      for (let c = 0; c < cols.length; c += 1) {
        const firstReal = cols[c].find((cell) => cell.date != null);
        if (!firstReal?.date) continue;
        const m = firstReal.date.getMonth();
        if (m !== prevMonth) {
          mLabels.push({ col: c, label: monthFmt.format(firstReal.date) });
          prevMonth = m;
        }
      }

      return {
        columns: cols,
        weekdayLabels: wdLabels,
        monthLabels: mLabels,
        totalMs: totMs,
        totalPlays: totPlays,
        maxMs: max,
      };
    }, [data, weeks, locale]);

  const dateFmt = useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        weekday: "long",
        day: "numeric",
        month: "long",
        year: "numeric",
      }),
    [locale],
  );

  if (maxMs === 0) {
    return (
      <p className="text-sm text-zinc-500 dark:text-zinc-400">
        {emptyText ?? t("statistics.heatmap.empty")}
      </p>
    );
  }

  return (
    <div className="w-full">
      <div className="flex justify-between items-baseline mb-3 text-xs text-zinc-500 dark:text-zinc-400">
        <span>
          {t("statistics.heatmap.summary", {
            time: formatListenTime(totalMs),
            plays: totalPlays,
          })}
        </span>
        <span aria-live="polite" className="min-h-4">
          {hovered?.date
            ? `${dateFmt.format(hovered.date)} — ${formatListenTime(hovered.listenedMs)} (${hovered.plays})`
            : ""}
        </span>
      </div>

      <div
        className="overflow-x-auto pb-1"
        role="img"
        aria-label={t("statistics.heatmap.title")}
      >
        <div className="inline-flex flex-col gap-1 min-w-full">
          {/* Month header row */}
          <div className="flex pl-8">
            {columns.map((_, ci) => {
              const label = monthLabels.find((m) => m.col === ci)?.label;
              return (
                <div
                  key={ci}
                  className="w-3.5 text-[10px] text-zinc-500 dark:text-zinc-400 mr-0.5"
                >
                  {label ?? ""}
                </div>
              );
            })}
          </div>

          <div className="flex">
            {/* Weekday labels — show a subset so they don't overlap. */}
            <div className="flex flex-col w-8 pr-1 gap-0.5 text-[10px] text-zinc-500 dark:text-zinc-400 leading-[14px]">
              {weekdayLabels.map((wd, i) => (
                <div
                  key={i}
                  className="h-3.5 flex items-center justify-end"
                  aria-hidden={i % 2 === 0}
                >
                  {i % 2 === 1 ? wd : ""}
                </div>
              ))}
            </div>

            {/* Grid */}
            <div className="flex gap-0.5">
              {columns.map((col, ci) => (
                <div key={ci} className="flex flex-col gap-0.5">
                  {col.map((cell, ri) => {
                    if (!cell.date) {
                      return (
                        <div key={ri} className="w-3.5 h-3.5" aria-hidden />
                      );
                    }
                    return (
                      <div
                        key={ri}
                        className={`w-3.5 h-3.5 rounded-[3px] ${LEVEL_CLASSES[cell.level]} hover:ring-1 hover:ring-emerald-500 cursor-default transition-shadow`}
                        title={`${dateFmt.format(cell.date)} — ${formatListenTime(cell.listenedMs)} (${cell.plays})`}
                        onMouseEnter={() => setHovered(cell)}
                        onMouseLeave={() => setHovered(null)}
                      />
                    );
                  })}
                </div>
              ))}
            </div>
          </div>

          {/* Legend */}
          <div className="flex items-center justify-end gap-1.5 pt-2 text-[10px] text-zinc-500 dark:text-zinc-400">
            <span>{t("statistics.heatmap.less")}</span>
            {([0, 1, 2, 3, 4] as const).map((lvl) => (
              <span
                key={lvl}
                className={`w-3 h-3 rounded-[3px] ${LEVEL_CLASSES[lvl]}`}
              />
            ))}
            <span>{t("statistics.heatmap.more")}</span>
          </div>
        </div>
      </div>
    </div>
  );
}
