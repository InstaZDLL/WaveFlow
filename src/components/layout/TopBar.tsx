import { useState, useRef, useEffect, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
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
  PanelLeftClose,
  PanelLeftOpen,
  X,
  History,
} from "lucide-react";
import type { SidebarMode, ViewId } from "../../types";

/** What the sidebar button does next, from each width. */
const SIDEBAR_NEXT_LABEL: Record<SidebarMode, string> = {
  full: "topbar.collapseSidebar",
  icons: "topbar.hideSidebar",
  hidden: "topbar.showSidebar",
};
import { useTheme } from "../../hooks/useTheme";
import { useProfile } from "../../hooks/useProfile";
import { usePlayer } from "../../hooks/usePlayer";
import { useRecentSearches } from "../../hooks/useRecentSearches";
import { FOCUS_SEARCH_EVENT } from "../../lib/shortcuts";
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
import {
  listGenres,
  searchAlbums,
  searchArtists,
  type GenreRow,
  type AlbumRow,
  type ArtistRow,
} from "../../lib/tauri/browse";
import { useAlbumArtistContextMenu } from "../../hooks/useAlbumArtistContextMenu";

interface TopBarProps {
  activeView: ViewId;
  setActiveView: (view: ViewId) => void;
  onOpenProfileSelector: () => void;
  onNavigateToAlbum: (id: number) => void;
  onNavigateToArtist: (id: number) => void;
  canGoBack: boolean;
  canGoForward: boolean;
  onGoBack: () => void;
  onGoForward: () => void;
  sidebarMode: SidebarMode;
  onCycleSidebar: () => void;
}

export function TopBar({
  setActiveView,
  onOpenProfileSelector,
  onNavigateToAlbum,
  onNavigateToArtist,
  canGoBack,
  canGoForward,
  onGoBack,
  onGoForward,
  sidebarMode,
  onCycleSidebar,
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

  // Search state. Tracks come from FTS; albums + artists from the
  // dedicated `search_albums` / `search_artists` commands (FTS is
  // track-scoped). Only tracks honour the advanced filter panel.
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<Track[]>([]);
  const [albumResults, setAlbumResults] = useState<AlbumRow[]>([]);
  const [artistResults, setArtistResults] = useState<ArtistRow[]>([]);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const searchRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const { recent, remember, forget, clear } = useRecentSearches();
  // Read from `runSearch`, which is memoised without it.
  const recentCountRef = useRef(recent.length);
  // The query the shown results answer. During the 250 ms debounce the
  // field already holds the next one, which did not produce them.
  const resultsQueryRef = useRef("");
  useEffect(() => {
    recentCountRef.current = recent.length;
  }, [recent.length]);
  const searchTimerRef = useRef<number | null>(null);
  // Monotonic id of the latest issued search. Each `runSearch` bumps it
  // and captures its value; the async `Promise.all` callback only writes
  // state when its id is still current, so a slow older request can't
  // clobber the results of a newer one.
  const searchSeqRef = useRef(0);

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
      // Invalidate any in-flight older request up-front — covers both
      // the empty-query clear below and the fan-out further down.
      const seq = ++searchSeqRef.current;
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
        setAlbumResults([]);
        setArtistResults([]);
        // A field emptied while typing in it goes back to the recent
        // searches rather than closing.
        setIsSearchOpen(
          document.activeElement === searchInputRef.current &&
            recentCountRef.current > 0,
        );
        return;
      }

      // Advanced filters are track-only — when any is set we run the
      // advanced track search and drop the album/artist sections (those
      // entities don't carry BPM / format / year constraints). A plain
      // text query fans out across all three entities in parallel.
      const tracksPromise = anyFilter
        ? searchTracksAdvanced({
            ...currentFilters,
            query: trimmed.length > 0 ? trimmed : null,
          })
        : searchTracks(trimmed);
      const albumsPromise =
        !anyFilter && trimmed.length > 0
          ? searchAlbums(trimmed, null)
          : Promise.resolve<AlbumRow[]>([]);
      const artistsPromise =
        !anyFilter && trimmed.length > 0
          ? searchArtists(trimmed, null)
          : Promise.resolve<ArtistRow[]>([]);

      Promise.all([tracksPromise, albumsPromise, artistsPromise])
        .then(([tracks, albums, artists]) => {
          // Drop the response if a newer search has been issued since.
          if (seq !== searchSeqRef.current) return;
          resultsQueryRef.current = trimmed;
          setSearchResults(tracks);
          setAlbumResults(albums);
          setArtistResults(artists);
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

  const closeSearch = () => {
    // Cancel the pending debounce so a keystroke from just before the
    // navigation doesn't fire `runSearch` afterward and reopen the
    // dropdown (the timer closure captured the old query value).
    if (searchTimerRef.current != null) {
      window.clearTimeout(searchTimerRef.current);
      searchTimerRef.current = null;
    }
    // Invalidate any in-flight request so a slow response can't reopen
    // the dropdown after the user navigated away (e.g. clicked an album
    // row while the fan-out was still resolving).
    searchSeqRef.current += 1;
    setIsSearchOpen(false);
    setSearchQuery("");
    setSearchResults([]);
    setAlbumResults([]);
    setArtistResults([]);
  };

  // A search the user went somewhere from is worth offering again.
  const handleSearchResultClick = (tracks: Track[], index: number) => {
    remember(resultsQueryRef.current);
    playTracks(tracks, index, { type: "library", id: null });
    closeSearch();
  };

  // Right-click on an artist or album result: pin it to the sidebar.
  const resultMenu = useAlbumArtistContextMenu();
  const resultMenuOpen = resultMenu.isOpen;

  const handleAlbumResultClick = (id: number) => {
    remember(resultsQueryRef.current);
    onNavigateToAlbum(id);
    closeSearch();
  };

  const handleArtistResultClick = (id: number) => {
    remember(resultsQueryRef.current);
    onNavigateToArtist(id);
    closeSearch();
  };

  const runRecentSearch = (query: string) => {
    if (searchTimerRef.current != null) {
      window.clearTimeout(searchTimerRef.current);
      searchTimerRef.current = null;
    }
    // Focus first: it fires `onFocus`, whose reopening must not be the
    // last word in this batch.
    searchInputRef.current?.focus();
    setSearchQuery(query);
    // Closed until the results land (`runSearch` reopens it), so the
    // recent list does not give way to a passing "no result".
    setIsSearchOpen(false);
    runSearch(query, filters);
  };

  // An empty field with no filter shows the recent searches instead of
  // results.
  const showingRecent = searchQuery.trim() === "" && !hasActiveFilters;

  // Recent searches can land after the field took focus (the profile is
  // still being read): show them then, if nothing else claims the field.
  useEffect(() => {
    if (recent.length === 0) return;
    if (document.activeElement !== searchInputRef.current) return;
    if (searchQuery.trim() !== "" || hasActiveFilters) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setIsSearchOpen(true);
    // Keyed on the list's arrival only, not on typing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recent.length]);

  // Ctrl+K (rebindable) from anywhere in the window.
  useEffect(() => {
    const focus = () => {
      const input = searchInputRef.current;
      if (!input) return;
      input.focus();
      input.select();
    };
    window.addEventListener(FOCUS_SEARCH_EVENT, focus);
    return () => window.removeEventListener(FOCUS_SEARCH_EVENT, focus);
  }, []);

  // Close search dropdown / filter panel on click outside.
  useEffect(() => {
    if (!isSearchOpen && !filtersOpen) return;
    const handleClick = (e: MouseEvent) => {
      // A result's right-click menu is portalled out of the dropdown;
      // choosing in it is not a click away from the results.
      if ((e.target as Element | null)?.closest?.("[data-context-menu]"))
        return;
      if (searchRef.current && !searchRef.current.contains(e.target as Node)) {
        setIsSearchOpen(false);
        setFiltersOpen(false);
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      // Escape closes the result's menu first, and only that.
      if (e.key === "Escape" && !resultMenuOpen) {
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
  }, [isSearchOpen, filtersOpen, resultMenuOpen]);

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
    // `backdrop-blur-md` makes this header its own stacking context, so
    // every dropdown inside it (profile menu, notifications, search
    // results) is clamped to the header's own z-index no matter how high
    // their own is. At `z-10` that put them *below* content-level sticky
    // group headers (`z-20`, e.g. the date header in Recently Played),
    // which painted over the open profile menu (issue #390). `z-30` keeps
    // the header — and everything it contains — above content chrome while
    // staying below the PlayerBar (`z-50`); see the layer scale in app.css.
    <header className="h-16 flex items-center justify-between px-6 z-30 sticky top-0 bg-zinc-50/80 backdrop-blur-md dark:bg-zinc-900/80 wf-glass">
      {/* Navigation Arrows + sidebar toggle (#167). The toggle sits
          left of the back/forward chevrons because that's where the
          sidebar's right edge would be when visible — keeps muscle
          memory consistent. It cycles the three widths — full, icons
          only, hidden — and names the step it takes next. */}
      {/* `data-topbar-*` marks the zones — left, search, right, the
          popovers — for the skins that recompose the bar. */}
      <div data-topbar-left className="flex space-x-2">
        <button
          type="button"
          onClick={onCycleSidebar}
          aria-label={t(SIDEBAR_NEXT_LABEL[sidebarMode])}
          title={t(SIDEBAR_NEXT_LABEL[sidebarMode])}
          className="p-2 rounded-full border transition-colors border-zinc-200 bg-white text-zinc-600 hover:text-zinc-800 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-400 dark:hover:text-white"
        >
          {sidebarMode === "hidden" ? (
            <PanelLeftOpen size={20} />
          ) : (
            <PanelLeftClose size={20} />
          )}
        </button>
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
      <div
        data-topbar-search
        className="flex-1 max-w-xl mx-8 relative"
        ref={searchRef}
      >
        <div
          data-topbar-search-field
          className="flex items-center px-4 py-2.5 rounded-full border transition-all focus-within:ring-2 ring-emerald-500/20 bg-white border-zinc-200 dark:bg-zinc-800/50 dark:border-zinc-700 dark:text-zinc-200"
        >
          <Search size={18} className="text-zinc-400 mr-3" />
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={(e) => handleSearchInput(e.target.value)}
            onFocus={() => {
              if (
                searchResults.length +
                  albumResults.length +
                  artistResults.length >
                  0 ||
                (showingRecent && recent.length > 0)
              )
                setIsSearchOpen(true);
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
        <AnimatePresence>
          {filtersOpen && (
            <motion.div
              initial={{ opacity: 0, y: -4, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -2, scale: 0.99 }}
              transition={{
                type: "spring",
                stiffness: 520,
                damping: 32,
                mass: 0.5,
              }}
              style={{ transformOrigin: "top center" }}
              data-topbar-popover
              className="absolute top-full left-0 right-0 mt-2 z-50 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 p-5 max-h-[70vh] overflow-y-auto"
            >
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
                    updateFilter(
                      "hi_res_only",
                      filters.hi_res_only ? null : true,
                    )
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
            </motion.div>
          )}
        </AnimatePresence>

        {/* Search results dropdown */}
        <AnimatePresence>
          {isSearchOpen && !filtersOpen && (
            <motion.div
              initial={{ opacity: 0, y: -4, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -2, scale: 0.99 }}
              transition={{
                type: "spring",
                stiffness: 520,
                damping: 32,
                mass: 0.5,
              }}
              style={{ transformOrigin: "top center" }}
              data-topbar-popover
              className="absolute top-full left-0 right-0 mt-2 z-50 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden"
            >
              {showingRecent ? (
                recent.length > 0 && (
                  <div className="max-h-96 overflow-y-auto py-1">
                    <div className="flex items-center justify-between px-4 pt-2 pb-1">
                      <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-400">
                        {t("topbar.search.recent.title")}
                      </span>
                      <button
                        type="button"
                        onClick={() => {
                          clear();
                          setIsSearchOpen(false);
                        }}
                        className="text-xs text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 transition-colors"
                      >
                        {t("topbar.search.recent.clear")}
                      </button>
                    </div>
                    <ul>
                      {recent.map((query) => (
                        <li key={query} className="group flex items-center">
                          <button
                            type="button"
                            onClick={() => runRecentSearch(query)}
                            className="flex flex-1 min-w-0 items-center space-x-3 px-4 py-2 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors"
                          >
                            <History
                              size={16}
                              className="text-zinc-400 shrink-0"
                              aria-hidden="true"
                            />
                            <span
                              dir="auto"
                              className="truncate text-sm text-zinc-800 dark:text-zinc-200"
                            >
                              {query}
                            </span>
                          </button>
                          <button
                            type="button"
                            onClick={() => {
                              forget(query);
                              if (recent.length <= 1) setIsSearchOpen(false);
                            }}
                            aria-label={t("topbar.search.recent.remove", {
                              query,
                            })}
                            title={t("topbar.search.recent.remove", { query })}
                            className="mr-2 rounded-full p-1.5 text-zinc-400 opacity-0 reveal:opacity-100 hover:text-zinc-700 dark:hover:text-zinc-200 transition-opacity"
                          >
                            <X size={14} />
                          </button>
                        </li>
                      ))}
                    </ul>
                  </div>
                )
              ) : searchResults.length +
                  albumResults.length +
                  artistResults.length ===
                0 ? (
                <div className="px-4 py-6 text-center text-sm text-zinc-500">
                  {t("topbar.search.empty")}
                </div>
              ) : (
                <div className="max-h-96 overflow-y-auto">
                  {artistResults.length > 0 && (
                    <SearchSection label={t("topbar.search.artists")}>
                      {artistResults.map((artist) => (
                        <li key={`artist-${artist.id}`}>
                          <button
                            type="button"
                            onClick={() => handleArtistResultClick(artist.id)}
                            {...resultMenu.bind({
                              kind: "artist",
                              id: artist.id,
                            })}
                            className="flex w-full items-center space-x-3 px-4 py-2.5 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer transition-colors"
                          >
                            <Artwork
                              path={artist.artwork_path ?? artist.picture_path}
                              remoteUrl={artist.picture_url}
                              className="w-10 h-10"
                              iconSize={16}
                              alt={artist.name}
                              placeholderIcon={Users}
                            />
                            <div className="flex-1 min-w-0 text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                              {artist.name}
                            </div>
                          </button>
                        </li>
                      ))}
                    </SearchSection>
                  )}

                  {albumResults.length > 0 && (
                    <SearchSection label={t("topbar.search.albums")}>
                      {albumResults.map((album) => (
                        <li key={`album-${album.id}`}>
                          <button
                            type="button"
                            onClick={() => handleAlbumResultClick(album.id)}
                            {...resultMenu.bind({
                              kind: "album",
                              id: album.id,
                            })}
                            className="flex w-full items-center space-x-3 px-4 py-2.5 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer transition-colors"
                          >
                            <Artwork
                              path={album.artwork_path}
                              className="w-10 h-10"
                              iconSize={16}
                              alt={album.title}
                              rounded="md"
                            />
                            <div className="flex-1 min-w-0">
                              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                                {album.title}
                              </div>
                              <div className="text-xs text-zinc-500 truncate">
                                {album.artist_name ?? "—"}
                                {album.year != null ? ` · ${album.year}` : ""}
                              </div>
                            </div>
                          </button>
                        </li>
                      ))}
                    </SearchSection>
                  )}

                  {searchResults.length > 0 && (
                    <SearchSection label={t("topbar.search.titles")}>
                      {searchResults.map((track, index) => (
                        <li key={`track-${track.id}`}>
                          <button
                            type="button"
                            onClick={() =>
                              handleSearchResultClick(searchResults, index)
                            }
                            className="flex w-full items-center space-x-3 px-4 py-2.5 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60 cursor-pointer transition-colors"
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
                                {track.artist_name ?? "—"} ·{" "}
                                {track.album_title ?? "—"}
                              </div>
                            </div>
                            <span className="text-xs text-zinc-400 tabular-nums shrink-0">
                              {formatDuration(track.duration_ms)}
                            </span>
                          </button>
                        </li>
                      ))}
                    </SearchSection>
                  )}
                </div>
              )}
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* Right Actions */}
      <div data-topbar-right className="flex items-center space-x-4">
        {/* Theme Toggle */}
        <button
          type="button"
          data-topbar-theme
          onClick={(e) => toggleTheme(e)}
          aria-label={
            isDark
              ? t("topbar.theme.enableLight")
              : t("topbar.theme.enableDark")
          }
          aria-pressed={isDark}
          className={`relative w-14 h-8 rounded-full border transition-colors duration-500 ease-in-out ${
            isDark
              ? "bg-zinc-800 border-zinc-700"
              : "bg-white border-zinc-300 dark:bg-zinc-800 dark:border-zinc-700"
          }`}
        >
          <div
            className={`absolute top-1 left-1 w-6 h-6 rounded-full flex items-center justify-center transition-all duration-500 ease-in-out ${
              isDark
                ? "translate-x-6 bg-zinc-700 text-yellow-400"
                : "translate-x-0 bg-zinc-100 text-amber-500 dark:bg-zinc-700 dark:text-amber-400"
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
            data-topbar-profile
            aria-expanded={isProfileOpen}
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

          <AnimatePresence>
            {isProfileOpen && (
              <motion.div
                initial={{ opacity: 0, scale: 0.96, y: 4 }}
                animate={{ opacity: 1, scale: 1, y: 0 }}
                exit={{ opacity: 0, scale: 0.97, y: 2 }}
                transition={{
                  type: "spring",
                  stiffness: 520,
                  damping: 32,
                  mass: 0.45,
                }}
                style={{ transformOrigin: "top right" }}
                data-topbar-popover
                className="absolute top-full right-0 mt-2 w-56 rounded-xl shadow-lg border overflow-hidden z-50 bg-white border-zinc-200 dark:bg-zinc-800 dark:border-zinc-700"
              >
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
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      </div>
      {resultMenu.render()}
    </header>
  );
}

/** One labelled section of the global-search dropdown (Artists /
 *  Albums / Titles). Renders a sticky-ish uppercase header above its
 *  rows so the three entity groups stay visually distinct. */
function SearchSection({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="not-first:border-t not-first:border-zinc-100 dark:not-first:border-zinc-800/60">
      <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-4 pt-3 pb-1">
        {label}
      </div>
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-800/60">
        {children}
      </ul>
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
