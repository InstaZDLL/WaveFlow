import { type YearRow } from "../../../lib/tauri/browse";
import { type Translator } from "./libraryTypes";
import { Artwork } from "../../common/Artwork";
import { CalendarDays } from "lucide-react";

interface YearListProps {
  years: YearRow[];
  isLoading: boolean;
  t: Translator;
  onSelect: (year: number | null) => void;
}

/** A section per decade, newest first, then the tracks with no usable
 *  year, which the backend already lists last. */
export function YearList({ years, isLoading, t, onSelect }: YearListProps) {
  const sections: { key: string; label: string; rows: YearRow[] }[] = [];
  for (const row of years) {
    const decade = row.year == null ? null : Math.floor(row.year / 10) * 10;
    const key = decade == null ? "unknown" : String(decade);
    let section = sections[sections.length - 1];
    if (!section || section.key !== key) {
      section = {
        key,
        label:
          decade == null
            ? t("library.yearList.unknown")
            : t("library.yearList.decade", { decade }),
        rows: [],
      };
      sections.push(section);
    }
    section.rows.push(row);
  }
  return (
    <div className={`space-y-8 ${isLoading ? "opacity-50" : ""}`}>
      {sections.map((section) => (
        <section key={section.key} aria-label={section.label}>
          <h3 className="mb-3 text-sm font-bold tracking-wide text-zinc-500 dark:text-zinc-400">
            {section.label}
          </h3>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-4">
            {section.rows.map((row) => (
              <button
                type="button"
                key={row.year ?? "unknown"}
                onClick={() => onSelect(row.year)}
                className="flex items-center space-x-3 p-4 rounded-2xl border border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-800/40 dark:hover:bg-zinc-800/70 transition-colors cursor-pointer text-left focus:outline-none focus:ring-2 focus:ring-emerald-500/40"
              >
                <Artwork
                  path={row.artwork_path}
                  path1x={row.artwork_path_1x}
                  path2x={row.artwork_path_2x}
                  size="1x"
                  className="w-12 h-12"
                  rounded="xl"
                  iconSize={22}
                  placeholderIcon={CalendarDays}
                  alt=""
                />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-semibold tabular-nums text-zinc-800 dark:text-zinc-200 truncate">
                    {row.year ?? t("library.yearList.unknown")}
                  </div>
                  <div className="text-xs text-zinc-500 truncate">
                    {t("library.genreList.trackCount", {
                      count: row.track_count,
                    })}
                  </div>
                </div>
              </button>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}
