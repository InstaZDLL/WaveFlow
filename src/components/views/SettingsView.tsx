import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  Globe,
  Power,
  Minimize2,
  ScanLine,
  RefreshCcw,
  ImageIcon,
  FolderOpen,
  Trash2,
  ChevronDown,
  Check,
  Volume2,
  Headphones,
  Shuffle,
  Radio,
  Eye,
  EyeOff,
  MousePointerClick,
  Sparkles,
  Gamepad2,
  FileText,
  Copy,
  Check as CheckIcon,
  Mic2,
  Server,
  ChevronsRight,
  WifiOff,
  Download,
  Upload,
  Activity,
  Gauge,
  ZoomIn,
  Library,
  Disc3,
  Zap,
  Palette,
  Database,
  Keyboard,
  Stethoscope,
} from "lucide-react";
import { useTheme } from "../../hooks/useTheme";
import { THEME_PRESETS } from "../../lib/themes";
import { getProfileSetting, setProfileSetting } from "../../lib/tauri/profile";
import type { ViewId } from "../../types";
import {
  SUPPORTED_LANGUAGES,
  normalizeSupportedLanguageCode,
} from "../../i18n";
import {
  playerGetAudioSettings,
  playerSetNormalize,
  playerSetMono,
  playerSetCrossfade,
  playerSetGapless,
  playerSetReplayGain,
} from "../../lib/tauri/player";
import {
  getDiscordRpcEnabled,
  getLastfmApiKey,
  getLastfmApiSecret,
  lastfmGetStatus,
  lastfmLogin,
  lastfmLogout,
  setDiscordRpcEnabled,
  setLastfmApiKey,
  setLastfmApiSecret,
  type LastfmStatus,
} from "../../lib/tauri/integration";
import {
  getSpotifyClientId,
  setSpotifyClientId,
  spotifyGetStatus,
  spotifyLogin,
  spotifyLogout,
  type SpotifyStatus,
} from "../../lib/tauri/spotify";
import {
  batchFetchMissingAlbumCovers,
  batchFetchMissingArtistPictures,
} from "../../lib/tauri/deezer";
import { openLogFolder, readRecentLogs } from "../../lib/tauri/diagnostics";
import { getOfflineMode, setOfflineMode } from "../../lib/tauri/offline";
import { exportProfile, importProfile } from "../../lib/tauri/profile_io";
import { pickFile, pickSaveFile } from "../../lib/tauri/dialog";
import {
  getVisualizerEnabled,
  setVisualizerEnabled,
} from "../../lib/tauri/visualizer";
import {
  getSmartCrossfade,
  setSmartCrossfade,
  getDynamicCrossfade,
  setDynamicCrossfade,
} from "../../lib/tauri/smartCrossfade";
import {
  dlnaGetConfig,
  dlnaGetStatus,
  dlnaSetConfig,
  type DlnaConfig,
  type DlnaStatus,
} from "../../lib/tauri/dlna";
import { listen } from "@tauri-apps/api/event";
import { useLibrary } from "../../hooks/useLibrary";
import { useProfile } from "../../hooks/useProfile";
import { invoke } from "@tauri-apps/api/core";
import {
  regenerateThumbnails,
  rescanLocalArtistImages,
} from "../../lib/tauri/library";
import {
  getAutoStart,
  getMinimizeToTray,
  getUiZoom,
  setAutoStart as persistAutoStart,
  setMinimizeToTray as persistMinimizeToTray,
  UI_ZOOM_CHANGED_EVENT,
  UI_ZOOM_MAX,
  UI_ZOOM_MIN,
  UI_ZOOM_STEP,
} from "../../lib/tauri/preferences";
import { applyUiZoom } from "../../hooks/useUiZoom";
import { DuplicatesModal } from "../common/DuplicatesModal";
import { BackupCard } from "./settings/BackupCard";
import { EqualizerCard } from "./settings/EqualizerCard";
import { ExclusiveModeCard } from "./settings/ExclusiveModeCard";
import { PlayerBarLayoutCard } from "./settings/PlayerBarLayoutCard";
import { ShortcutsCard } from "./settings/ShortcutsCard";
import { WrappedBannerCard } from "./settings/WrappedBannerCard";

interface SettingsViewProps {
  onNavigate: (view: ViewId) => void;
}

/**
 * Settings categories — surfaced as a horizontal tab bar at the top
 * of the page (Lokal-style). Only one section is mounted at a time
 * so heavy subviews (EQ visualizer, backup card, shortcuts editor)
 * don't run their effects until the user actually opens that tab.
 */
type SettingsCategory =
  | "library"
  | "playback"
  | "integrations"
  | "appearance"
  | "data"
  | "shortcuts"
  | "diagnostics";

const SETTINGS_CATEGORIES: ReadonlyArray<{
  id: SettingsCategory;
  labelKey: string;
  Icon: typeof Library;
}> = [
  { id: "library", labelKey: "settings.sections.library", Icon: Library },
  { id: "playback", labelKey: "settings.sections.playback", Icon: Disc3 },
  {
    id: "integrations",
    labelKey: "settings.sections.integrations",
    Icon: Zap,
  },
  {
    id: "appearance",
    labelKey: "settings.sections.appearance",
    Icon: Palette,
  },
  { id: "data", labelKey: "settings.sections.data", Icon: Database },
  {
    id: "shortcuts",
    labelKey: "settings.sections.shortcuts",
    Icon: Keyboard,
  },
  {
    id: "diagnostics",
    labelKey: "settings.sections.diagnostics",
    Icon: Stethoscope,
  },
];

const SETTINGS_CATEGORY_STORAGE_KEY = "waveflow.settings.activeCategory";
const SETTINGS_CATEGORY_IDS = new Set<SettingsCategory>(
  SETTINGS_CATEGORIES.map((c) => c.id),
);

function readStoredCategory(): SettingsCategory {
  if (typeof window === "undefined") return "library";
  try {
    const stored = window.localStorage.getItem(SETTINGS_CATEGORY_STORAGE_KEY);
    if (stored && SETTINGS_CATEGORY_IDS.has(stored as SettingsCategory)) {
      return stored as SettingsCategory;
    }
  } catch {
    // localStorage unavailable — fall through to default.
  }
  return "library";
}

function ToggleSwitch({
  enabled,
  onToggle,
  label,
}: {
  enabled: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      role="switch"
      aria-checked={enabled}
      aria-label={label}
      className={`relative w-12 h-7 rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 dark:focus-visible:ring-offset-zinc-900 ${
        enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600"
      }`}
    >
      <div
        className={`absolute top-0.5 w-6 h-6 rounded-full bg-white shadow-sm transition-transform ${
          enabled ? "left-[calc(100%-1.625rem)]" : "left-0.5"
        }`}
      />
    </button>
  );
}

interface LanguageDropdownProps {
  currentCode: string;
  onSelect: (code: string) => void;
}

function LanguageDropdown({ currentCode, onSelect }: LanguageDropdownProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const optionRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const normalizedCurrentCode = normalizeSupportedLanguageCode(currentCode);

  const currentLanguage =
    SUPPORTED_LANGUAGES.find((lang) => lang.code === normalizedCurrentCode) ??
    SUPPORTED_LANGUAGES[0];

  // Click-outside + Escape handling
  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };

    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setIsOpen(false);
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKey);
    };
  }, [isOpen]);

  // Keep keyboard focus on the highlighted option when it changes
  useEffect(() => {
    if (isOpen) {
      optionRefs.current[focusedIndex]?.focus();
    }
  }, [isOpen, focusedIndex]);

  const handleTriggerClick = () => {
    setIsOpen((prev) => {
      if (!prev) {
        // Opening: focus the currently selected option
        const initialIndex = Math.max(
          0,
          SUPPORTED_LANGUAGES.findIndex(
            (lang) => lang.code === normalizedCurrentCode,
          ),
        );
        setFocusedIndex(initialIndex);
      }
      return !prev;
    });
  };

  const handleOptionKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setFocusedIndex((index + 1) % SUPPORTED_LANGUAGES.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setFocusedIndex(
        (index - 1 + SUPPORTED_LANGUAGES.length) % SUPPORTED_LANGUAGES.length,
      );
    } else if (event.key === "Home") {
      event.preventDefault();
      setFocusedIndex(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setFocusedIndex(SUPPORTED_LANGUAGES.length - 1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleSelect(SUPPORTED_LANGUAGES[index].code);
    }
  };

  const handleSelect = (code: string) => {
    onSelect(code);
    setIsOpen(false);
  };

  return (
    <div className="relative" ref={containerRef}>
      <button
        type="button"
        onClick={handleTriggerClick}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        aria-label={t("settings.language.title")}
        className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
      >
        <span>{currentLanguage.nativeLabel}</span>
        <ChevronDown
          size={14}
          className={`transition-transform ${isOpen ? "rotate-180" : ""}`}
        />
      </button>

      {isOpen && (
        <ul
          role="listbox"
          aria-label={t("settings.language.title")}
          className="absolute top-full right-0 mt-2 min-w-48 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden z-50 animate-fade-in py-1"
        >
          {SUPPORTED_LANGUAGES.map((lang, index) => {
            const isSelected = lang.code === normalizedCurrentCode;
            return (
              <li key={lang.code} role="presentation">
                <button
                  ref={(el) => {
                    optionRefs.current[index] = el;
                  }}
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => handleSelect(lang.code)}
                  onKeyDown={(event) => handleOptionKeyDown(event, index)}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors focus:outline-none ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30 focus:bg-zinc-50 dark:focus:bg-zinc-700/30"
                  }`}
                >
                  <span>{lang.nativeLabel}</span>
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

export function SettingsView({ onNavigate }: SettingsViewProps) {
  const { t, i18n } = useTranslation();
  const { theme, setThemeId } = useTheme();
  const { libraries, rescanLibrary } = useLibrary();
  // Category tab the user is currently viewing. Persisted to
  // localStorage so re-entering Settings lands them on the same tab
  // they last had open — small but expected polish.
  const [activeCategory, setActiveCategory] =
    useState<SettingsCategory>(readStoredCategory);
  const handleCategoryChange = useCallback((next: SettingsCategory) => {
    setActiveCategory(next);
    try {
      window.localStorage.setItem(SETTINGS_CATEGORY_STORAGE_KEY, next);
    } catch {
      // localStorage unavailable — not worth surfacing.
    }
  }, []);
  const [isAnalyzingLib, setIsAnalyzingLib] = useState(false);
  const [analyzeProgress, setAnalyzeProgress] = useState<{
    processed: number;
    total: number;
    failed: number;
  } | null>(null);
  const [autoAnalyze, setAutoAnalyzeState] = useState(false);

  // Hydrate the auto-analyze flag once at mount. The setter below
  // handles flips optimistically + rollback on failure so the toggle
  // never feels laggy.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const { getAutoAnalyze } = await import("../../lib/tauri/analysis");
        const v = await getAutoAnalyze();
        if (!cancelled) setAutoAnalyzeState(v);
      } catch (err) {
        console.error("[SettingsView] get auto_analyze failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleToggleAutoAnalyze = useCallback(() => {
    const next = !autoAnalyze;
    setAutoAnalyzeState(next);
    void (async () => {
      try {
        const { setAutoAnalyze } = await import("../../lib/tauri/analysis");
        await setAutoAnalyze(next);
      } catch (err) {
        console.error("[SettingsView] set auto_analyze failed", err);
        setAutoAnalyzeState(!next);
      }
    })();
  }, [autoAnalyze]);
  const { activeProfile } = useProfile();
  const [isRescanning, setIsRescanning] = useState(false);
  const [autoStart, setAutoStart] = useState(false);
  const [minimizeToTray, setMinimizeToTray] = useState(true);
  // UI zoom slider value. Hydrated from `app_setting` and kept in
  // sync with the `Ctrl+=` / `Ctrl+-` / `Ctrl+0` shortcuts via the
  // window-level event the `useUiZoom` hook broadcasts.
  const [uiZoom, setUiZoom] = useState(1);
  const [scanOnStart, setScanOnStart] = useState(false);
  const [singleClickPlay, setSingleClickPlay] = useState(false);
  // Per-profile toggle for the Spotify sidebar entry. Default ON;
  // hide it for profiles that never use Spotify.
  const [showSpotify, setShowSpotify] = useState(true);
  const [isDuplicatesOpen, setIsDuplicatesOpen] = useState(false);
  // Status of the last "Copy logs" click — null when idle, "ok" or
  // "fail" briefly during the toast period before clearing back to null.
  const [copyLogsStatus, setCopyLogsStatus] = useState<"ok" | "fail" | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    getMinimizeToTray()
      .then((v) => {
        if (cancelled) return;
        setMinimizeToTray(v);
      })
      .catch((err) => console.error("[Settings] load minimize_to_tray", err));
    getAutoStart()
      .then((v) => {
        if (cancelled) return;
        setAutoStart(v);
      })
      .catch((err) => console.error("[Settings] load auto_start", err));
    getUiZoom()
      .then((v) => {
        if (cancelled) return;
        setUiZoom(v);
      })
      .catch((err) => console.error("[Settings] load ui_zoom", err));
    getProfileSetting("library.scan_on_start")
      .then((v) => {
        if (cancelled) return;
        if (v != null) setScanOnStart(v === "true" || v === "1");
      })
      .catch(() => {});
    getProfileSetting("ui.single_click_play")
      .then((v) => {
        if (cancelled) return;
        if (v === "true" || v === "1") setSingleClickPlay(true);
      })
      .catch(() => {});
    // Sleep timer / A-B loop / audio-quality-footer visibility now
    // live inside `PlayerBarLayoutCard` and are read through the
    // shared `usePlayerBarLayout` hook — no need to hydrate them
    // here anymore.
    getProfileSetting("ui.show_spotify")
      .then((v) => {
        if (cancelled) return;
        // Missing key → ON (matches Sidebar default).
        if (v != null) setShowSpotify(v === "true" || v === "1");
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Keep the slider in sync when the user nudges zoom from the
  // keyboard shortcuts (`Ctrl+=` / `Ctrl+-` / `Ctrl+0`) — those land
  // in `useUiZoom` which broadcasts the new level via the window
  // event. Same defensive bounds check as the hook: the event is
  // public on `window` so we don't trust arbitrary numbers.
  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<number>).detail;
      if (
        typeof detail === "number" &&
        Number.isFinite(detail) &&
        detail >= UI_ZOOM_MIN &&
        detail <= UI_ZOOM_MAX
      ) {
        setUiZoom(detail);
      }
    };
    window.addEventListener(UI_ZOOM_CHANGED_EVENT, handler);
    return () => window.removeEventListener(UI_ZOOM_CHANGED_EVENT, handler);
  }, []);

  // Functional setter so rapid clicks accumulate from the latest
  // committed value (and not from the value React captured at the
  // click that triggered the handler). The fire-and-forget
  // `applyUiZoom` runs on the side; its broadcast event reconciles
  // the local state once it lands, so any clamping the backend does
  // is reflected here without a second `setUiZoom` call racing the
  // optimistic one we return.
  const handleZoomDelta = useCallback((delta: number) => {
    setUiZoom((prev) => {
      const next = Math.min(
        UI_ZOOM_MAX,
        Math.max(UI_ZOOM_MIN, Math.round((prev + delta) * 10) / 10),
      );
      if (next === prev) return prev;
      applyUiZoom(next).catch((err) =>
        console.error("[Settings] applyUiZoom failed", err),
      );
      return next;
    });
  }, []);
  const handleZoomReset = useCallback(() => {
    setUiZoom((prev) => {
      if (prev === 1) return prev;
      applyUiZoom(1).catch((err) =>
        console.error("[Settings] applyUiZoom failed", err),
      );
      return 1;
    });
  }, []);

  const handleToggleSingleClickPlay = useCallback(() => {
    const next = !singleClickPlay;
    setSingleClickPlay(next);
    setProfileSetting(
      "ui.single_click_play",
      next ? "true" : "false",
      "bool",
    ).catch((err) => {
      console.error("[Settings] set single_click_play failed", err);
      setSingleClickPlay(!next);
    });
  }, [singleClickPlay]);

  const handleToggleShowSpotify = useCallback(() => {
    const next = !showSpotify;
    setShowSpotify(next);
    setProfileSetting("ui.show_spotify", next ? "true" : "false", "bool")
      .then(() => {
        window.dispatchEvent(
          new CustomEvent("waveflow:show-spotify-visibility"),
        );
      })
      .catch((err) => {
        console.error("[Settings] set show_spotify failed", err);
        setShowSpotify(!next);
      });
  }, [showSpotify]);

  const handleToggleAutoStart = useCallback(() => {
    const next = !autoStart;
    setAutoStart(next);
    persistAutoStart(next).catch((err) => {
      console.error("[Settings] set auto_start failed", err);
      setAutoStart(!next);
    });
  }, [autoStart]);

  const handleToggleMinimizeToTray = useCallback(() => {
    const next = !minimizeToTray;
    setMinimizeToTray(next);
    persistMinimizeToTray(next).catch((err) => {
      console.error("[Settings] set minimize_to_tray failed", err);
      setMinimizeToTray(!next);
    });
  }, [minimizeToTray]);

  const handleToggleScanOnStart = useCallback(() => {
    const next = !scanOnStart;
    setScanOnStart(next);
    setProfileSetting(
      "library.scan_on_start",
      next ? "true" : "false",
      "bool",
    ).catch((err) => {
      console.error("[Settings] set scan_on_start failed", err);
      setScanOnStart(!next);
    });
  }, [scanOnStart]);

  const handleCopyLogs = useCallback(async () => {
    try {
      const text = await readRecentLogs();
      await navigator.clipboard.writeText(text);
      setCopyLogsStatus("ok");
    } catch (err) {
      console.error("[Settings] copy logs failed", err);
      setCopyLogsStatus("fail");
    }
    // Clear the toast after a short window so the next click reads
    // as a fresh action rather than a still-stale confirmation.
    window.setTimeout(() => setCopyLogsStatus(null), 2000);
  }, []);

  const handleOpenLogFolder = useCallback(() => {
    openLogFolder().catch((err) =>
      console.error("[Settings] open log folder failed", err),
    );
  }, []);

  const handleRescan = async () => {
    if (isRescanning) return;
    setIsRescanning(true);
    try {
      for (const lib of libraries) {
        await rescanLibrary(lib.id);
      }
    } catch (err) {
      console.error("[SettingsView] rescan failed", err);
    } finally {
      setIsRescanning(false);
    }
  };

  // Subscribe once to `analysis:progress` so the bar updates while
  // the worker grinds through the library. The backend always emits
  // a final event with `current_track_id = null` after the run, so
  // we don't need a manual reset on success.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{
      processed: number;
      total: number;
      current_track_id: number | null;
      failed: number;
    }>("analysis:progress", (event) => {
      setAnalyzeProgress({
        processed: event.payload.processed,
        total: event.payload.total,
        failed: event.payload.failed,
      });
    })
      .then((un) => {
        unlisten = un;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const handleAnalyzeLibrary = async () => {
    if (isAnalyzingLib) return;
    setIsAnalyzingLib(true);
    setAnalyzeProgress({ processed: 0, total: 0, failed: 0 });
    try {
      const { analyzeLibrary } = await import("../../lib/tauri/analysis");
      await analyzeLibrary();
    } catch (err) {
      console.error("[SettingsView] analyze library failed", err);
    } finally {
      setIsAnalyzingLib(false);
      // Clear the progress card after a short delay so the user
      // sees the final 100% state before it disappears.
      window.setTimeout(() => setAnalyzeProgress(null), 1500);
    }
  };

  // Cover batch fetch
  const [isFetchingCovers, setIsFetchingCovers] = useState(false);
  const [coverProgress, setCoverProgress] = useState<{
    current: number;
    total: number;
    albumTitle: string;
  } | null>(null);
  const [coverResultMsg, setCoverResultMsg] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ current: number; total: number; album_title: string }>(
      "cover-fetch-progress",
      (event) => {
        setCoverProgress({
          current: event.payload.current,
          total: event.payload.total,
          albumTitle: event.payload.album_title,
        });
      },
    )
      .then((un) => {
        unlisten = un;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const handleFetchMissingCovers = async () => {
    if (isFetchingCovers) return;
    setIsFetchingCovers(true);
    setCoverProgress(null);
    setCoverResultMsg(null);
    try {
      const fetched = await batchFetchMissingAlbumCovers();
      setCoverResultMsg(t("library.fetchCoversResult", { count: fetched }));
    } catch (err) {
      console.error("[SettingsView] fetch missing covers failed", err);
      setCoverResultMsg(t("library.fetchCoversFailed"));
    } finally {
      setIsFetchingCovers(false);
      window.setTimeout(() => {
        setCoverProgress(null);
        setCoverResultMsg(null);
      }, 4000);
    }
  };

  // Artist picture batch fetch
  const [isFetchingArtists, setIsFetchingArtists] = useState(false);
  const [artistFetchProgress, setArtistFetchProgress] = useState<{
    current: number;
    total: number;
    artistName: string;
  } | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ current: number; total: number; artist_name: string }>(
      "artist-fetch-progress",
      (event) => {
        setArtistFetchProgress({
          current: event.payload.current,
          total: event.payload.total,
          artistName: event.payload.artist_name,
        });
      },
    )
      .then((un) => {
        unlisten = un;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Rescan local sidecar `artist.jpg` files (see scan.rs::extract_artist_image).
  const [isRescanningLocalArtists, setIsRescanningLocalArtists] =
    useState(false);
  const [localArtistRescanStatus, setLocalArtistRescanStatus] = useState<
    string | null
  >(null);

  const handleRescanLocalArtistImages = useCallback(async () => {
    if (isRescanningLocalArtists) return;
    setIsRescanningLocalArtists(true);
    setLocalArtistRescanStatus(null);
    try {
      const summary = await rescanLocalArtistImages();
      setLocalArtistRescanStatus(
        t("settings.localArtistImages.done", {
          linked: summary.linked,
          considered: summary.considered,
        }),
      );
    } catch (err) {
      console.error("[SettingsView] rescan local artist images failed", err);
    } finally {
      setIsRescanningLocalArtists(false);
      window.setTimeout(() => setLocalArtistRescanStatus(null), 5000);
    }
  }, [isRescanningLocalArtists, t]);

  const handleFetchMissingArtistPictures = async () => {
    if (isFetchingArtists) return;
    setIsFetchingArtists(true);
    setArtistFetchProgress(null);
    try {
      await batchFetchMissingArtistPictures();
    } catch (err) {
      console.error("[SettingsView] fetch missing artist pictures failed", err);
    } finally {
      setIsFetchingArtists(false);
      window.setTimeout(() => setArtistFetchProgress(null), 3000);
    }
  };

  // Lyrics library prefetch
  const [isPrefetchingLyrics, setIsPrefetchingLyrics] = useState(false);
  const [lyricsPrefetchProgress, setLyricsPrefetchProgress] = useState<{
    processed: number;
    total: number;
    hits: number;
    misses: number;
    failed: number;
    currentTitle: string | null;
  } | null>(null);
  const [lyricsResultMsg, setLyricsResultMsg] = useState<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{
      processed: number;
      total: number;
      hits: number;
      misses: number;
      failed: number;
      current_title: string | null;
    }>("lyrics:prefetch-progress", (event) => {
      setLyricsPrefetchProgress({
        processed: event.payload.processed,
        total: event.payload.total,
        hits: event.payload.hits,
        misses: event.payload.misses,
        failed: event.payload.failed,
        currentTitle: event.payload.current_title,
      });
    })
      .then((un) => {
        unlisten = un;
      })
      .catch(() => {});
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const handlePrefetchLyrics = async () => {
    if (isPrefetchingLyrics) return;
    setIsPrefetchingLyrics(true);
    setLyricsPrefetchProgress(null);
    setLyricsResultMsg(null);
    try {
      const { prefetchLibraryLyrics } = await import("../../lib/tauri/lyrics");
      const summary = await prefetchLibraryLyrics();
      setLyricsResultMsg(
        t("settings.lyricsPrefetch.result", {
          hits: summary.hits,
          misses: summary.misses,
          failed: summary.failed,
        }),
      );
    } catch (err) {
      console.error("[SettingsView] prefetch lyrics failed", err);
      // Distinguish offline mode (the user can act on it) from a real
      // failure so the message is not just a generic "something broke".
      const msg = String(err);
      setLyricsResultMsg(
        msg.includes("offline")
          ? t("settings.lyricsPrefetch.offline")
          : t("settings.lyricsPrefetch.failed"),
      );
    } finally {
      setIsPrefetchingLyrics(false);
      window.setTimeout(() => {
        setLyricsPrefetchProgress(null);
        setLyricsResultMsg(null);
      }, 5000);
    }
  };

  const handleCancelPrefetchLyrics = async () => {
    try {
      const { cancelLyricsPrefetch } = await import("../../lib/tauri/lyrics");
      await cancelLyricsPrefetch();
    } catch (err) {
      console.error("[SettingsView] cancel prefetch lyrics failed", err);
    }
  };

  // Thumbnail regeneration
  const [isRegeneratingThumbs, setIsRegeneratingThumbs] = useState(false);
  const [thumbsStatus, setThumbsStatus] = useState<string | null>(null);

  const handleRegenerateThumbnails = useCallback(async () => {
    if (isRegeneratingThumbs) return;
    setIsRegeneratingThumbs(true);
    setThumbsStatus(null);
    try {
      const count = await regenerateThumbnails();
      setThumbsStatus(t("settings.regenerateThumbnailsDone", { count }));
    } catch (err) {
      console.error("[SettingsView] regenerate thumbnails failed", err);
    } finally {
      setIsRegeneratingThumbs(false);
      window.setTimeout(() => setThumbsStatus(null), 4000);
    }
  }, [isRegeneratingThumbs, t]);

  // Audio settings — hydrated from backend at mount.
  const [normalize, setNormalize] = useState(false);
  const [mono, setMono] = useState(false);
  const [crossfadeSec, setCrossfadeSec] = useState(0);
  const [replayGain, setReplayGain] = useState(false);
  const [gapless, setGapless] = useState(true);

  // Integrations
  const [lastfmKey, setLastfmKey] = useState("");
  const [lastfmKeyVisible, setLastfmKeyVisible] = useState(false);
  const [lastfmSecret, setLastfmSecret] = useState("");
  const [lastfmSecretVisible, setLastfmSecretVisible] = useState(false);
  const [lastfmSaving, setLastfmSaving] = useState(false);
  const [lastfmSaved, setLastfmSaved] = useState(false);
  // Login form state — only shown when keys are set + user not yet
  // connected. Cleared aggressively so the password never lingers.
  const [lastfmStatus, setLastfmStatus] = useState<LastfmStatus | null>(null);
  const [lastfmUsername, setLastfmUsername] = useState("");
  const [lastfmPassword, setLastfmPassword] = useState("");
  const [lastfmLoggingIn, setLastfmLoggingIn] = useState(false);
  const [lastfmLoginError, setLastfmLoginError] = useState<string | null>(null);
  const [spotifyClientId, setSpotifyClientIdState] = useState("");
  const [spotifyClientIdVisible, setSpotifyClientIdVisible] = useState(false);
  const [spotifyStatus, setSpotifyStatus] = useState<SpotifyStatus | null>(
    null,
  );
  const [spotifySaving, setSpotifySaving] = useState(false);
  const [spotifySaved, setSpotifySaved] = useState(false);
  const [spotifyLoggingIn, setSpotifyLoggingIn] = useState(false);
  const [spotifyError, setSpotifyError] = useState<string | null>(null);

  // Discord Rich Presence opt-in. Hydrated once at mount, flipped
  // optimistically with rollback on failure.
  const [discordRpc, setDiscordRpc] = useState(false);

  // Global offline-mode flag. Persisted in app_setting (process-wide,
  // not per-profile), hydrated at mount.
  const [offlineMode, setOfflineModeState] = useState(false);

  useEffect(() => {
    getOfflineMode()
      .then(setOfflineModeState)
      .catch((err) => console.error("[SettingsView] get offline mode", err));
  }, []);

  const handleToggleOfflineMode = useCallback(() => {
    const next = !offlineMode;
    setOfflineModeState(next);
    setOfflineMode(next).catch((err) => {
      console.error("[SettingsView] set offline mode failed", err);
      setOfflineModeState(!next);
    });
  }, [offlineMode]);

  // Profile export / import — both are async operations that show a
  // transient status string under the row so the user gets feedback
  // without a modal.
  const [profileIoBusy, setProfileIoBusy] = useState<
    "export" | "import" | null
  >(null);
  const [profileIoStatus, setProfileIoStatus] = useState<{
    kind: "ok" | "fail";
    message: string;
  } | null>(null);

  const flashStatus = useCallback((kind: "ok" | "fail", message: string) => {
    setProfileIoStatus({ kind, message });
    window.setTimeout(() => setProfileIoStatus(null), 4000);
  }, []);

  const handleExportProfile = useCallback(async () => {
    if (profileIoBusy) return;
    if (!activeProfile) return;
    const safeName = activeProfile.name.replace(/[^\w.-]+/g, "_");
    const target = await pickSaveFile(
      `${safeName || "profile"}.waveflow`,
      ["waveflow"],
      t("settings.profileIo.export.dialogTitle") ?? undefined,
    );
    if (!target) return;
    setProfileIoBusy("export");
    try {
      await exportProfile(target, activeProfile.id);
      flashStatus("ok", t("settings.profileIo.export.done"));
    } catch (err) {
      console.error("[SettingsView] export profile failed", err);
      flashStatus("fail", t("settings.profileIo.export.failed"));
    } finally {
      setProfileIoBusy(null);
    }
  }, [activeProfile, flashStatus, profileIoBusy, t]);

  const handleImportProfile = useCallback(async () => {
    if (profileIoBusy) return;
    const source = await pickFile(
      ["waveflow"],
      t("settings.profileIo.import.dialogTitle") ?? undefined,
    );
    if (!source) return;
    setProfileIoBusy("import");
    try {
      const newId = await importProfile(source, null);
      flashStatus("ok", t("settings.profileIo.import.done", { id: newId }));
    } catch (err) {
      console.error("[SettingsView] import profile failed", err);
      flashStatus("fail", t("settings.profileIo.import.failed"));
    } finally {
      setProfileIoBusy(null);
    }
  }, [flashStatus, profileIoBusy, t]);

  // DLNA / UPnP MediaServer. `dlnaConfig` carries the persisted
  // settings (name, port, enabled flag); `dlnaStatus` is the live
  // worker-thread snapshot (running, bound URL, last error).
  const [dlnaConfig, setDlnaConfig] = useState<DlnaConfig>({
    enabled: false,
    server_name: "WaveFlow",
    port: 0,
  });
  const [dlnaStatus, setDlnaStatus] = useState<DlnaStatus | null>(null);
  const [dlnaUrlCopied, setDlnaUrlCopied] = useState(false);

  useEffect(() => {
    getDiscordRpcEnabled()
      .then(setDiscordRpc)
      .catch((err) =>
        console.error("[SettingsView] get discord rpc failed", err),
      );
  }, []);

  const handleToggleDiscordRpc = useCallback(() => {
    const next = !discordRpc;
    setDiscordRpc(next);
    setDiscordRpcEnabled(next).catch((err) => {
      console.error("[SettingsView] set discord rpc failed", err);
      setDiscordRpc(!next);
    });
  }, [discordRpc]);

  // DLNA — load persisted config + live status at mount.
  useEffect(() => {
    dlnaGetConfig()
      .then(setDlnaConfig)
      .catch((err) => console.error("[SettingsView] dlna config", err));
    dlnaGetStatus()
      .then(setDlnaStatus)
      .catch((err) => console.error("[SettingsView] dlna status", err));
  }, []);

  // Push a config change to the backend and immediately refresh the
  // status so the UI reflects whether the bind succeeded. Optimistic
  // — the local state already shows the requested value; the status
  // refresh just confirms.
  const persistDlna = useCallback(async (next: DlnaConfig) => {
    setDlnaConfig(next);
    try {
      const status = await dlnaSetConfig(next);
      setDlnaStatus(status);
    } catch (err) {
      console.error("[SettingsView] dlna set", err);
    }
  }, []);

  const handleToggleDlna = useCallback(() => {
    persistDlna({ ...dlnaConfig, enabled: !dlnaConfig.enabled });
  }, [dlnaConfig, persistDlna]);

  const handleDlnaCopyUrl = useCallback(() => {
    if (!dlnaStatus?.bound_url) return;
    navigator.clipboard
      .writeText(dlnaStatus.bound_url)
      .then(() => {
        setDlnaUrlCopied(true);
        setTimeout(() => setDlnaUrlCopied(false), 1500);
      })
      .catch(() => {});
  }, [dlnaStatus]);

  useEffect(() => {
    getLastfmApiKey()
      .then((v) => {
        if (v) setLastfmKey(v);
      })
      .catch(() => {});
    getLastfmApiSecret()
      .then((v) => {
        if (v) setLastfmSecret(v);
      })
      .catch(() => {});
    lastfmGetStatus()
      .then(setLastfmStatus)
      .catch((err) =>
        console.error("[SettingsView] Last.fm status failed", err),
      );
  }, []);

  const refreshLastfmStatus = useCallback(() => {
    lastfmGetStatus()
      .then(setLastfmStatus)
      .catch((err) =>
        console.error("[SettingsView] Last.fm status failed", err),
      );
  }, []);

  const handleSaveLastfmKey = async () => {
    if (lastfmSaving) return;
    setLastfmSaving(true);
    setLastfmSaved(false);
    try {
      await setLastfmApiKey(lastfmKey);
      await setLastfmApiSecret(lastfmSecret);
      setLastfmSaved(true);
      window.setTimeout(() => setLastfmSaved(false), 2000);
      refreshLastfmStatus();
    } catch (err) {
      console.error("[SettingsView] save Last.fm credentials failed", err);
    } finally {
      setLastfmSaving(false);
    }
  };

  const handleLastfmLogin = async () => {
    if (lastfmLoggingIn) return;
    if (!lastfmUsername.trim() || !lastfmPassword) return;
    setLastfmLoggingIn(true);
    setLastfmLoginError(null);
    try {
      const status = await lastfmLogin(lastfmUsername.trim(), lastfmPassword);
      setLastfmStatus(status);
      setLastfmUsername("");
      setLastfmPassword("");
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setLastfmLoginError(message);
      console.error("[SettingsView] Last.fm login failed", err);
    } finally {
      setLastfmLoggingIn(false);
    }
  };

  const handleLastfmLogout = async () => {
    try {
      await lastfmLogout();
      refreshLastfmStatus();
    } catch (err) {
      console.error("[SettingsView] Last.fm logout failed", err);
    }
  };

  useEffect(() => {
    getSpotifyClientId()
      .then((v) => {
        if (v) setSpotifyClientIdState(v);
      })
      .catch(() => {});
    spotifyGetStatus()
      .then(setSpotifyStatus)
      .catch((err) =>
        console.error("[SettingsView] Spotify status failed", err),
      );
  }, []);

  const refreshSpotifyStatus = useCallback(() => {
    spotifyGetStatus()
      .then(setSpotifyStatus)
      .catch((err) =>
        console.error("[SettingsView] Spotify status failed", err),
      );
  }, []);

  const handleSaveSpotifyClientId = async () => {
    if (spotifySaving) return;
    setSpotifySaving(true);
    setSpotifySaved(false);
    setSpotifyError(null);
    try {
      await setSpotifyClientId(spotifyClientId);
      setSpotifySaved(true);
      window.setTimeout(() => setSpotifySaved(false), 2000);
      refreshSpotifyStatus();
    } catch (err) {
      setSpotifyError(err instanceof Error ? err.message : String(err));
      console.error("[SettingsView] save Spotify Client ID failed", err);
    } finally {
      setSpotifySaving(false);
    }
  };

  const handleSpotifyLogin = async () => {
    if (spotifyLoggingIn || !spotifyClientId.trim()) return;
    setSpotifyLoggingIn(true);
    setSpotifyError(null);
    try {
      const status = await spotifyLogin();
      setSpotifyStatus(status);
    } catch (err) {
      setSpotifyError(err instanceof Error ? err.message : String(err));
      console.error("[SettingsView] Spotify login failed", err);
    } finally {
      setSpotifyLoggingIn(false);
    }
  };

  const handleSpotifyLogout = async () => {
    setSpotifyError(null);
    try {
      await spotifyLogout();
      refreshSpotifyStatus();
    } catch (err) {
      setSpotifyError(err instanceof Error ? err.message : String(err));
      console.error("[SettingsView] Spotify logout failed", err);
    }
  };

  useEffect(() => {
    playerGetAudioSettings()
      .then((s) => {
        setNormalize(s.normalize);
        setMono(s.mono);
        setCrossfadeSec(Math.round(s.crossfade_ms / 1000));
        setReplayGain(s.replaygain);
        setGapless(s.gapless);
      })
      .catch((err) =>
        console.error("[Settings] audio settings load failed", err),
      );
  }, []);

  const handleToggleNormalize = useCallback(() => {
    const next = !normalize;
    setNormalize(next);
    playerSetNormalize(next).catch((err) => {
      console.error("[Settings] set normalize failed", err);
      setNormalize(!next); // rollback
    });
  }, [normalize]);

  const handleToggleReplayGain = useCallback(() => {
    const next = !replayGain;
    setReplayGain(next);
    playerSetReplayGain(next).catch((err) => {
      console.error("[Settings] set replaygain failed", err);
      setReplayGain(!next); // rollback
    });
  }, [replayGain]);

  // Smart crossfade — skip the fade between two tracks of the same
  // album so concept records / live sets hand off naturally. Persisted
  // backend-side; default OFF (opinionated behaviour, opt-in).
  const [smartCrossfade, setSmartCrossfadeState] = useState(false);

  useEffect(() => {
    getSmartCrossfade()
      .then(setSmartCrossfadeState)
      .catch((err) => console.error("[SettingsView] get smart crossfade", err));
  }, []);

  const handleToggleSmartCrossfade = useCallback(() => {
    const next = !smartCrossfade;
    setSmartCrossfadeState(next);
    setSmartCrossfade(next).catch((err) => {
      console.error("[SettingsView] set smart crossfade failed", err);
      setSmartCrossfadeState(!next);
    });
  }, [smartCrossfade]);

  // Dynamic (tempo-aware) crossfade — scales the upcoming fade by
  // the BPM gap. Same opt-in pattern; falls back silently to the
  // static crossfade when either track has no stored BPM.
  const [dynamicCrossfade, setDynamicCrossfadeState] = useState(false);

  useEffect(() => {
    getDynamicCrossfade()
      .then(setDynamicCrossfadeState)
      .catch((err) =>
        console.error("[SettingsView] get dynamic crossfade", err),
      );
  }, []);

  const handleToggleDynamicCrossfade = useCallback(() => {
    const next = !dynamicCrossfade;
    setDynamicCrossfadeState(next);
    setDynamicCrossfade(next).catch((err) => {
      console.error("[SettingsView] set dynamic crossfade failed", err);
      setDynamicCrossfadeState(!next);
    });
  }, [dynamicCrossfade]);

  // Spectrum visualizer toggle. Persisted backend-side (per-profile)
  // and pushed live to the decoder thread, so flipping it shows /
  // hides the bars on the next emitted frame.
  const [visualizer, setVisualizer] = useState(false);

  useEffect(() => {
    getVisualizerEnabled()
      .then(setVisualizer)
      .catch((err) => console.error("[SettingsView] get visualizer", err));
  }, []);

  const handleToggleVisualizer = useCallback(() => {
    const next = !visualizer;
    setVisualizer(next);
    setVisualizerEnabled(next).catch((err) => {
      console.error("[SettingsView] set visualizer failed", err);
      setVisualizer(!next);
    });
  }, [visualizer]);

  const handleToggleGapless = useCallback(() => {
    const next = !gapless;
    setGapless(next);
    playerSetGapless(next).catch((err) => {
      console.error("[Settings] set gapless failed", err);
      setGapless(!next); // rollback
    });
  }, [gapless]);

  const handleToggleMono = useCallback(() => {
    const next = !mono;
    setMono(next);
    playerSetMono(next).catch((err) => {
      console.error("[Settings] set mono failed", err);
      setMono(!next);
    });
  }, [mono]);

  // Debounce crossfade slider changes to avoid spamming the backend.
  const crossfadeTimerRef = useRef<number | null>(null);
  const handleCrossfadeChange = useCallback((sec: number) => {
    setCrossfadeSec(sec);
    if (crossfadeTimerRef.current != null) {
      window.clearTimeout(crossfadeTimerRef.current);
    }
    crossfadeTimerRef.current = window.setTimeout(() => {
      playerSetCrossfade(sec).catch((err) =>
        console.error("[Settings] set crossfade failed", err),
      );
    }, 300);
  }, []);

  const handleLanguageChange = (code: string) => {
    i18n.changeLanguage(code).catch((err) => {
      console.error("[i18n] changeLanguage failed", err);
    });
  };

  const handleOpenDataFolder = useCallback(async () => {
    try {
      await invoke("open_data_folder", {
        profileId: activeProfile?.id ?? null,
      });
    } catch (err) {
      console.error("[SettingsView] open data folder failed", err);
    }
  }, [activeProfile]);

  return (
    <div className="max-w-4xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-4">
        <button
          type="button"
          onClick={() => onNavigate("home")}
          aria-label={t("common.back")}
          className="p-1 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
        >
          <ArrowLeft size={20} />
        </button>
        <div>
          <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
            {t("settings.title")}
          </h1>
          <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
        </div>
      </div>

      {/* Category tabs (Lokal-style). Horizontal pill bar that
          swaps which section is visible. The mounted-on-demand model
          keeps heavy effects (EQ spectrum, backup card, shortcuts
          editor) idle until the user actually opens their tab. */}
      <div
        role="tablist"
        aria-label={t("settings.categoryNavLabel")}
        aria-orientation="horizontal"
        className="-mx-2 flex flex-wrap gap-2"
      >
        {SETTINGS_CATEGORIES.map(({ id, labelKey, Icon }) => {
          const isActive = id === activeCategory;
          return (
            <button
              key={id}
              type="button"
              role="tab"
              id={`settings-tab-${id}`}
              aria-selected={isActive}
              aria-controls={`settings-panel-${id}`}
              // Roving tabindex: only the active tab is in the
              // sequential tab order; arrow-key navigation between
              // tabs is the standard WAI-ARIA tab pattern (not wired
              // here yet — clicking a tab still works for keyboard
              // users via Tab + Enter/Space).
              tabIndex={isActive ? 0 : -1}
              onClick={() => handleCategoryChange(id)}
              className={`group inline-flex items-center gap-2 px-4 py-2 rounded-full border text-sm font-medium transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                isActive
                  ? "border-emerald-500 bg-emerald-50 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300 shadow-sm"
                  : "border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900/50 text-zinc-600 dark:text-zinc-300 hover:border-zinc-300 dark:hover:border-zinc-600 hover:text-zinc-900 dark:hover:text-white"
              }`}
            >
              <Icon
                size={16}
                className={isActive ? "" : "text-zinc-400"}
                aria-hidden="true"
              />
              {t(labelKey)}
            </button>
          );
        })}
      </div>

      {/* Library category — app-level + library-management settings. */}
      {activeCategory === "library" && (
        <section
          role="tabpanel"
          id="settings-panel-library"
          aria-labelledby="settings-tab-library"
          tabIndex={0}
        >
          <h2
            id="settings-general-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.library")}
          </h2>
          <div className="space-y-1">
            {/* Langue */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Globe size={20} className="text-zinc-400" aria-hidden="true" />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.language.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.language.subtitle")}
                  </div>
                </div>
              </div>
              <LanguageDropdown
                currentCode={normalizeSupportedLanguageCode(
                  i18n.resolvedLanguage ?? i18n.language,
                )}
                onSelect={handleLanguageChange}
              />
            </div>

            {/* UI zoom — same shape as VS Code / browser zoom. The
              -/+/reset cluster on the right is a thin control band so
              users with cramped 1080p screens can shrink everything
              while 4K users can bump it up. Hooked to the same
              keyboard shortcuts (Ctrl+=, Ctrl+-, Ctrl+0) via the
              `useUiZoom` hook mounted on AppLayout. */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <ZoomIn
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.uiZoom.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.uiZoom.subtitle")}
                  </div>
                </div>
              </div>
              <div className="flex items-center space-x-2">
                <button
                  type="button"
                  onClick={() => handleZoomDelta(-UI_ZOOM_STEP)}
                  disabled={uiZoom <= UI_ZOOM_MIN + 1e-3}
                  aria-label={t("settings.uiZoom.decreaseAria")}
                  className="w-8 h-8 flex items-center justify-center rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed text-lg leading-none"
                >
                  −
                </button>
                <button
                  type="button"
                  onClick={handleZoomReset}
                  aria-label={t("settings.uiZoom.resetAria")}
                  title={t("settings.uiZoom.resetAria")}
                  className="min-w-14 px-2 h-8 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-sm font-mono tabular-nums"
                >
                  {Math.round(uiZoom * 100)} %
                </button>
                <button
                  type="button"
                  onClick={() => handleZoomDelta(UI_ZOOM_STEP)}
                  disabled={uiZoom >= UI_ZOOM_MAX - 1e-3}
                  aria-label={t("settings.uiZoom.increaseAria")}
                  className="w-8 h-8 flex items-center justify-center rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed text-lg leading-none"
                >
                  +
                </button>
              </div>
            </div>

            {/* Lancement au démarrage */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Power size={20} className="text-zinc-400" aria-hidden="true" />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.autoStart.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.autoStart.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={autoStart}
                onToggle={handleToggleAutoStart}
                label={t("settings.autoStart.title")}
              />
            </div>

            {/* Minimiser dans la barre système */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Minimize2
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.minimizeToTray.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.minimizeToTray.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={minimizeToTray}
                onToggle={handleToggleMinimizeToTray}
                label={t("settings.minimizeToTray.title")}
              />
            </div>

            {/* Scanner au démarrage */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <ScanLine
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.scanOnStart.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.scanOnStart.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={scanOnStart}
                onToggle={handleToggleScanOnStart}
                label={t("settings.scanOnStart.title")}
              />
            </div>

            {/* Lecture au clic simple */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <MousePointerClick
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.singleClickPlay.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.singleClickPlay.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={singleClickPlay}
                onToggle={handleToggleSingleClickPlay}
                label={t("settings.singleClickPlay.title")}
              />
            </div>

            {/* Visibilité de l'entrée Spotify dans la sidebar */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Headphones
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.showSpotify.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.showSpotify.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={showSpotify}
                onToggle={handleToggleShowSpotify}
                label={t("settings.showSpotify.title")}
              />
            </div>
          </div>
        </section>
      )}

      {/* Playback category — audio engine, lyrics, EQ. */}
      {activeCategory === "playback" && (
        <section
          role="tabpanel"
          id="settings-panel-playback"
          aria-labelledby="settings-tab-playback"
          tabIndex={0}
        >
          <h2
            id="settings-playback-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.playback")}
          </h2>
          <div className="space-y-1">
            {/* Crossfade */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Shuffle
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.crossfade.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.crossfade.subtitle")}
                  </div>
                </div>
              </div>
              <div className="flex items-center space-x-3">
                <input
                  type="range"
                  min={0}
                  max={12}
                  step={1}
                  value={crossfadeSec}
                  onChange={(e) =>
                    handleCrossfadeChange(Number(e.target.value))
                  }
                  className="w-32 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
                  aria-label={t("settings.crossfade.title")}
                />
                <span className="text-sm font-medium text-zinc-500 w-10 text-right tabular-nums">
                  {crossfadeSec} s
                </span>
              </div>
            </div>

            {/* Smart crossfade — same-album skip */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Sparkles
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.smartCrossfade.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.smartCrossfade.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={smartCrossfade}
                onToggle={handleToggleSmartCrossfade}
                label={t("settings.smartCrossfade.title")}
              />
            </div>

            {/* Dynamic crossfade — tempo-aware fade scaling */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Gauge size={20} className="text-zinc-400" aria-hidden="true" />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.dynamicCrossfade.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.dynamicCrossfade.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={dynamicCrossfade}
                onToggle={handleToggleDynamicCrossfade}
                label={t("settings.dynamicCrossfade.title")}
              />
            </div>

            {/* Spectrum visualizer */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Activity
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.visualizer.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.visualizer.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={visualizer}
                onToggle={handleToggleVisualizer}
                label={t("settings.visualizer.title")}
              />
            </div>

            {/* Gapless playback */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <ChevronsRight
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.gapless.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.gapless.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={gapless}
                onToggle={handleToggleGapless}
                label={t("settings.gapless.title")}
              />
            </div>

            {/* Normaliser le volume */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Volume2
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.normalize.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.normalize.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={normalize}
                onToggle={handleToggleNormalize}
                label={t("settings.normalize.title")}
              />
            </div>

            {/* ReplayGain */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Volume2
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.replayGain.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.replayGain.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={replayGain}
                onToggle={handleToggleReplayGain}
                label={t("settings.replayGain.title")}
              />
            </div>

            {/* Equalizer */}
            <div className="px-4">
              <EqualizerCard />
            </div>

            {/* WASAPI Exclusive Mode — Windows-only, the card hides
              itself on other platforms via UA sniff. */}
            <ExclusiveModeCard />

            {/* Audio mono */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Headphones
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.mono.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.mono.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={mono}
                onToggle={handleToggleMono}
                label={t("settings.mono.title")}
              />
            </div>
          </div>
        </section>
      )}

      {/* Integrations category — Spotify, Last.fm, Discord, DLNA. */}
      {activeCategory === "integrations" && (
        <section
          role="tabpanel"
          id="settings-panel-integrations"
          aria-labelledby="settings-tab-integrations"
          tabIndex={0}
        >
          <h2
            id="settings-integrations-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.integrations")}
          </h2>
          <div className="space-y-1">
            {/* Mode hors-ligne — coupe Last.fm / Deezer / LRCLIB d'un
              seul coup. Affiché en tête car il conditionne l'effet
              de toutes les intégrations en dessous. */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <WifiOff
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.offlineMode.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.offlineMode.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={offlineMode}
                onToggle={handleToggleOfflineMode}
                label={t("settings.offlineMode.title")}
              />
            </div>

            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-start space-x-4">
                <Radio
                  size={20}
                  className="text-zinc-400 mt-0.5"
                  aria-hidden="true"
                />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.integrations.lastfm.title")}
                  </div>
                  <div className="text-xs text-zinc-400 mb-3">
                    {t("settings.integrations.lastfm.subtitle")}
                  </div>

                  {/* API key */}
                  <div className="flex items-center space-x-2 mb-2">
                    <div className="relative flex-1">
                      <input
                        type={lastfmKeyVisible ? "text" : "password"}
                        value={lastfmKey}
                        onChange={(e) => {
                          setLastfmKey(e.target.value);
                          setLastfmSaved(false);
                        }}
                        placeholder={t(
                          "settings.integrations.lastfm.placeholder",
                        )}
                        spellCheck={false}
                        autoComplete="off"
                        className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                      />
                      <button
                        type="button"
                        onClick={() => setLastfmKeyVisible((v) => !v)}
                        aria-label={
                          lastfmKeyVisible
                            ? t("settings.integrations.lastfm.hide")
                            : t("settings.integrations.lastfm.show")
                        }
                        className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                      >
                        {lastfmKeyVisible ? (
                          <EyeOff size={16} />
                        ) : (
                          <Eye size={16} />
                        )}
                      </button>
                    </div>
                  </div>

                  {/* API secret */}
                  <div className="flex items-center space-x-2 mb-2">
                    <div className="relative flex-1">
                      <input
                        type={lastfmSecretVisible ? "text" : "password"}
                        value={lastfmSecret}
                        onChange={(e) => {
                          setLastfmSecret(e.target.value);
                          setLastfmSaved(false);
                        }}
                        placeholder={t(
                          "settings.integrations.lastfm.secretPlaceholder",
                        )}
                        spellCheck={false}
                        autoComplete="off"
                        className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                      />
                      <button
                        type="button"
                        onClick={() => setLastfmSecretVisible((v) => !v)}
                        aria-label={
                          lastfmSecretVisible
                            ? t("settings.integrations.lastfm.hide")
                            : t("settings.integrations.lastfm.show")
                        }
                        className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                      >
                        {lastfmSecretVisible ? (
                          <EyeOff size={16} />
                        ) : (
                          <Eye size={16} />
                        )}
                      </button>
                    </div>
                    <button
                      type="button"
                      onClick={handleSaveLastfmKey}
                      disabled={lastfmSaving}
                      className={`px-4 py-2 rounded-xl text-sm font-medium transition-colors disabled:opacity-50 ${
                        lastfmSaved
                          ? "bg-emerald-500 text-white"
                          : "border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                      }`}
                    >
                      {lastfmSaved
                        ? t("settings.integrations.lastfm.saved")
                        : t("settings.integrations.lastfm.save")}
                    </button>
                  </div>

                  {/* Account login / status — only shown once API
                    credentials are present. The session stays per
                    profile so two profiles can scrobble to two
                    different Last.fm accounts. */}
                  {lastfmStatus?.configured && (
                    <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800">
                      {lastfmStatus.connected ? (
                        <div className="flex items-center justify-between">
                          <div className="text-xs">
                            <span className="text-zinc-500">
                              {t(
                                "settings.integrations.lastfm.connectedAs",
                              )}{" "}
                            </span>
                            <span className="font-medium text-emerald-500">
                              {lastfmStatus.username}
                            </span>
                          </div>
                          <button
                            type="button"
                            onClick={handleLastfmLogout}
                            className="px-3 py-1.5 rounded-lg text-xs font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
                          >
                            {t("settings.integrations.lastfm.disconnect")}
                          </button>
                        </div>
                      ) : (
                        <div className="space-y-2">
                          <div className="text-xs text-zinc-500">
                            {t("settings.integrations.lastfm.loginPrompt")}
                          </div>
                          <div className="flex items-center space-x-2">
                            <input
                              type="text"
                              value={lastfmUsername}
                              onChange={(e) => {
                                setLastfmUsername(e.target.value);
                                setLastfmLoginError(null);
                              }}
                              placeholder={t(
                                "settings.integrations.lastfm.usernamePlaceholder",
                              )}
                              autoComplete="username"
                              spellCheck={false}
                              className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                            />
                            <input
                              type="password"
                              value={lastfmPassword}
                              onChange={(e) => {
                                setLastfmPassword(e.target.value);
                                setLastfmLoginError(null);
                              }}
                              placeholder={t(
                                "settings.integrations.lastfm.passwordPlaceholder",
                              )}
                              autoComplete="current-password"
                              className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                            />
                            <button
                              type="button"
                              onClick={handleLastfmLogin}
                              disabled={
                                lastfmLoggingIn ||
                                !lastfmUsername.trim() ||
                                !lastfmPassword
                              }
                              className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                            >
                              {lastfmLoggingIn
                                ? t("settings.integrations.lastfm.connecting")
                                : t("settings.integrations.lastfm.connect")}
                            </button>
                          </div>
                          {lastfmLoginError && (
                            <div className="text-xs text-rose-500">
                              {lastfmLoginError}
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              </div>
            </div>

            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-start space-x-4">
                <Headphones
                  size={20}
                  className="text-zinc-400 mt-0.5"
                  aria-hidden="true"
                />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.integrations.spotify.title", "Spotify")}
                  </div>
                  <div className="text-xs text-zinc-400 mb-3">
                    {t(
                      "settings.integrations.spotify.subtitle",
                      "Connect Spotify Premium with your own Spotify Developer Client ID.",
                    )}
                  </div>

                  <div className="flex items-center space-x-2 mb-2">
                    <div className="relative flex-1">
                      <input
                        type={spotifyClientIdVisible ? "text" : "password"}
                        value={spotifyClientId}
                        onChange={(e) => {
                          setSpotifyClientIdState(e.target.value);
                          setSpotifySaved(false);
                          setSpotifyError(null);
                        }}
                        placeholder={t(
                          "settings.integrations.spotify.clientIdPlaceholder",
                          "Spotify Client ID",
                        )}
                        spellCheck={false}
                        autoComplete="off"
                        className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                      />
                      <button
                        type="button"
                        onClick={() => setSpotifyClientIdVisible((v) => !v)}
                        aria-label={
                          spotifyClientIdVisible
                            ? t("settings.integrations.lastfm.hide")
                            : t("settings.integrations.lastfm.show")
                        }
                        className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                      >
                        {spotifyClientIdVisible ? (
                          <EyeOff size={16} />
                        ) : (
                          <Eye size={16} />
                        )}
                      </button>
                    </div>
                    <button
                      type="button"
                      onClick={handleSaveSpotifyClientId}
                      disabled={spotifySaving}
                      className={`px-4 py-2 rounded-xl text-sm font-medium transition-colors disabled:opacity-50 ${
                        spotifySaved
                          ? "bg-emerald-500 text-white"
                          : "border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                      }`}
                    >
                      {spotifySaved
                        ? t("settings.integrations.lastfm.saved")
                        : t("settings.integrations.lastfm.save")}
                    </button>
                  </div>

                  <div className="text-[11px] text-zinc-400 mb-3">
                    {t(
                      "settings.integrations.spotify.redirectHint",
                      "Add this Redirect URI in Spotify Developer Dashboard: http://127.0.0.1:49387/spotify/callback",
                    )}
                  </div>

                  {spotifyStatus?.configured && (
                    <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800">
                      {spotifyStatus.connected ? (
                        <div className="flex items-center justify-between">
                          <div className="text-xs">
                            <span className="text-zinc-500">
                              {t(
                                "settings.integrations.spotify.connectedAs",
                                "Connected as",
                              )}{" "}
                            </span>
                            <span className="font-medium text-emerald-500">
                              {spotifyStatus.username}
                            </span>
                          </div>
                          <button
                            type="button"
                            onClick={handleSpotifyLogout}
                            className="px-3 py-1.5 rounded-lg text-xs font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
                          >
                            {t(
                              "settings.integrations.spotify.disconnect",
                              "Disconnect",
                            )}
                          </button>
                        </div>
                      ) : (
                        <button
                          type="button"
                          onClick={handleSpotifyLogin}
                          disabled={spotifyLoggingIn || !spotifyClientId.trim()}
                          className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          {spotifyLoggingIn
                            ? t(
                                "settings.integrations.spotify.connecting",
                                "Connecting...",
                              )
                            : t(
                                "settings.integrations.spotify.connect",
                                "Connect Spotify",
                              )}
                        </button>
                      )}
                    </div>
                  )}
                  {spotifyError && (
                    <div className="text-xs text-rose-500 mt-2">
                      {spotifyError}
                    </div>
                  )}
                </div>
              </div>
            </div>

            {/* Discord Rich Presence */}
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Gamepad2
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.integrations.discord.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.integrations.discord.subtitle")}
                  </div>
                </div>
              </div>
              <ToggleSwitch
                enabled={discordRpc}
                onToggle={handleToggleDiscordRpc}
                label={t("settings.integrations.discord.title")}
              />
            </div>

            {/* DLNA / UPnP MediaServer */}
            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-start space-x-4">
                <Server
                  size={20}
                  className="text-zinc-400 mt-0.5"
                  aria-hidden="true"
                />
                <div className="flex-1 min-w-0">
                  <div className="flex items-center justify-between mb-1">
                    <div>
                      <div className="text-sm font-medium text-zinc-900 dark:text-white">
                        {t("settings.integrations.dlna.title")}
                      </div>
                      <div className="text-xs text-zinc-400">
                        {t("settings.integrations.dlna.subtitle")}
                      </div>
                    </div>
                    <ToggleSwitch
                      enabled={dlnaConfig.enabled}
                      onToggle={handleToggleDlna}
                      label={t("settings.integrations.dlna.title")}
                    />
                  </div>

                  {dlnaConfig.enabled && (
                    <div className="mt-3 space-y-3">
                      {/* Server name */}
                      <div className="flex items-center space-x-2">
                        <label
                          htmlFor="dlna-name"
                          className="text-xs text-zinc-500 w-24 shrink-0"
                        >
                          {t("settings.integrations.dlna.serverName")}
                        </label>
                        <input
                          id="dlna-name"
                          type="text"
                          value={dlnaConfig.server_name}
                          onChange={(e) =>
                            setDlnaConfig((c) => ({
                              ...c,
                              server_name: e.target.value,
                            }))
                          }
                          onBlur={() => persistDlna(dlnaConfig)}
                          spellCheck={false}
                          className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                        />
                      </div>

                      {/* Port */}
                      <div className="flex items-center space-x-2">
                        <label
                          htmlFor="dlna-port"
                          className="text-xs text-zinc-500 w-24 shrink-0"
                        >
                          {t("settings.integrations.dlna.port")}
                        </label>
                        <input
                          id="dlna-port"
                          type="number"
                          min={0}
                          max={65535}
                          value={dlnaConfig.port}
                          onChange={(e) =>
                            setDlnaConfig((c) => ({
                              ...c,
                              port: Math.max(
                                0,
                                Math.min(65535, Number(e.target.value) || 0),
                              ),
                            }))
                          }
                          onBlur={() => persistDlna(dlnaConfig)}
                          className="w-32 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100"
                        />
                        <span className="text-xs text-zinc-400">
                          {t("settings.integrations.dlna.portHint")}
                        </span>
                      </div>

                      {/* Status */}
                      <div className="flex items-center space-x-2 pt-1">
                        <span
                          className={`inline-block w-2 h-2 rounded-full ${
                            dlnaStatus?.running
                              ? "bg-emerald-500"
                              : "bg-zinc-400"
                          }`}
                          aria-hidden="true"
                        />
                        <span className="text-xs text-zinc-500">
                          {dlnaStatus?.running
                            ? t("settings.integrations.dlna.statusRunning")
                            : t("settings.integrations.dlna.statusStopped")}
                        </span>
                        {dlnaStatus?.bound_url && (
                          <>
                            <span className="text-xs font-mono text-zinc-700 dark:text-zinc-300 truncate">
                              {dlnaStatus.bound_url}
                            </span>
                            <button
                              type="button"
                              onClick={handleDlnaCopyUrl}
                              aria-label={t(
                                "settings.integrations.dlna.copyUrl",
                              )}
                              className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                            >
                              {dlnaUrlCopied ? (
                                <CheckIcon size={14} />
                              ) : (
                                <Copy size={14} />
                              )}
                            </button>
                          </>
                        )}
                      </div>

                      {dlnaStatus?.last_error && (
                        <div className="text-xs text-rose-500 break-words">
                          {dlnaStatus.last_error}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              </div>
            </div>
          </div>
        </section>
      )}

      {/* Appearance category — theme picker + player-bar layout.
          Switching theme re-skins every `bg-emerald-*` / `text-emerald-*`
          utility through the CSS variable layer in app.css, so no
          component code changes. `PlayerBarLayoutCard` was previously
          parked under Playback while this tab was still a placeholder
          — moved here now that the picker fills the section. */}
      {activeCategory === "appearance" && (
        <section
          role="tabpanel"
          id="settings-panel-appearance"
          aria-labelledby="settings-tab-appearance"
          tabIndex={0}
          className="space-y-8"
        >
          <div>
            <h2
              id="settings-appearance-heading"
              className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
            >
              {t("settings.sections.appearance")}
            </h2>
            <div className="px-4 py-3">
              <div className="flex items-center space-x-4 mb-4">
                <Palette
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.appearance.theme.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.appearance.theme.subtitle")}
                  </div>
                </div>
              </div>
              <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-5 gap-3">
                {THEME_PRESETS.map((preset) => {
                  const isActive = preset.id === theme.id;
                  return (
                    <button
                      key={preset.id}
                      type="button"
                      onClick={(event) => setThemeId(preset.id, event)}
                      aria-pressed={isActive}
                      className={`group relative rounded-xl border overflow-hidden transition-all focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 ${
                        isActive
                          ? "border-emerald-500 ring-2 ring-emerald-500/30"
                          : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600"
                      }`}
                    >
                      <div
                        className="h-16 flex items-center justify-between px-3 relative"
                        style={{
                          backgroundColor:
                            preset.ambient ??
                            (preset.mode === "dark" ? "#121212" : "#ffffff"),
                        }}
                      >
                        <div className="flex space-x-1">
                          <div
                            className="w-3 h-3 rounded-full"
                            style={{ backgroundColor: preset.accent[400] }}
                          />
                          <div
                            className="w-3 h-3 rounded-full"
                            style={{ backgroundColor: preset.accent[500] }}
                          />
                          <div
                            className="w-3 h-3 rounded-full"
                            style={{ backgroundColor: preset.accent[600] }}
                          />
                        </div>
                        {isActive && (
                          <span
                            className="flex items-center justify-center w-5 h-5 rounded-full shadow-sm"
                            style={{
                              backgroundColor: preset.accent[500],
                              color: "#fff",
                            }}
                          >
                            <Check size={12} strokeWidth={3} />
                          </span>
                        )}
                      </div>
                      <div className="px-3 py-2 bg-white dark:bg-zinc-900 text-left">
                        <div className="text-xs font-semibold text-zinc-800 dark:text-zinc-100 truncate">
                          {t(preset.labelKey)}
                        </div>
                        <div className="text-[10px] text-zinc-400 capitalize">
                          {t(`settings.appearance.mode.${preset.mode}`)}
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          </div>

          <PlayerBarLayoutCard />

          <WrappedBannerCard />
        </section>
      )}

      {/* Data category — backup, offline mode, export/import. */}
      {activeCategory === "data" && (
        <section
          role="tabpanel"
          id="settings-panel-data"
          aria-labelledby="settings-tab-data"
          tabIndex={0}
        >
          <h2
            id="settings-storage-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.data")}
          </h2>
          <div className="space-y-1">
            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <RefreshCcw
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.rescan.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.rescan.subtitle")}
                  </div>
                </div>
              </div>
              <button
                type="button"
                onClick={handleRescan}
                disabled={isRescanning || libraries.length === 0}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCcw
                  size={14}
                  aria-hidden="true"
                  className={isRescanning ? "animate-spin" : ""}
                />
                <span>{t("settings.rescan.action")}</span>
              </button>
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Copy size={20} className="text-zinc-400" aria-hidden="true" />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.duplicates.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.duplicates.subtitle")}
                  </div>
                </div>
              </div>
              <button
                type="button"
                onClick={() => setIsDuplicatesOpen(true)}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                <Copy size={14} aria-hidden="true" />
                <span>{t("settings.duplicates.action")}</span>
              </button>
            </div>

            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center justify-between">
                <div className="flex items-center space-x-4">
                  <Sparkles
                    size={20}
                    className="text-zinc-400"
                    aria-hidden="true"
                  />
                  <div>
                    <div className="text-sm font-medium text-zinc-900 dark:text-white">
                      {t("settings.analyze.title")}
                    </div>
                    <div className="text-xs text-zinc-400">
                      {t("settings.analyze.subtitle")}
                    </div>
                  </div>
                </div>
                <button
                  type="button"
                  onClick={handleAnalyzeLibrary}
                  disabled={isAnalyzingLib || libraries.length === 0}
                  className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <Sparkles
                    size={14}
                    aria-hidden="true"
                    className={isAnalyzingLib ? "animate-pulse" : ""}
                  />
                  <span>{t("settings.analyze.action")}</span>
                </button>
              </div>
              {/* Auto-analyze toggle: when on, every scan that
                adds new tracks fires the analyzer in the
                background. Sits inside the same card so users
                see it as a related option rather than a
                disconnected setting. */}
              <div className="mt-3 ml-9 flex items-center justify-between">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.analyze.autoTitle")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.analyze.autoSubtitle")}
                  </div>
                </div>
                <ToggleSwitch
                  enabled={autoAnalyze}
                  onToggle={handleToggleAutoAnalyze}
                  label={t("settings.analyze.autoTitle")}
                />
              </div>
              {/* Progress strip — shown during a run + briefly after
                completion so the user sees the final tally. */}
              {analyzeProgress && (
                <div className="mt-3 ml-9">
                  <div className="flex justify-between text-[11px] text-zinc-500 mb-1">
                    <span>
                      {analyzeProgress.processed} / {analyzeProgress.total}
                    </span>
                    {analyzeProgress.failed > 0 && (
                      <span className="text-rose-500">
                        {t("settings.analyze.failed", {
                          count: analyzeProgress.failed,
                        })}
                      </span>
                    )}
                  </div>
                  <div className="h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                    <div
                      className="h-full bg-emerald-500 transition-all duration-200"
                      style={{
                        width: `${
                          analyzeProgress.total > 0
                            ? Math.round(
                                (analyzeProgress.processed /
                                  analyzeProgress.total) *
                                  100,
                              )
                            : 0
                        }%`,
                      }}
                    />
                  </div>
                </div>
              )}
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4 flex-1 min-w-0">
                <ImageIcon
                  size={20}
                  className="text-zinc-400 shrink-0"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.localArtistImages.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {localArtistRescanStatus ??
                      t("settings.localArtistImages.subtitle")}
                  </div>
                </div>
              </div>
              <button
                type="button"
                onClick={handleRescanLocalArtistImages}
                disabled={isRescanningLocalArtists}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <ImageIcon
                  size={14}
                  aria-hidden="true"
                  className={isRescanningLocalArtists ? "animate-pulse" : ""}
                />
                <span>{t("settings.localArtistImages.action")}</span>
              </button>
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4 flex-1 min-w-0">
                <ImageIcon
                  size={20}
                  className="text-zinc-400 shrink-0"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.artistImages.title")}
                  </div>
                  {artistFetchProgress && isFetchingArtists ? (
                    <div className="text-xs text-zinc-500 mt-1 truncate">
                      {t("settings.artistImages.progress", {
                        current: artistFetchProgress.current,
                        total: artistFetchProgress.total,
                      })}
                      {artistFetchProgress.artistName
                        ? ` — ${artistFetchProgress.artistName}`
                        : ""}
                    </div>
                  ) : (
                    <div className="text-xs text-zinc-400">
                      {t("settings.artistImages.subtitle")}
                    </div>
                  )}
                  {artistFetchProgress && artistFetchProgress.total > 0 && (
                    <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                      <div
                        className="h-full bg-emerald-500 transition-all"
                        style={{
                          width: `${Math.min(100, (artistFetchProgress.current / artistFetchProgress.total) * 100)}%`,
                        }}
                      />
                    </div>
                  )}
                </div>
              </div>
              <button
                type="button"
                onClick={handleFetchMissingArtistPictures}
                disabled={isFetchingArtists}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <ImageIcon
                  size={14}
                  aria-hidden="true"
                  className={isFetchingArtists ? "animate-pulse" : ""}
                />
                <span>{t("settings.artistImages.action")}</span>
              </button>
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4 flex-1 min-w-0">
                <ImageIcon
                  size={20}
                  className="text-zinc-400 shrink-0"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("library.fetchMissingCovers")}
                  </div>
                  {coverProgress && isFetchingCovers ? (
                    <div className="text-xs text-zinc-500 mt-1 truncate">
                      {t("library.fetchingCovers", {
                        current: coverProgress.current,
                        total: coverProgress.total,
                      })}
                      {coverProgress.albumTitle
                        ? ` — ${coverProgress.albumTitle}`
                        : ""}
                    </div>
                  ) : coverResultMsg ? (
                    <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                      {coverResultMsg}
                    </div>
                  ) : (
                    <div className="text-xs text-zinc-400">
                      {t("settings.artistImages.subtitle")}
                    </div>
                  )}
                  {coverProgress && coverProgress.total > 0 && (
                    <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                      <div
                        className="h-full bg-emerald-500 transition-all"
                        style={{
                          width: `${Math.min(100, (coverProgress.current / coverProgress.total) * 100)}%`,
                        }}
                      />
                    </div>
                  )}
                </div>
              </div>
              <button
                type="button"
                onClick={handleFetchMissingCovers}
                disabled={isFetchingCovers}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <ImageIcon
                  size={14}
                  aria-hidden="true"
                  className={isFetchingCovers ? "animate-pulse" : ""}
                />
                <span>{t("settings.artistImages.action")}</span>
              </button>
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4 flex-1 min-w-0">
                <Mic2
                  size={20}
                  className="text-zinc-400 shrink-0"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.lyricsPrefetch.title")}
                  </div>
                  {lyricsPrefetchProgress && isPrefetchingLyrics ? (
                    <div className="text-xs text-zinc-500 mt-1 truncate">
                      {t("settings.lyricsPrefetch.progress", {
                        current: lyricsPrefetchProgress.processed,
                        total: lyricsPrefetchProgress.total,
                        hits: lyricsPrefetchProgress.hits,
                      })}
                      {lyricsPrefetchProgress.currentTitle
                        ? ` — ${lyricsPrefetchProgress.currentTitle}`
                        : ""}
                    </div>
                  ) : lyricsResultMsg ? (
                    <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                      {lyricsResultMsg}
                    </div>
                  ) : (
                    <div className="text-xs text-zinc-400">
                      {t("settings.lyricsPrefetch.subtitle")}
                    </div>
                  )}
                  {lyricsPrefetchProgress &&
                    lyricsPrefetchProgress.total > 0 && (
                      <div className="mt-2 h-1.5 w-full max-w-xs rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                        <div
                          className="h-full bg-emerald-500 transition-all"
                          style={{
                            width: `${Math.min(100, (lyricsPrefetchProgress.processed / lyricsPrefetchProgress.total) * 100)}%`,
                          }}
                        />
                      </div>
                    )}
                </div>
              </div>
              {isPrefetchingLyrics ? (
                <button
                  type="button"
                  onClick={handleCancelPrefetchLyrics}
                  className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                >
                  <span>{t("common.cancel")}</span>
                </button>
              ) : (
                <button
                  type="button"
                  onClick={handlePrefetchLyrics}
                  className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  <Mic2 size={14} aria-hidden="true" />
                  <span>{t("settings.lyricsPrefetch.action")}</span>
                </button>
              )}
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4 flex-1 min-w-0">
                <ImageIcon
                  size={20}
                  className="text-zinc-400 shrink-0"
                  aria-hidden="true"
                />
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.regenerateThumbnails")}
                  </div>
                  {thumbsStatus ? (
                    <div className="text-xs text-emerald-600 dark:text-emerald-400 mt-1 truncate">
                      {thumbsStatus}
                    </div>
                  ) : (
                    <div className="text-xs text-zinc-400">
                      {t("settings.regenerateThumbnailsSubtitle")}
                    </div>
                  )}
                </div>
              </div>
              <button
                type="button"
                onClick={handleRegenerateThumbnails}
                disabled={isRegeneratingThumbs}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCcw
                  size={14}
                  aria-hidden="true"
                  className={isRegeneratingThumbs ? "animate-spin" : ""}
                />
                <span>{t("settings.regenerateThumbnailsAction")}</span>
              </button>
            </div>

            {/* Profile export / import — packages the per-profile DB
              + manual artwork into a single .waveflow archive. Useful
              for backups + machine migration. Shared metadata cache
              and Last.fm key live in app.db so they're not bundled. */}
            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center justify-between gap-4">
                <div className="flex items-center space-x-4 min-w-0">
                  <Download
                    size={20}
                    className="text-zinc-400 shrink-0"
                    aria-hidden="true"
                  />
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-zinc-900 dark:text-white">
                      {t("settings.profileIo.title")}
                    </div>
                    <div className="text-xs text-zinc-400">
                      {t("settings.profileIo.subtitle")}
                    </div>
                  </div>
                </div>
                <div className="flex items-center space-x-2 shrink-0">
                  <button
                    type="button"
                    onClick={handleExportProfile}
                    disabled={profileIoBusy != null || !activeProfile}
                    className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <Download
                      size={14}
                      aria-hidden="true"
                      className={
                        profileIoBusy === "export" ? "animate-pulse" : ""
                      }
                    />
                    <span>{t("settings.profileIo.export.action")}</span>
                  </button>
                  <button
                    type="button"
                    onClick={handleImportProfile}
                    disabled={profileIoBusy != null}
                    className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <Upload
                      size={14}
                      aria-hidden="true"
                      className={
                        profileIoBusy === "import" ? "animate-pulse" : ""
                      }
                    />
                    <span>{t("settings.profileIo.import.action")}</span>
                  </button>
                </div>
              </div>
              {profileIoStatus && (
                <div
                  className={`mt-2 ml-9 text-xs ${
                    profileIoStatus.kind === "ok"
                      ? "text-emerald-600 dark:text-emerald-400"
                      : "text-red-500"
                  }`}
                >
                  {profileIoStatus.message}
                </div>
              )}
            </div>

            {/* Auto-backup card — sits right after the manual export/import
              so users see the two profile-IO features together. */}
            <BackupCard language={i18n.resolvedLanguage ?? i18n.language} />

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <FolderOpen
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.dataFolder.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.dataFolder.subtitle")}
                  </div>
                </div>
              </div>
              <button
                type="button"
                onClick={handleOpenDataFolder}
                aria-label={t("settings.openDataFolder")}
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                <FolderOpen size={14} aria-hidden="true" />
                <span>{t("settings.dataFolder.action")}</span>
              </button>
            </div>

            <div className="flex items-center justify-between py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center space-x-4">
                <Trash2
                  size={20}
                  className="text-zinc-400"
                  aria-hidden="true"
                />
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.reset.title")}
                  </div>
                  <div className="text-xs text-zinc-400">
                    {t("settings.reset.subtitle")}
                  </div>
                </div>
              </div>
              <button
                type="button"
                className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-red-200 bg-white text-sm font-medium text-red-500 hover:bg-red-50 dark:border-red-500/30 dark:bg-zinc-800 dark:hover:bg-red-500/10 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
              >
                <Trash2 size={14} aria-hidden="true" />
                <span>{t("settings.reset.action")}</span>
              </button>
            </div>
          </div>
        </section>
      )}

      {/* Shortcuts category — keyboard shortcut editor. */}
      {activeCategory === "shortcuts" && (
        <section
          role="tabpanel"
          id="settings-panel-shortcuts"
          aria-labelledby="settings-tab-shortcuts"
          tabIndex={0}
        >
          <h2
            id="settings-shortcuts-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.shortcuts")}
          </h2>
          <ShortcutsCard />
        </section>
      )}

      {/* Diagnostics category — logs, version. */}
      {activeCategory === "diagnostics" && (
        <section
          role="tabpanel"
          id="settings-panel-diagnostics"
          aria-labelledby="settings-tab-diagnostics"
          tabIndex={0}
        >
          <h2
            id="settings-diagnostics-heading"
            className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
          >
            {t("settings.sections.diagnostics")}
          </h2>
          <div className="space-y-1">
            <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
              <div className="flex items-center justify-between gap-4">
                <div className="flex items-center space-x-4 min-w-0">
                  <FileText
                    size={20}
                    className="text-zinc-400 shrink-0"
                    aria-hidden="true"
                  />
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-zinc-900 dark:text-white">
                      {t("settings.diagnostics.title")}
                    </div>
                    <div className="text-xs text-zinc-400">
                      {t("settings.diagnostics.subtitle")}
                    </div>
                  </div>
                </div>
                <div className="flex items-center space-x-2 shrink-0">
                  <button
                    type="button"
                    onClick={handleOpenLogFolder}
                    className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                  >
                    <FolderOpen size={14} aria-hidden="true" />
                    <span>{t("settings.diagnostics.openFolder")}</span>
                  </button>
                  <button
                    type="button"
                    onClick={handleCopyLogs}
                    className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                  >
                    {copyLogsStatus === "ok" ? (
                      <CheckIcon
                        size={14}
                        aria-hidden="true"
                        className="text-emerald-500"
                      />
                    ) : (
                      <Copy size={14} aria-hidden="true" />
                    )}
                    <span>
                      {copyLogsStatus === "ok"
                        ? t("settings.diagnostics.copied")
                        : copyLogsStatus === "fail"
                          ? t("settings.diagnostics.copyFailed")
                          : t("settings.diagnostics.copyLogs")}
                    </span>
                  </button>
                </div>
              </div>
            </div>
          </div>
        </section>
      )}

      <DuplicatesModal
        isOpen={isDuplicatesOpen}
        onClose={() => setIsDuplicatesOpen(false)}
      />
    </div>
  );
}
