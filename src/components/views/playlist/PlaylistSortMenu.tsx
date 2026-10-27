import { type PlaylistSortMode } from "./playlistTypes";
import { useTranslation } from "react-i18next";
import { useEffect, useRef, useState } from "react";
import { ArrowUpDown, Check } from "lucide-react";

// =============================================================================
// Playlist sort menu (Spotify-style: list of modes + check, no direction)
// =============================================================================

interface PlaylistSortMenuProps {
  current: PlaylistSortMode;
  /** The modes this playlist's source can actually back. */
  modes: ReadonlyArray<PlaylistSortMode>;
  onChange: (next: PlaylistSortMode) => void;
  // i18next's `t` is heavily overloaded — accept it whole rather than
  // re-declaring a subset that the type checker would reject.
  t: ReturnType<typeof useTranslation>["t"];
}

export function PlaylistSortMenu({
  current,
  modes,
  onChange,
  t,
}: PlaylistSortMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", onClickOutside);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClickOutside);
      document.removeEventListener("keydown", onKey);
    };
  }, [isOpen]);

  // i18n labels with inline fallbacks so the menu works in every locale
  // without forcing a 17-file translation pass for keys most users
  // never see. The fallback strings stay in English so the option set
  // is at least intelligible if a translation drops a key.
  const labels: Record<PlaylistSortMode, string> = {
    custom: t("sort.customOrder", "Custom order"),
    title: t("sort.title"),
    artist: t("sort.artist"),
    album: t("sort.album"),
    added_at: t("sort.recentlyAdded", "Recently added"),
    duration_ms: t("sort.duration"),
    filename: t("sort.filename"),
  };

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((p) => !p)}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        className="flex items-center space-x-2 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
      >
        <ArrowUpDown size={14} />
        <span>{labels[current]}</span>
      </button>
      {isOpen && (
        <ul
          role="listbox"
          className="absolute top-full right-0 mt-2 min-w-56 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated overflow-hidden z-50 animate-fade-in py-1"
        >
          <li
            className="px-4 pt-1 pb-2 text-[10px] font-bold tracking-widest text-zinc-400 uppercase"
            aria-hidden="true"
          >
            {t("sort.menuTitle", "Sort by")}
          </li>
          {modes.map((mode) => {
            const isSelected = mode === current;
            return (
              <li key={mode} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => {
                    onChange(mode);
                    setIsOpen(false);
                  }}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30"
                  }`}
                >
                  <span>{labels[mode]}</span>
                  {isSelected && <Check size={14} />}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
