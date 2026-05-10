import { useState, useRef, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronLeft,
  ChevronRight,
  Search,
  Sun,
  Moon,
  Users,
  BarChart2,
  Settings,
  MessageSquare,
  Info,
  LogOut,
  ChevronDown,
  ChevronUp,
  SlidersHorizontal,
  X,
} from "lucide-react";
import type { ViewId } from "../../types";
import { useTheme } from "../../hooks/useTheme";
import { useProfile } from "../../hooks/useProfile";
import { usePlayer } from "../../hooks/usePlayer";
import { getProfileColor, profileInitial } from "../../lib/profileColors";
import { MenuActionItem } from "../common/MenuActionItem";
import { Artwork } from "../common/Artwork";
import {
  searchTracks,
  searchTracksAdvanced,
  formatDuration,
  type SearchFilters,
  type Track,
} from "../../lib/tauri/track";
import { listGenres, type GenreRow } from "../../lib/tauri/browse";

interface TopBarProps {
  activeView: ViewId;
  setActiveView: (view: ViewId) => void;
  onOpenProfileSelector: () => void;
  canGoBack: boolean;
  canGoForward: boolean;
  onGoBack: () => void;
  onGoForward: () => void;
}

export function TopBar({
  setActiveView,
  onOpenProfileSelector,
  canGoBack,
  canGoForward,
  onGoBack,
  onGoForward,
}: TopBarProps) {
  const { t } = useTranslation();
  const { isDark, toggleTheme } = useTheme();
  const { activeProfile } = useProfile();
  const profileColor = getProfileColor(activeProfile?.color_id);
  const profileName = activeProfile?.name ?? "";
  const profileLetter = activeProfile ? profileInitial(activeProfile.name) : "";
  const { playTracks } = usePlayer();
  const [isProfileOpen, setIsProfileOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Search state
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<Track[]>([]);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const searchRef = useRef<HTMLDivElement>(null);
  const searchTimerRef = useRef<number | null>(null);

  // Advanced filters state. Empty/null fields mean "no constraint".
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [filters, setFilters] = useState<SearchFilters>({});
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [genresLoaded, setGenresLoaded] = useState(false);

  // Lazy-load the genre list the first time the user opens the panel.
  // Genre lists can grow large in eclectic libraries; no point fetching
  // up-front for users who never touch advanced search.
  useEffect(() => {
    if (!filtersOpen || genresLoaded) return;
    listGenres(null)
      .then((g) => {
        setGenres(g);
        setGenresLoaded(true);
      })
      .catch((err) => console.error("[TopBar] listGenres failed", err));
  }, [filtersOpen, genresLoaded]);

  const hasActiveFilters =
    (filters.genre_ids?.length ?? 0) > 0 ||
    filters.year_min != null ||
    filters.year_max != null ||
    filters.bpm_min != null ||
    filters.bpm_max != null ||
    filters.duration_min_ms != null ||
    filters.duration_max_ms != null ||
    (filters.formats?.length ?? 0) > 0 ||
    filters.min_sample_rate != null ||
    filters.min_bit_depth != null ||
    filters.hi_res_only === true ||
    filters.liked_only === true;

  const runSearch = useCallback(
    (query: string, currentFilters: SearchFilters) => {
      const trimmed = query.trim();
      const anyFilter =
        (currentFilters.genre_ids?.length ?? 0) > 0 ||
        currentFilters.year_min != null ||
        currentFilters.year_max != null ||
        currentFilters.bpm_min != null ||
        currentFilters.bpm_max != null ||
        currentFilters.duration_min_ms != null ||
        currentFilters.duration_max_ms != null ||
        (currentFilters.formats?.length ?? 0) > 0 ||
        currentFilters.min_sample_rate != null ||
        currentFilters.min_bit_depth != null ||
        currentFilters.hi_res_only === true ||
        currentFilters.liked_only === true;

      if (trimmed.length === 0 && !anyFilter) {
        setSearchResults([]);
        setIsSearchOpen(false);
        return;
      }

      const promise = anyFilter
        ? searchTracksAdvanced({
            ...currentFilters,
            query: trimmed.length > 0 ? trimmed : null,
          })
        : searchTracks(trimmed);

      promise
        .then((results) => {
          setSearchResults(results);
          setIsSearchOpen(true);
        })
        .catch((err) => console.error("[TopBar] search failed", err));
    },
    [],
  );

  const handleSearchInput = useCallback(
    (value: string) => {
      setSearchQuery(value);
      if (searchTimerRef.current != null) {
        window.clearTimeout(searchTimerRef.current);
      }
      searchTimerRef.current = window.setTimeout(() => {
        runSearch(value, filters);
      }, 250);
    },
    [filters, runSearch],
  );

  // Re-run the current search whenever the filter set changes so the
  // dropdown reflects the new constraints without the user having to
  // re-type anything. Debounced through a timer so the state writes
  // happen outside the effect body (cascading-render lint).
  useEffect(() => {
    const handle = window.setTimeout(() => {
      runSearch(searchQuery, filters);
    }, 0);
    return () => window.clearTimeout(handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filters]);

  const updateFilter = <K extends keyof SearchFilters>(
    key: K,
    value: SearchFilters[K],
  ) => {
    setFilters((prev) => ({ ...prev, [key]: value }));
  };

  const toggleGenre = (id: number) => {
    setFilters((prev) => {
      const current = prev.genre_ids ?? [];
      const next = current.includes(id)
        ? current.filter((g) => g !== id)
        : [...current, id];
      return { ...prev, genre_ids: next.length > 0 ? next : null };
    });
  };

  const toggleFormat = (fmt: string) => {
    setFilters((prev) => {
      const current = prev.formats ?? [];
      const next = current.includes(fmt)
        ? current.filter((f) => f !== fmt)
        : [...current, fmt];
      return { ...prev, formats: next.length > 0 ? next : null };
    });
  };

  const resetFilters = () => setFilters({});

  const handleSearchResultClick = (tracks: Track[], index: number) => {
    playTracks(tracks, index, { type: "library", id: null });
    setIsSearchOpen(false);
    setSearchQuery("");
    setSearchResults([]);
  };

  // Close search dropdown / filter panel on click outside.
  useEffect(() => {
    if (!isSearchOpen && !filtersOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (searchRef.current && !searchRef.current.contains(e.target as Node)) {
        setIsSearchOpen(false);
        setFiltersOpen(false);
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setIsSearchOpen(false);
        setFiltersOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [isSearchOpen, filtersOpen]);

  // Close dropdown on click outside
  useEffect(() => {
    if (!isProfileOpen) return;
    const handleClick = (e: MouseEvent) => {
      if (
        dropdownRef.current &&
        !dropdownRef.current.contains(e.target as Node)
      ) {
        setIsProfileOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [isProfileOpen]);

  const handleMenuNav = (view: ViewId) => {
    setActiveView(view);
    setIsProfileOpen(false);
  };

  const handleQuit = async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      getCurrentWindow().close();
    } catch {
      window.close();
    }
  };

  return (
    <div className="h-20 flex items-center justify-between px-8 z-10 sticky top-0 bg-zinc-50/80 backdrop-blur-md dark:bg-zinc-900/80">
      {/* Navigation Arrows */}
      <div className="flex space-x-2">
        <button
          onClick={onGoBack}
          disabled={!canGoBack}
          className={`p-2 rounded-full border transition-colors border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 ${
            canGoBack
              ? "text-zinc-600 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-white"
              : "text-zinc-300 cursor-not-allowed dark:text-zinc-600"
          }`}
        >
          <ChevronLeft size={20} />
        </button>
        <button
          onClick={onGoForward}
          disabled={!canGoForward}
          className={`p-2 rounded-full border transition-colors border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 ${
            canGoForward
              ? "text-zinc-600 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-white"
              : "text-zinc-300 cursor-not-allowed dark:text-zinc-600"
          }`}
        >
          <ChevronRight size={20} />
        </button>
      </div>

      {/* Search Bar */}
      <div className="flex-1 max-w-xl mx-8 relative" ref={searchRef}>
        <div className="flex items-center px-4 py-2.5 rounded-full border transition-all focus-within:ring-2 ring-emerald-500/20 bg-white border-zinc-200 dark:bg-zinc-800/50 dark:border-zinc-700 dark:text-zinc-200">
          <Search size={18} className="text-zinc-400 mr-3" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => handleSearchInput(e.target.value)}
            onFocus={() => {
              if (searchResults.length > 0) setIsSearchOpen(true);
            }}
            placeholder={t("topbar.search.placeholder")}
            className="bg-transparent border-none outline-none w-full text-sm placeholder-zinc-400"
          />
          <button
            type="button"
            onClick={() => {
              setFiltersOpen((v) => !v);
              if (!filtersOpen) setIsSearchOpen(false);
            }}
            aria-label={t("topbar.search.filters.toggle")}
            title={t("topbar.search.filters.toggle")}
            className={`relative ml-2 p-1.5 rounded-full transition-colors ${
              filtersOpen || hasActiveFilters
                ? "text-emerald-500 bg-emerald-500/10"
                : "text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
            }`}
          >
            <SlidersHorizontal size={16} />
            {hasActiveFilters && !filtersOpen && (
              <span className="absolute -top-0.5 -right-0.5 w-2 h-2 rounded-full bg-emerald-500" />
            )}
          </button>
        </div>

        {/* Advanced filter panel */}
        {filtersOpen && (
          <div className="absolute top-full left-0 right-0 mt-2 z-50 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 p-5 animate-fade-in max-h-[70vh] overflow-y-auto">
            <div className="flex items-center justify-between mb-4">
              <span className="text-xs font-bold tracking-widest text-zinc-500 uppercase">
                {t("topbar.search.filters.title")}
              </span>
              <div className="flex items-center gap-2">
                {hasActiveFilters && (
                  <button
                    type="button"
                    onClick={resetFilters}
                    className="text-xs text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 px-2 py-1 rounded"
                  >
                    {t("topbar.search.filters.reset")}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setFiltersOpen(false)}
                  className="p-1 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-500"
                  aria-label={t("topbar.search.filters.close")}
                >
                  <X size={14} />
                </button>
              </div>
            </div>

            {/* Quick toggles */}
            <div className="flex flex-wrap gap-2 mb-4">
              <FilterChip
                active={filters.hi_res_only === true}
                onClick={() =>
                  updateFilter("hi_res_only", filters.hi_res_only ? null : true)
                }
                label={t("topbar.search.filters.hiRes")}
              />
              <FilterChip
                active={filters.liked_only === true}
                onClick={() =>
                  updateFilter("liked_only", filters.liked_only ? null : true)
                }
                label={t("topbar.search.filters.likedOnly")}
              />
            </div>

            {/* Year range */}
            <FilterRow label={t("topbar.search.filters.year")}>
              <RangeInput
                min={filters.year_min ?? null}
                max={filters.year_max ?? null}
                onMin={(v) => updateFilter("year_min", v)}
                onMax={(v) => updateFilter("year_max", v)}
                placeholderMin="1900"
                placeholderMax="2099"
              />
            </FilterRow>

            {/* BPM range */}
            <FilterRow label={t("topbar.search.filters.bpm")}>
              <RangeInput
                min={filters.bpm_min ?? null}
                max={filters.bpm_max ?? null}
                onMin={(v) => updateFilter("bpm_min", v)}
                onMax={(v) => updateFilter("bpm_max", v)}
                placeholderMin="40"
                placeholderMax="220"
              />
            </FilterRow>

            {/* Duration range (in minutes for ergonomics) */}
            <FilterRow label={t("topbar.search.filters.durationMin")}>
              <RangeInput
                min={
                  filters.duration_min_ms != null
                    ? Math.round(filters.duration_min_ms / 60_000)
                    : null
                }
                max={
                  filters.duration_max_ms != null
                    ? Math.round(filters.duration_max_ms / 60_000)
                    : null
                }
                onMin={(v) =>
                  updateFilter(
                    "duration_min_ms",
                    v != null ? Math.round(v * 60_000) : null,
                  )
                }
                onMax={(v) =>
                  updateFilter(
                    "duration_max_ms",
                    v != null ? Math.round(v * 60_000) : null,
                  )
                }
                placeholderMin="0"
                placeholderMax="60"
              />
            </FilterRow>

            {/* Format chips */}
            <FilterRow label={t("topbar.search.filters.format")}>
              <div className="flex flex-wrap gap-1.5">
                {[
                  "FLAC",
                  "WAV",
                  "AIFF",
                  "ALAC",
                  "DSF",
                  "DFF",
                  "MP3",
                  "AAC",
                  "OGG",
                  "OPUS",
                ].map((fmt) => (
                  <FilterChip
                    key={fmt}
                    active={(filters.formats ?? []).includes(fmt)}
                    onClick={() => toggleFormat(fmt)}
                    label={fmt}
                    compact
                  />
                ))}
              </div>
            </FilterRow>

            {/* Genres */}
            {genres.length > 0 && (
              <FilterRow label={t("topbar.search.filters.genres")}>
                <div className="flex flex-wrap gap-1.5 max-h-40 overflow-y-auto">
                  {genres.map((g) => (
                    <FilterChip
                      key={g.id}
                      active={(filters.genre_ids ?? []).includes(g.id)}
                      onClick={() => toggleGenre(g.id)}
                      label={g.name}
                      compact
                    />
                  ))}
                </div>
              </FilterRow>
            )}
          </div>
        )}

        {/* Search results dropdown */}
        {isSearchOpen && !filtersOpen && (
          <div className="absolute top-full left-0 right-0 mt-2 z-50 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in">
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-4 pt-3 pb-2">
              {t("topbar.search.results", { count: searchResults.length })}
            </div>
            {searchResults.length === 0 ? (
              <div className="px-4 py-6 text-center text-sm text-zinc-500">
                {t("topbar.search.empty")}
              </div>
            ) : (
              <ul className="max-h-80 overflow-y-auto divide-y divide-zinc-100 dark:divide-zinc-800/60">
                {searchResults.map((track, index) => (
                  <li
                    key={track.id}
                    onClick={() =>
                      handleSearchResultClick(searchResults, index)
                    }
                    className="flex items-center space-x-3 px-4 py-2.5 hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer transition-colors"
                  >
                    <Artwork
                      path={track.artwork_path}
                      className="w-10 h-10"
                      iconSize={16}
                      alt={track.title}
                      rounded="md"
                    />
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                        {track.title}
                      </div>
                      <div className="text-xs text-zinc-500 truncate">
                        {track.artist_name ?? "—"} · {track.album_title ?? "—"}
                      </div>
                    </div>
                    <span className="text-xs text-zinc-400 tabular-nums shrink-0">
                      {formatDuration(track.duration_ms)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>

      {/* Right Actions */}
      <div className="flex items-center space-x-4">
        {/* Theme Toggle */}
        <button
          type="button"
          onClick={(e) => toggleTheme(e)}
          aria-label={
            isDark
              ? t("topbar.theme.enableLight")
              : t("topbar.theme.enableDark")
          }
          aria-pressed={isDark}
          className={`relative w-14 h-8 rounded-full border transition-colors duration-500 ease-in-out ${
            isDark ? "bg-zinc-800 border-zinc-700" : "bg-white border-zinc-300"
          }`}
        >
          <div
            className={`absolute top-1 left-1 w-6 h-6 rounded-full flex items-center justify-center transition-all duration-500 ease-in-out ${
              isDark
                ? "translate-x-6 bg-zinc-700 text-yellow-400"
                : "translate-x-0 bg-zinc-100 text-amber-500"
            }`}
          >
            <Sun
              size={14}
              className={`absolute transition-all duration-500 ${
                isDark
                  ? "opacity-0 rotate-90 scale-50"
                  : "opacity-100 rotate-0 scale-100"
              }`}
            />
            <Moon
              size={14}
              className={`absolute transition-all duration-500 ${
                isDark
                  ? "opacity-100 rotate-0 scale-100"
                  : "opacity-0 -rotate-90 scale-50"
              }`}
            />
          </div>
        </button>

        {/* Profile Dropdown */}
        <div className="relative" ref={dropdownRef}>
          <button
            onClick={() => setIsProfileOpen(!isProfileOpen)}
            className={`flex items-center space-x-2 px-3 py-1.5 rounded-full border transition-colors
              ${
                isProfileOpen
                  ? "border-zinc-300 bg-zinc-100 text-zinc-800 dark:border-zinc-600 dark:bg-zinc-700 dark:text-zinc-200"
                  : "border-zinc-200 bg-white hover:bg-zinc-50 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-200"
              }`}
          >
            <div
              className={`w-6 h-6 rounded-full ${profileColor.avatarBg} ${profileColor.avatarText} flex items-center justify-center text-xs font-bold`}
            >
              {profileLetter}
            </div>
            <span className="text-sm font-medium">{profileName}</span>
            {isProfileOpen ? (
              <ChevronUp size={14} className="text-zinc-400" />
            ) : (
              <ChevronDown size={14} className="text-zinc-400" />
            )}
          </button>

          {isProfileOpen && (
            <div className="absolute top-full right-0 mt-2 w-56 rounded-xl shadow-lg border overflow-hidden z-50 bg-white border-zinc-200 dark:bg-zinc-800 dark:border-zinc-700 animate-fade-in">
              {/* Profile Header */}
              <div className="p-4 flex items-center space-x-3">
                <div
                  className={`w-10 h-10 rounded-full ${profileColor.avatarBg} ${profileColor.avatarText} flex items-center justify-center font-bold text-lg shadow-sm`}
                >
                  {profileLetter}
                </div>
                <div className="flex flex-col text-left min-w-0">
                  <div className="font-semibold text-sm text-zinc-900 dark:text-white truncate">
                    {profileName}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("topbar.profile.user")}
                  </div>
                </div>
              </div>

              <div className="border-t py-2 border-zinc-100 dark:border-zinc-700">
                <MenuActionItem
                  icon={<Users size={16} />}
                  label={t("topbar.profile.changeProfile")}
                  onClick={() => {
                    onOpenProfileSelector();
                    setIsProfileOpen(false);
                  }}
                />
                <MenuActionItem
                  icon={<BarChart2 size={16} />}
                  label={t("topbar.profile.statistics")}
                  onClick={() => handleMenuNav("statistics")}
                />
                <MenuActionItem
                  icon={<Settings size={16} />}
                  label={t("topbar.profile.settings")}
                  onClick={() => handleMenuNav("settings")}
                />
                <MenuActionItem
                  icon={<MessageSquare size={16} />}
                  label={t("topbar.profile.feedback")}
                  onClick={() => handleMenuNav("feedback")}
                />
                <MenuActionItem
                  icon={<Info size={16} />}
                  label={t("topbar.profile.about")}
                  onClick={() => handleMenuNav("about")}
                />
              </div>

              <div className="border-t py-2 border-zinc-100 dark:border-zinc-700">
                <MenuActionItem
                  icon={<LogOut size={16} />}
                  label={t("topbar.profile.quit")}
                  danger
                  onClick={handleQuit}
                />
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

interface FilterChipProps {
  active: boolean;
  onClick: () => void;
  label: string;
  compact?: boolean;
}

function FilterChip({ active, onClick, label, compact }: FilterChipProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`${compact ? "text-[11px] px-2 py-0.5" : "text-xs px-3 py-1"} rounded-full border transition-colors ${
        active
          ? "bg-emerald-500 border-emerald-500 text-white"
          : "border-zinc-200 text-zinc-600 hover:border-zinc-300 dark:border-zinc-700 dark:text-zinc-300 dark:hover:border-zinc-600"
      }`}
    >
      {label}
    </button>
  );
}

function FilterRow({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-3 last:mb-0">
      <div className="text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-1.5">
        {label}
      </div>
      {children}
    </div>
  );
}

interface RangeInputProps {
  min: number | null;
  max: number | null;
  onMin: (v: number | null) => void;
  onMax: (v: number | null) => void;
  placeholderMin: string;
  placeholderMax: string;
}

function RangeInput({
  min,
  max,
  onMin,
  onMax,
  placeholderMin,
  placeholderMax,
}: RangeInputProps) {
  const parse = (raw: string): number | null => {
    const trimmed = raw.trim();
    if (trimmed.length === 0) return null;
    const n = Number(trimmed);
    return Number.isFinite(n) ? n : null;
  };
  return (
    <div className="flex items-center gap-2">
      <input
        type="number"
        value={min ?? ""}
        onChange={(e) => onMin(parse(e.target.value))}
        placeholder={placeholderMin}
        className="w-24 text-xs px-2 py-1 rounded border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
      />
      <span className="text-xs text-zinc-400">→</span>
      <input
        type="number"
        value={max ?? ""}
        onChange={(e) => onMax(parse(e.target.value))}
        placeholder={placeholderMax}
        className="w-24 text-xs px-2 py-1 rounded border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-200"
      />
    </div>
  );
}
