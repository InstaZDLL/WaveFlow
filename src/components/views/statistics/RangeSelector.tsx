import { useTranslation } from "react-i18next";
import type { StatsRange } from "../../../lib/tauri/stats";

interface RangeSelectorProps {
  value: StatsRange;
  onChange: (range: StatsRange) => void;
}

const RANGES: StatsRange[] = ["7d", "30d", "90d", "1y", "all"];

export function RangeSelector({ value, onChange }: RangeSelectorProps) {
  const { t } = useTranslation();
  return (
    <div
      role="tablist"
      aria-label={t("statistics.range.label")}
      className="inline-flex items-center gap-1 p-1 rounded-xl bg-zinc-100 dark:bg-zinc-800/60 border border-zinc-200 dark:border-zinc-700"
    >
      {RANGES.map((r) => {
        const active = r === value;
        return (
          <button
            key={r}
            role="tab"
            aria-selected={active}
            type="button"
            onClick={() => onChange(r)}
            className={`px-3 py-1.5 text-xs font-medium rounded-lg transition-colors ${
              active
                ? "bg-white dark:bg-zinc-900 text-zinc-900 dark:text-white shadow-sm"
                : "text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
            }`}
          >
            {t(`statistics.range.${r}`)}
          </button>
        );
      })}
    </div>
  );
}
