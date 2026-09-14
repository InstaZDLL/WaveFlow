import {
  AlertTriangle,
  CalendarX,
  CopyCheck,
  Disc3,
  FileWarning,
  Hash,
  ImageOff,
  ListX,
  Mic2Icon,
  TextCursorInput,
  Users,
} from "lucide-react";
import type { InventoryCategory } from "../../../lib/tauri/inventory";

/**
 * The "needs attention" inventory, as counted categories you click into
 * (issue #589).
 *
 * There was no way to ask the library what was wrong with it: somebody
 * who had just imported two thousand files had no starting point other
 * than scrolling and noticing.
 *
 * Two rendering decisions:
 *
 * - **A zero-count category is shown, greyed and inert**, not hidden. A
 *   list whose contents change shape between two visits is hard to
 *   learn, and "no albums disagree about their year" is itself the
 *   answer to a question the user came here to ask.
 * - **Categories keep the backend's order** — per-track problems, then
 *   the ones only an album view can see, then probable duplicates.
 *   Sorting by count would reshuffle the list every time the user fixed
 *   something.
 */
const ICONS: Record<string, typeof AlertTriangle> = {
  missing_title: TextCursorInput,
  missing_artist: Mic2Icon,
  missing_album: Disc3,
  missing_year: CalendarX,
  missing_track_number: Hash,
  missing_cover: ImageOff,
  untaggable: FileWarning,
  album_year_conflict: CalendarX,
  compilation_no_album_artist: Users,
  duplicate_track_number: ListX,
  track_number_gap: ListX,
  probable_duplicate: CopyCheck,
};

/** See the note in `TrackTableHeader`. */
type Translator = (key: string, options?: Record<string, unknown>) => string;

interface InventoryCategoriesProps {
  categories: InventoryCategory[];
  isLoading: boolean;
  activeKey: string | null;
  onSelect: (key: string) => void;
  t: Translator;
}

export function InventoryCategories({
  categories,
  isLoading,
  activeKey,
  onSelect,
  t,
}: InventoryCategoriesProps) {
  if (isLoading) {
    return (
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {Array.from({ length: 6 }, (_, i) => (
          <div
            key={i}
            className="h-16 rounded-xl bg-zinc-100 dark:bg-zinc-800/50 animate-pulse"
          />
        ))}
      </div>
    );
  }

  const total = categories.reduce((sum, c) => sum + c.count, 0);

  return (
    <div className="space-y-3">
      <p className="text-xs text-zinc-500 dark:text-zinc-400">
        {total === 0
          ? t("library.inventory.allClear")
          : t("library.inventory.summary", { count: total })}
      </p>
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {categories.map((category) => {
          const Icon = ICONS[category.key] ?? AlertTriangle;
          const empty = category.count === 0;
          const active = activeKey === category.key;
          return (
            <button
              key={category.key}
              type="button"
              disabled={empty}
              aria-pressed={active}
              onClick={() => onSelect(category.key)}
              className={`flex items-start gap-3 p-3 rounded-xl border text-left transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                active
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/40"
                  : "border-zinc-200 dark:border-zinc-800 hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
              } ${empty ? "opacity-50 cursor-default hover:bg-transparent dark:hover:bg-transparent" : ""}`}
            >
              <Icon
                size={18}
                className={
                  empty
                    ? "mt-0.5 shrink-0 text-zinc-400"
                    : "mt-0.5 shrink-0 text-amber-500"
                }
                aria-hidden="true"
              />
              <span className="min-w-0">
                <span className="block text-sm font-medium text-zinc-900 dark:text-white">
                  {t(`library.inventory.categories.${category.key}.label`, {
                    defaultValue: category.key,
                  })}
                </span>
                <span className="block text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed">
                  {t(`library.inventory.categories.${category.key}.help`, {
                    defaultValue: "",
                  })}
                </span>
                <span className="block text-xs font-semibold text-zinc-700 dark:text-zinc-200 mt-1 tabular-nums">
                  {t("library.inventory.trackCount", { count: category.count })}
                </span>
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
