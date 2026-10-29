import {
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { SettingsNavigation } from "./settings/SettingsNavigation";
import {
  AppearanceSection,
  AudioSection,
  DataSection,
  DiagnosticsSection,
  GeneralSection,
  IntegrationsSection,
  LibrarySection,
  LyricsSection,
  preloadSettingsSections,
  ShortcutsSection,
} from "./settings/sections";
import {
  SETTINGS_CATEGORIES,
  type SettingsCategory,
} from "./settings/settingsCatalog";
import "./settings/settings.css";
import {
  dopPlatformSupported as isDopPlatform,
  SettingsResetScope,
  type CompletedReset,
} from "./settings/settingsReset";
import { SETTINGS } from "../../lib/settingsRegistry";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import { useSortIgnoreArticles } from "../../hooks/useSortIgnoreArticles";
import { ArrowLeft } from "lucide-react";
import { getProfileSetting, setProfileSetting } from "../../lib/tauri/profile";
import { useSingleClickPlay } from "../../hooks/useSingleClickPlay";
import type { ViewId } from "../../types";
import {
  playerGetAudioSettings,
  playerSetNormalize,
  playerSetMono,
  playerSetPauseOnDeviceLoss,
  playerSetKeepAwake,
  playerSetMatchSourceRate,
  playerSetCrossfade,
  playerSetGapless,
  playerSetReplayGain,
  playerSetReplayGainOptions,
  playerSetReplayGainMode,
  playerSetDsdPrecision,
  playerSetDsdDop,
  DSD_PRECISION_TAPS,
  type DsdPrecisionTaps,
  type ReplayGainMode,
} from "../../lib/tauri/player";
import {
  getDiscordRpcEnabled,
  getLastfmApiKey,
  getLastfmApiSecret,
  getNotificationsTrackChange,
  lastfmGetStatus,
  lastfmLogin,
  lastfmLogout,
  setDiscordRpcEnabled,
  setLastfmApiKey,
  setLastfmApiSecret,
  setNotificationsTrackChange,
  type LastfmStatus,
} from "../../lib/tauri/integration";
import {
  batchFetchMissingAlbumCovers,
  batchFetchMissingArtistPictures,
} from "../../lib/tauri/deezer";
import { openLogFolder, readRecentLogs } from "../../lib/tauri/diagnostics";
import { getOfflineMode, setOfflineMode } from "../../lib/tauri/offline";
import {
  ARCHIVE_UNREADABLE,
  exportProfile,
  importProfile,
  previewProfileArchive,
} from "../../lib/tauri/profile_io";
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
import {
  mpdGetConfig,
  mpdGetStatus,
  mpdSetConfig,
  type MpdConfig,
  type MpdStatus,
} from "../../lib/tauri/mpd";
import { generatePassword } from "../../lib/password";
import { listen } from "@tauri-apps/api/event";
import { useLibrary } from "../../hooks/useLibrary";
import { useTasks } from "../../hooks/useTasks";
import { useProfile } from "../../hooks/useProfile";
import { usePageScroll } from "../../hooks/usePageScroll";
import { useProfileScopedToggle } from "../../hooks/useProfileScopedToggle";
import { useAutoplay } from "../../hooks/useAutoplay";
import { useGeneratorAlbumMode } from "../../hooks/useGeneratorAlbumMode";
import { invoke } from "@tauri-apps/api/core";
import {
  regenerateThumbnails,
  pruneCachedAlbumCovers,
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
} from "../../lib/tauri/preferences";
import { applyUiZoom } from "../../hooks/useUiZoom";
import { DuplicatesModal } from "../common/DuplicatesModal";
import { ResetAppModal } from "../common/ResetAppModal";
import { ViewErrorBoundary } from "../common/ViewErrorBoundary";
import {
  ProfileArchivePreviewModal,
  type ArchivePreviewState,
} from "../common/ProfileArchivePreviewModal";

interface SettingsViewProps {
  onNavigate: (view: ViewId) => void;
  /**
   * Open on this category instead of the one the user last had open.
   *
   * For the callers that send someone here to see a *particular* card:
   * the remembered category is the right default for opening Settings,
   * and the wrong one when something has just said "look at this".
   */
  initialCategory?: SettingsCategory;
}

export type { SettingsCategory } from "./settings/settingsCatalog";

const SETTINGS_CATEGORY_STORAGE_KEY = "waveflow.settings.activeCategory";
const SETTINGS_CATEGORY_IDS = new Set<SettingsCategory>(
  SETTINGS_CATEGORIES.map((c) => c.id),
);

/**
 * Runs `onReveal` once the page it sits beside has mounted. Inside the
 * same `Suspense` boundary as the page, so a search result for a page
 * whose chunk is still loading scrolls to its group when the group
 * exists, not before.
 */
function RevealOnMount({
  request,
  onReveal,
}: {
  request: { group: string | null; seq: number } | null;
  onReveal: (group: string | null) => void;
}) {
  // The latest callback, without making a new render re-run a reveal
  // that already happened: only a new request does.
  const onRevealRef = useRef(onReveal);
  useLayoutEffect(() => {
    onRevealRef.current = onReveal;
  });
  useEffect(() => {
    if (request == null) return;
    requestAnimationFrame(() => onRevealRef.current(request.group));
  }, [request]);
  return null;
}

/** Holds a page's place, unseen, while its chunk loads. */
function SectionFallback() {
  const { t } = useTranslation();
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={t("common.loading")}
      className="settings-category min-h-[50vh]"
    />
  );
}

function readStoredCategory(): SettingsCategory {
  if (typeof window === "undefined") return "general";
  try {
    const stored = window.localStorage.getItem(SETTINGS_CATEGORY_STORAGE_KEY);
    if (stored && SETTINGS_CATEGORY_IDS.has(stored as SettingsCategory)) {
      return stored as SettingsCategory;
    }
  } catch {
    // localStorage unavailable — fall through to default.
  }
  return "general";
}

/**
 * Settings, remounted whole after a category is reset to its defaults.
 *
 * Much of what a page shows is state read once from a command when the
 * view mounts: the audio engine's switches, the DLNA and MPD configs, the
 * cards that keep a value of their own. A reset writes through those same
 * commands, and a fresh mount is the one refresh that reaches all of them;
 * the preferences that live in a hook or a provider are told through their
 * own events and never depended on this.
 *
 * The page the reset happened on is the one the fresh mount opens, not
 * whatever `initialCategory` last asked for.
 */
export function SettingsView(props: SettingsViewProps) {
  const [generation, setGeneration] = useState(0);
  const [resetCategory, setResetCategory] = useState<SettingsCategory | null>(
    null,
  );
  // A new request from outside wins over the page a reset left open.
  const [lastInitialCategory, setLastInitialCategory] = useState(
    props.initialCategory,
  );
  if (lastInitialCategory !== props.initialCategory) {
    setLastInitialCategory(props.initialCategory);
    setResetCategory(null);
  }
  // Above the remount, so the fresh header learns what it follows.
  const [completed, setCompleted] = useState<CompletedReset | null>(null);
  const handleCategoryReset = useCallback(
    (category: SettingsCategory, failed: number) => {
      setResetCategory(category);
      setCompleted({ category, failed });
      setGeneration((g) => g + 1);
    },
    [],
  );
  const consume = useCallback(() => setCompleted(null), []);
  const scope = useMemo(
    () => ({ onCategoryReset: handleCategoryReset, completed, consume }),
    [handleCategoryReset, completed, consume],
  );
  return (
    <SettingsResetScope.Provider value={scope}>
      <SettingsViewContent
        key={generation}
        {...props}
        initialCategory={resetCategory ?? props.initialCategory}
      />
    </SettingsResetScope.Provider>
  );
}

function SettingsViewContent({
  onNavigate,
  initialCategory,
}: SettingsViewProps) {
  const { t, i18n } = useTranslation();
  const { libraries, rescanLibrary } = useLibrary();
  // Category tab the user is currently viewing. Persisted to
  // localStorage so re-entering Settings lands them on the same tab
  // they last had open — small but expected polish.
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>(
    () => initialCategory ?? readStoredCategory(),
  );
  // Group a settings-search result asked to reveal; cleared whenever the
  // category changes by any other route, so it never outlives its request.
  const [targetGroup, setTargetGroup] = useState<string | null>(null);
  // The latest navigation request, for `RevealOnMount`: a new object per
  // request, so asking for the same group twice still scrolls to it.
  const [revealRequest, setRevealRequest] = useState<{
    group: string | null;
    seq: number;
  } | null>(null);
  // The banner that sends people here is visible *on* this page too, so
  // the request can arrive while Settings is already mounted — React
  // keeps the instance and the initializer above never runs again. The
  // value changes once per request, so a tab the user picks afterwards
  // is not fought over.
  useEffect(() => {
    // Falling back to the stored category rather than returning early:
    // two Settings entries can sit next to each other in history — the
    // banner pushes one while Settings is already open — and going back
    // to the ordinary one has to leave Diagnostics rather than stay
    // there. On a first mount this repeats what the initializer already
    // chose, which costs nothing.
    /* eslint-disable-next-line react-hooks/set-state-in-effect */
    setActiveCategory(initialCategory ?? readStoredCategory());
    // A restored entry carries no search target: a group revealed by an
    // earlier search result must not stay expanded in it.
    setTargetGroup(null);
  }, [initialCategory]);
  const panelRef = useRef<HTMLDivElement>(null);
  const pageScroll = usePageScroll();
  const handleCategoryChange = useCallback((next: SettingsCategory) => {
    setActiveCategory(next);
    setTargetGroup(null);
    try {
      window.localStorage.setItem(SETTINGS_CATEGORY_STORAGE_KEY, next);
    } catch {
      // localStorage unavailable — not worth surfacing.
    }
  }, []);
  // Whether a sweep is running, and how far it is, come from the task
  // registry, which outlives this view: a flag of our own was lost on
  // the way to another page and back, and a bar we cleared ourselves
  // stayed full once the view that would have cleared it had gone.
  // `isStartingAnalysis` only covers the moment between the click and
  // the task registering.
  const tasks = useTasks();
  const analysisTask = tasks.find((task) => task.kind === "analysis");
  const [isStartingAnalysis, setIsStartingAnalysis] = useState(false);
  const isAnalyzingLib = isStartingAnalysis || analysisTask != null;
  // Failures are not in the registry; the sweep's own event carries them.
  const [analyzeFailed, setAnalyzeFailed] = useState(0);
  const [autoAnalyze, setAutoAnalyzeState] = useState<boolean>(
    SETTINGS["audio.auto_analyze"].defaultValue,
  );

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
  const { activeProfile, refresh: refreshProfiles } = useProfile();
  const [isRescanning, setIsRescanning] = useState(false);
  const [autoStart, setAutoStart] = useState<boolean>(
    SETTINGS["app.auto_start"].defaultValue,
  );
  const [minimizeToTray, setMinimizeToTray] = useState<boolean>(
    SETTINGS["app.minimize_to_tray"].defaultValue,
  );
  // UI zoom slider value. Hydrated from `app_setting` and kept in
  // sync with the `Ctrl+=` / `Ctrl+-` / `Ctrl+0` shortcuts via the
  // window-level event the `useUiZoom` hook broadcasts.
  const [uiZoom, setUiZoom] = useState<number>(
    SETTINGS["ui.zoom_level"].defaultValue,
  );
  const [scanOnStart, setScanOnStart] = useState<boolean>(
    SETTINGS["library.scan_on_start"].defaultValue,
  );
  const { value: singleClickPlay, setValue: setSingleClickPlay } =
    useSingleClickPlay();
  const [isDuplicatesOpen, setIsDuplicatesOpen] = useState(false);
  const [isResetOpen, setIsResetOpen] = useState(false);
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
    // Sleep timer / A-B loop / audio-quality-footer visibility now
    // live inside `PlayerBarLayoutCard` and are read through the
    // shared `usePlayerBarLayout` hook — no need to hydrate them
    // here anymore.
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

  // Through the shared hook, which broadcasts the write: the lists already
  // on screen pick it up without being remounted.
  const handleToggleSingleClickPlay = useCallback(() => {
    void setSingleClickPlay((previous) => !previous);
  }, [setSingleClickPlay]);

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

  const { ignoreArticles, setIgnoreArticles } = useSortIgnoreArticles();
  // The switch moves once the choice is saved, so a second click before
  // then would flip from the same value; it waits instead.
  const [savingIgnoreArticles, setSavingIgnoreArticles] = useState(false);
  const handleToggleIgnoreArticles = useCallback(() => {
    setSavingIgnoreArticles(true);
    setIgnoreArticles(!ignoreArticles)
      .catch((err) =>
        console.error("[Settings] set sort_ignore_articles failed", err),
      )
      .finally(() => setSavingIgnoreArticles(false));
  }, [ignoreArticles, setIgnoreArticles]);

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
        // Per library, like the two loops in `LibraryView`: one
        // unreadable library used to abort the rescan of every library
        // after it, and the user was told nothing about the ones that
        // never ran.
        try {
          const summary = await rescanLibrary(lib.id);
          // Stopping the scan stops the whole rescan, not just the
          // library it happened to be on.
          if (summary.cancelled) break;
        } catch (err) {
          console.error(
            `[SettingsView] rescan failed for library ${lib.id}`,
            err,
          );
        }
      }
    } catch (err) {
      console.error("[SettingsView] rescan failed", err);
    } finally {
      setIsRescanning(false);
    }
  };

  // `analysis:progress` for the failure count, which the task registry
  // does not carry.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{
      processed: number;
      total: number;
      current_track_id: number | null;
      failed: number;
    }>("analysis:progress", (event) => {
      setAnalyzeFailed(event.payload.failed);
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
    setIsStartingAnalysis(true);
    setAnalyzeFailed(0);
    try {
      const { analyzeLibrary } = await import("../../lib/tauri/analysis");
      await analyzeLibrary();
    } catch (err) {
      console.error("[SettingsView] analyze library failed", err);
    } finally {
      setIsStartingAnalysis(false);
    }
  };

  const handleCancelAnalyzeLibrary = async () => {
    try {
      const { cancelLibraryAnalysis } =
        await import("../../lib/tauri/analysis");
      await cancelLibraryAnalysis();
      // Nothing to flip here: the backend exits at the next track
      // boundary, and the task leaving the registry is what turns the
      // button back, whichever view started the sweep.
    } catch (err) {
      console.error("[SettingsView] cancel analyze library failed", err);
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

  // Lyrics translation language (Musixmatch) — issue #208.
  // `null` = no translation (default). The backend whitelist must
  // stay in sync with `SUPPORTED_TRANSLATION_LANGS` in
  // `commands/lyrics/settings.rs`; we propagate the same set + an "off"
  // sentinel here.
  const [translationLang, setTranslationLang] = useState<string | null>(
    SETTINGS["lyrics.translation_lang"].defaultValue,
  );
  const [translationLangBusy, setTranslationLangBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const { getLyricsTranslationLang } =
          await import("../../lib/tauri/lyrics");
        const stored = await getLyricsTranslationLang();
        if (!cancelled) setTranslationLang(stored);
      } catch (err) {
        console.error("[SettingsView] getLyricsTranslationLang failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleTranslationLangChange = async (next: string) => {
    setTranslationLangBusy(true);
    try {
      const { setLyricsTranslationLang } =
        await import("../../lib/tauri/lyrics");
      const stored = await setLyricsTranslationLang(next === "" ? null : next);
      setTranslationLang(stored);
    } catch (err) {
      console.error("[SettingsView] setLyricsTranslationLang failed", err);
    } finally {
      setTranslationLangBusy(false);
    }
  };

  // Prefer LRCLIB / online providers over the track's embedded + sidecar
  // lyrics — issue #378. `false` = local-first (default). The flag is
  // per-profile, so the loader re-runs on profile switch and the toggle
  // stays busy (disabled) until the new profile's value has hydrated.
  const [preferLrclib, setPreferLrclib] = useState<boolean>(
    SETTINGS["lyrics.prefer_lrclib"].defaultValue,
  );
  const [preferLrclibBusy, setPreferLrclibBusy] = useState(true);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // Reset to a neutral, disabled state while the new profile's flag
      // loads so a stale value never shows and can't be toggled
      // mid-hydration. Inside the async IIFE (not the effect body) to
      // satisfy `react-hooks/set-state-in-effect`.
      setPreferLrclib(SETTINGS["lyrics.prefer_lrclib"].defaultValue);
      setPreferLrclibBusy(true);
      try {
        const { getPreferLrclib } = await import("../../lib/tauri/lyrics");
        const stored = await getPreferLrclib();
        if (!cancelled) setPreferLrclib(stored);
      } catch (err) {
        console.error("[SettingsView] getPreferLrclib failed", err);
      } finally {
        if (!cancelled) setPreferLrclibBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id]);

  const handlePreferLrclibToggle = async () => {
    if (preferLrclibBusy) return; // guard re-entrancy (pointer + keyboard)
    const next = !preferLrclib;
    setPreferLrclib(next); // optimistic
    setPreferLrclibBusy(true);
    try {
      const { setPreferLrclib: persist } =
        await import("../../lib/tauri/lyrics");
      await persist(next);
    } catch (err) {
      console.error("[SettingsView] setPreferLrclib failed", err);
      setPreferLrclib(!next); // revert on failure
    } finally {
      setPreferLrclibBusy(false);
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

  const [isPruningCovers, setIsPruningCovers] = useState(false);
  const [pruneCoversStatus, setPruneCoversStatus] = useState<{
    ok: boolean;
    text: string;
  } | null>(null);
  // Auto-clear timer for the prune status — kept in a ref so a rapid re-run (or
  // an unmount) cancels the previous one instead of clearing a newer result.
  const pruneCoversTimeoutRef = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (pruneCoversTimeoutRef.current != null) {
        window.clearTimeout(pruneCoversTimeoutRef.current);
      }
    },
    [],
  );

  const handlePruneCachedCovers = useCallback(async () => {
    if (isPruningCovers) return;
    setIsPruningCovers(true);
    setPruneCoversStatus(null);
    try {
      const r = await pruneCachedAlbumCovers();
      // Locale-aware one-decimal MB (e.g. "12,5" in fr) rather than the
      // always-`.` `toFixed`.
      const mb = new Intl.NumberFormat(i18n.resolvedLanguage ?? i18n.language, {
        minimumFractionDigits: 1,
        maximumFractionDigits: 1,
      }).format(r.bytesFreed / (1024 * 1024));
      setPruneCoversStatus({
        ok: true,
        // `count`, not `files`: i18next selects the plural form on
        // `count` alone, so the old name left every language on its
        // plural wording at one file deleted.
        text: t("settings.pruneAlbumCoversDone", { count: r.filesDeleted, mb }),
      });
    } catch (err) {
      console.error("[SettingsView] prune cached covers failed", err);
      setPruneCoversStatus({
        ok: false,
        text: t("settings.pruneAlbumCoversFailed"),
      });
    } finally {
      setIsPruningCovers(false);
      if (pruneCoversTimeoutRef.current != null) {
        window.clearTimeout(pruneCoversTimeoutRef.current);
      }
      pruneCoversTimeoutRef.current = window.setTimeout(
        () => setPruneCoversStatus(null),
        5000,
      );
    }
  }, [isPruningCovers, t, i18n.language, i18n.resolvedLanguage]);

  // Audio settings — hydrated from backend at mount.
  const [normalize, setNormalize] = useState<boolean>(
    SETTINGS["audio.normalize"].defaultValue,
  );
  const [mono, setMono] = useState<boolean>(
    SETTINGS["audio.mono"].defaultValue,
  );
  // #617. Mirrors the engine's own default, so the row does not flip
  // under the user between the first paint and the read below.
  const [pauseOnDeviceLoss, setPauseOnDeviceLoss] = useState<boolean>(
    SETTINGS["audio.pause_on_device_loss"].defaultValue,
  );
  // Keep the machine awake while playing; the backend's default too.
  const [keepAwake, setKeepAwake] = useState<boolean>(
    SETTINGS["playback.keep_awake"].defaultValue,
  );
  // Which of these two the user has touched since the audio-settings
  // read in flight started. That read is an IPC round trip, and a toggle
  // during it would otherwise be painted over by the value it just
  // replaced — the write still lands, so the UI would then disagree with
  // the database until the next hydration. Cleared when a read starts,
  // and consulted when it resolves.
  const audioSettingsTouched = useRef<Set<string>>(new Set());
  // Bumped when one of those writes fails, to re-run the read below. The
  // optimistic rollback restores the value the click replaced, and after
  // a failed write that value may itself never have been persisted — so
  // the panel is refilled from the backend rather than from a guess.
  const [audioSettingsNonce, setAudioSettingsNonce] = useState(0);
  // #600. Off, like the engine: a reopen costs an audible gap, so this
  // is a trade rather than an improvement.
  const [matchSourceRate, setMatchSourceRate] = useState<boolean>(
    SETTINGS["audio.match_source_rate"].defaultValue,
  );
  const [crossfadeSec, setCrossfadeSec] = useState<number>(
    SETTINGS["audio.crossfade_ms"].defaultValue / 1000,
  );
  const [replayGain, setReplayGain] = useState<boolean>(
    SETTINGS["audio.replaygain"].defaultValue,
  );
  const [replayGainPreamp, setReplayGainPreamp] = useState<number>(
    SETTINGS["audio.replaygain_preamp"].defaultValue,
  );
  const [replayGainFallback, setReplayGainFallback] = useState<number>(
    SETTINGS["audio.replaygain_fallback"].defaultValue,
  );
  const {
    enabled: generatorAlbumMode,
    resolved: generatorAlbumModeResolved,
    setEnabled: setGeneratorAlbumMode,
  } = useGeneratorAlbumMode();
  const {
    enabled: autoplay,
    resolved: autoplayResolved,
    setEnabled: setAutoplay,
  } = useAutoplay();
  const [replayGainMode, setReplayGainMode] = useState<ReplayGainMode>(
    SETTINGS["audio.replaygain_mode"].defaultValue,
  );
  const [replayGainPreventClipping, setReplayGainPreventClipping] =
    useState<boolean>(
      SETTINGS["audio.replaygain_prevent_clipping"].defaultValue,
    );
  const replayGainOptionsDebounce = useRef<number | null>(null);
  const [gapless, setGapless] = useState<boolean>(
    SETTINGS["audio.gapless"].defaultValue,
  );
  const [dsdTaps, setDsdTaps] = useState<DsdPrecisionTaps>(
    SETTINGS["audio.dsd_precision"].defaultValue,
  );
  const [dsdDop, setDsdDop] = useState<boolean>(
    SETTINGS["audio.dsd_dop"].defaultValue,
  );
  // DoP needs exclusive device access, which WaveFlow has on every
  // desktop OS: WASAPI Exclusive (Windows), raw ALSA `hw:` (Linux),
  // CoreAudio hog mode (macOS). Hidden on mobile (#495 / #497). UA sniff
  // like the ExclusiveModeCard; the WebView is platform-pinned. Shared
  // with the page's modified count, which must skip what is not shown.
  const dopPlatformSupported = isDopPlatform();

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

  // Discord Rich Presence. Hydrated once at mount, flipped
  // optimistically with rollback on failure.
  const [discordRpc, setDiscordRpc] = useState<boolean>(
    SETTINGS["integrations.discord_rpc"].defaultValue,
  );
  // Native OS track-change toast — opt-in (default OFF). Persisted
  // app-wide in `app_setting['notifications.track_change']`.
  const [trackChangeNotif, setTrackChangeNotif] = useState<boolean>(
    SETTINGS["notifications.track_change"].defaultValue,
  );

  // Global offline-mode flag. Persisted in app_setting (process-wide,
  // not per-profile), hydrated at mount.
  const [offlineMode, setOfflineModeState] = useState<boolean>(
    SETTINGS["network.offline_mode"].defaultValue,
  );

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

  // One timer at a time: a result's pending clear must neither wipe the
  // next result early nor hide the "running" line of the next operation.
  const profileIoStatusTimer = useRef<number | null>(null);
  const clearProfileIoStatus = useCallback(() => {
    if (profileIoStatusTimer.current != null) {
      window.clearTimeout(profileIoStatusTimer.current);
      profileIoStatusTimer.current = null;
    }
    setProfileIoStatus(null);
  }, []);
  const flashStatus = useCallback(
    (kind: "ok" | "fail", message: string) => {
      clearProfileIoStatus();
      setProfileIoStatus({ kind, message });
      profileIoStatusTimer.current = window.setTimeout(() => {
        profileIoStatusTimer.current = null;
        setProfileIoStatus(null);
      }, 4000);
    },
    [clearProfileIoStatus],
  );
  useEffect(() => clearProfileIoStatus, [clearProfileIoStatus]);

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
    clearProfileIoStatus();
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
  }, [activeProfile, clearProfileIoStatus, flashStatus, profileIoBusy, t]);

  // The archive picked for import, and what it was found to hold. The
  // import starts only from the preview's own button, which the backend's
  // verdict enables: nothing is created before the user has seen what the
  // archive is and that this build can take it.
  const [archivePreview, setArchivePreview] =
    useState<ArchivePreviewState | null>(null);
  const archiveSourceRef = useRef<string | null>(null);
  // Bumped per pick, so a slow preview of an archive the user already
  // moved on from cannot fill the dialog.
  const archivePreviewSeqRef = useRef(0);

  const handleImportProfile = useCallback(async () => {
    if (profileIoBusy) return;
    const source = await pickFile(
      ["waveflow"],
      t("settings.profileIo.import.dialogTitle") ?? undefined,
    );
    if (!source) return;
    clearProfileIoStatus();
    const seq = ++archivePreviewSeqRef.current;
    archiveSourceRef.current = source;
    setArchivePreview({ status: "loading" });
    try {
      const preview = await previewProfileArchive(source);
      if (seq !== archivePreviewSeqRef.current) return;
      setArchivePreview({ status: "ready", preview });
    } catch (err) {
      console.error("[SettingsView] preview profile archive failed", err);
      if (seq !== archivePreviewSeqRef.current) return;
      setArchivePreview({
        status: String(err).includes(ARCHIVE_UNREADABLE)
          ? "unreadable"
          : "failed",
      });
    }
  }, [clearProfileIoStatus, profileIoBusy, t]);

  const handleCancelArchivePreview = useCallback(() => {
    archivePreviewSeqRef.current += 1;
    archiveSourceRef.current = null;
    setArchivePreview(null);
  }, []);

  const handleConfirmImportProfile = useCallback(async () => {
    const source = archiveSourceRef.current;
    if (profileIoBusy || !source) return;
    archiveSourceRef.current = null;
    setArchivePreview(null);
    clearProfileIoStatus();
    setProfileIoBusy("import");
    try {
      const imported = await importProfile(source, null);
      // The selector lists what the context last fetched; without this
      // the message points at a profile the selector does not show.
      await refreshProfiles();
      flashStatus(
        "ok",
        t("settings.profileIo.import.done", { name: imported.name }),
      );
    } catch (err) {
      console.error("[SettingsView] import profile failed", err);
      flashStatus("fail", t("settings.profileIo.import.failed"));
    } finally {
      setProfileIoBusy(null);
    }
  }, [clearProfileIoStatus, flashStatus, profileIoBusy, refreshProfiles, t]);

  // DLNA / UPnP MediaServer. `dlnaConfig` carries the persisted
  // settings (name, port, enabled flag); `dlnaStatus` is the live
  // worker-thread snapshot (running, bound URL, last error).
  const [dlnaConfig, setDlnaConfig] = useState<DlnaConfig>({
    enabled: SETTINGS["dlna.enabled"].defaultValue,
    server_name: SETTINGS["dlna.server_name"].defaultValue,
    port: SETTINGS["dlna.port"].defaultValue,
  });
  const [dlnaStatus, setDlnaStatus] = useState<DlnaStatus | null>(null);
  const [dlnaUrlCopied, setDlnaUrlCopied] = useState(false);

  // MPD protocol server (issue #471). Same config/status split as DLNA:
  // `mpdConfig` is what the user set, `mpdStatus` is what the worker
  // actually bound — they differ when the configured port was taken and
  // the server scanned forward.
  const [mpdConfig, setMpdConfig] = useState<MpdConfig>({
    enabled: SETTINGS["mpd.enabled"].defaultValue,
    port: SETTINGS["mpd.port"].defaultValue,
    password: "",
  });
  const [mpdStatus, setMpdStatus] = useState<MpdStatus | null>(null);
  const [mpdAddressCopied, setMpdAddressCopied] = useState(false);
  // False until the initial `mpdGetConfig` settles, so the controls stay
  // disabled and the user can't edit placeholder defaults before the real
  // config arrives (a failed load still unlocks them, on the defaults).
  const [mpdHydrated, setMpdHydrated] = useState(false);
  // The password field shows its text once asked to, or once a generated
  // password was put in it: the user has to read it to type it into their
  // client.
  const [mpdPasswordVisible, setMpdPasswordVisible] = useState(false);
  // "Not now" on the suggestion to set a password. Session-only, and
  // cleared by the next switch-on: the suggestion is made on activation.
  const [mpdSuggestDismissed, setMpdSuggestDismissed] = useState(false);
  // A generated password that was saved, so the card can say where it
  // goes. Cleared by any later edit of the field.
  const [mpdPasswordGenerated, setMpdPasswordGenerated] = useState(false);
  // The last save was refused: the card says so instead of leaving the
  // rolled-back value to speak for itself.
  const [mpdSaveFailed, setMpdSaveFailed] = useState(false);
  const mpdPasswordInputRef = useRef<HTMLInputElement>(null);
  // A generated password is not in `mpdConfig` until the backend took it,
  // so any other save sent meanwhile would carry the old password and
  // overwrite it. The card's controls wait for the generation instead.
  const [mpdGenerating, setMpdGenerating] = useState(false);

  useEffect(() => {
    getDiscordRpcEnabled()
      .then(setDiscordRpc)
      .catch((err) =>
        console.error("[SettingsView] get discord rpc failed", err),
      );
    getNotificationsTrackChange()
      .then(setTrackChangeNotif)
      .catch((err) =>
        console.error("[SettingsView] get track-change notif failed", err),
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

  const handleToggleTrackChangeNotif = useCallback(() => {
    const next = !trackChangeNotif;
    setTrackChangeNotif(next);
    setNotificationsTrackChange(next).catch((err) => {
      console.error("[SettingsView] set track-change notif failed", err);
      setTrackChangeNotif(!next);
    });
  }, [trackChangeNotif]);

  // Last MPD config the backend confirmed — the rollback target when a
  // `mpdSetConfig` fails, so a failed write doesn't leave the optimistic value
  // on screen. `mpdTouchedRef` flips once the user issues a set, so a slow
  // initial hydration response can't clobber an action sent in the meantime.
  const mpdConfirmedRef = useRef<MpdConfig | null>(null);
  const mpdTouchedRef = useRef(false);
  // Serialize MPD saves + gate side effects on the latest revision, so a
  // toggle and an onBlur firing together can't apply out of order or let a
  // stale response clobber newer state.
  const mpdWriteChainRef = useRef<Promise<void>>(Promise.resolve());
  const mpdWriteSeqRef = useRef(0);

  // DLNA — load persisted config + live status at mount.
  useEffect(() => {
    dlnaGetConfig()
      .then(setDlnaConfig)
      .catch((err) => console.error("[SettingsView] dlna config", err));
    dlnaGetStatus()
      .then(setDlnaStatus)
      .catch((err) => console.error("[SettingsView] dlna status", err));
    // Drop a late hydration response if the user already acted on MPD — its
    // config would be stale relative to the set they just issued.
    mpdGetConfig()
      .then((cfg) => {
        if (mpdTouchedRef.current) return;
        mpdConfirmedRef.current = cfg;
        setMpdConfig(cfg);
      })
      .catch((err) => console.error("[SettingsView] mpd config", err))
      // Unlock the controls once the config settles — even on failure, where
      // the user then edits the defaults.
      .finally(() => setMpdHydrated(true));
    mpdGetStatus()
      .then((st) => {
        if (mpdTouchedRef.current) return;
        setMpdStatus(st);
      })
      .catch((err) => console.error("[SettingsView] mpd status", err));
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

  // `optimistic: false` shows `next` only once the backend took it — for a
  // generated password, which must never be on screen unless it is the one
  // the server now asks for.
  const persistMpd = useCallback(
    async (next: MpdConfig, { optimistic = true } = {}) => {
      mpdTouchedRef.current = true;
      const seq = ++mpdWriteSeqRef.current;
      setMpdSaveFailed(false);
      if (optimistic) setMpdConfig(next);
      // Chain each save so concurrent toggle/onBlur writes reach the backend
      // in order; only the latest revision commits its side effects.
      const run = mpdWriteChainRef.current.then(async () => {
        try {
          const status = await mpdSetConfig(next);
          // The writes are serialized, so every success — the latest or
          // not — is what the backend now holds, and what a later failure
          // has to roll back to.
          mpdConfirmedRef.current = next;
          if (seq !== mpdWriteSeqRef.current) return false;
          if (!optimistic) setMpdConfig(next);
          setMpdStatus(status);
          return true;
        } catch (err) {
          // The error is the backend's; the config — password included — is
          // never logged.
          console.error("[SettingsView] mpd set", err);
          if (seq !== mpdWriteSeqRef.current) return false;
          // Restore the last backend-confirmed config so a failed set doesn't
          // leave the optimistic value displayed as if it took.
          if (mpdConfirmedRef.current) setMpdConfig(mpdConfirmedRef.current);
          setMpdSaveFailed(true);
          return false;
        }
      });
      mpdWriteChainRef.current = run.then(
        () => undefined,
        () => undefined,
      );
      return run;
    },
    [],
  );

  const handleToggleMpd = useCallback(() => {
    // Switching on is when the server starts answering the whole network,
    // so that is when a missing password is pointed out again.
    if (!mpdConfig.enabled) setMpdSuggestDismissed(false);
    persistMpd({ ...mpdConfig, enabled: !mpdConfig.enabled });
  }, [mpdConfig, persistMpd]);

  // Saved first, through the same path as a typed password, and put in the
  // field only once the backend confirmed it. The suggestion it came from
  // unmounts with the password set, so focus moves to the field that now
  // holds it.
  const handleGenerateMpdPassword = useCallback(async () => {
    setMpdGenerating(true);
    let saved: boolean;
    try {
      saved = await persistMpd(
        { ...mpdConfig, password: generatePassword() },
        { optimistic: false },
      );
    } finally {
      // Rendered now rather than batched: the field is disabled while a
      // password is generated, and a disabled field takes no focus.
      flushSync(() => setMpdGenerating(false));
    }
    if (!saved) return;
    setMpdPasswordVisible(true);
    setMpdPasswordGenerated(true);
    mpdPasswordInputRef.current?.focus();
  }, [mpdConfig, persistMpd]);

  const handleMpdCopyAddress = useCallback(() => {
    if (!mpdStatus?.bound_address) return;
    navigator.clipboard
      .writeText(mpdStatus.bound_address)
      .then(() => {
        setMpdAddressCopied(true);
        setTimeout(() => setMpdAddressCopied(false), 1500);
      })
      .catch((err) => console.error("[SettingsView] mpd copy", err));
  }, [mpdStatus]);

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

  // Audio settings live in `profile_setting`, so they have to be
  // re-read when the active profile changes — otherwise the panel keeps
  // showing the previous profile's values and the next toggle writes
  // them into the new one. The `stale` latch drops an in-flight answer
  // that belongs to the profile we just left.
  useEffect(() => {
    let stale = false;
    audioSettingsTouched.current = new Set();
    playerGetAudioSettings()
      .then((s) => {
        if (stale) return;
        setNormalize(s.normalize);
        setMono(s.mono);
        if (!audioSettingsTouched.current.has("pauseOnDeviceLoss")) {
          setPauseOnDeviceLoss(s.pause_on_device_loss);
        }
        if (!audioSettingsTouched.current.has("keepAwake")) {
          setKeepAwake(s.keep_awake);
        }
        if (!audioSettingsTouched.current.has("matchSourceRate")) {
          setMatchSourceRate(s.match_source_rate);
        }
        setCrossfadeSec(Math.round(s.crossfade_ms / 1000));
        setReplayGain(s.replaygain);
        setReplayGainPreamp(s.replaygain_preamp_db);
        setReplayGainFallback(s.replaygain_fallback_db);
        setReplayGainPreventClipping(s.replaygain_prevent_clipping);
        if (!audioSettingsTouched.current.has("replayGainMode")) {
          setReplayGainMode(s.replaygain_mode);
        }
        setGapless(s.gapless);
        // Guard against a stale / out-of-set value from the backend.
        setDsdTaps(
          (DSD_PRECISION_TAPS as readonly number[]).includes(s.dsd_taps)
            ? (s.dsd_taps as DsdPrecisionTaps)
            : SETTINGS["audio.dsd_precision"].defaultValue,
        );
        setDsdDop(s.dsd_dop);
      })
      .catch((err) =>
        console.error("[Settings] audio settings load failed", err),
      );
    return () => {
      stale = true;
    };
  }, [activeProfile?.id, audioSettingsNonce]);

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

  // The sliders fire on every pixel of drag. The audio side is a pair
  // of atomics and would happily take all of them, but each push also
  // writes three rows to SQLite — so the round trip is debounced and
  // local state drives the UI in the meantime.
  const pushReplayGainOptions = useCallback(
    (preampDb: number, fallbackDb: number, preventClipping: boolean) => {
      if (replayGainOptionsDebounce.current != null) {
        window.clearTimeout(replayGainOptionsDebounce.current);
      }
      replayGainOptionsDebounce.current = window.setTimeout(() => {
        playerSetReplayGainOptions({
          preampDb,
          fallbackDb,
          preventClipping,
        }).catch((err) => {
          console.error("[Settings] set replaygain options failed", err);
          // Not saved, so not applied either: show what the engine
          // still holds instead of the slider's position.
          setAudioSettingsNonce((n) => n + 1);
        });
      }, 250);
    },
    [],
  );

  useEffect(
    () => () => {
      if (replayGainOptionsDebounce.current != null) {
        window.clearTimeout(replayGainOptionsDebounce.current);
      }
    },
    [],
  );

  const handleReplayGainPreampChange = useCallback(
    (value: number) => {
      setReplayGainPreamp(value);
      pushReplayGainOptions(
        value,
        replayGainFallback,
        replayGainPreventClipping,
      );
    },
    [pushReplayGainOptions, replayGainFallback, replayGainPreventClipping],
  );

  const handleReplayGainFallbackChange = useCallback(
    (value: number) => {
      setReplayGainFallback(value);
      pushReplayGainOptions(replayGainPreamp, value, replayGainPreventClipping);
    },
    [pushReplayGainOptions, replayGainPreamp, replayGainPreventClipping],
  );

  // Which profile is live, readable from inside a callback that was
  // scheduled under a different one. Declared here rather than beside
  // its first user further down: every handler below captures the id
  // at click time and compares against this, and `react-hooks`
  // reasons about the whole component — a ref first referenced above
  // its own declaration makes the rule treat it as frozen.
  const activeProfileIdRef = useRef(activeProfile?.id);
  useEffect(() => {
    activeProfileIdRef.current = activeProfile?.id;
  }, [activeProfile?.id]);

  const handleReplayGainModeChange = useCallback(
    (next: ReplayGainMode) => {
      const previous = replayGainMode;
      // Captured at click time: a profile switch while the write is in
      // flight must not let this land on, or roll back into, someone
      // else's settings. Same guard as the toggles below.
      const profileId = activeProfile?.id;
      // Before the write, so a hydration response still in flight
      // cannot land on top of a choice the user has already made.
      audioSettingsTouched.current.add("replayGainMode");
      setReplayGainMode(next);
      playerSetReplayGainMode(next).catch((err) => {
        console.error("[Settings] set replaygain mode failed", err);
        if (activeProfileIdRef.current !== profileId) return;
        // Only roll back if no newer selection superseded this one —
        // a stale failure must not clobber a later successful click.
        // Same rule as `handleSetDsdPrecision`.
        setReplayGainMode((cur) => (cur === next ? previous : cur));
      });
    },
    [replayGainMode, activeProfile?.id],
  );

  const handleToggleReplayGainPreventClipping = useCallback(() => {
    const next = !replayGainPreventClipping;
    setReplayGainPreventClipping(next);
    pushReplayGainOptions(replayGainPreamp, replayGainFallback, next);
  }, [
    pushReplayGainOptions,
    replayGainPreamp,
    replayGainFallback,
    replayGainPreventClipping,
  ]);

  const handleSetDsdPrecision = useCallback(
    (next: DsdPrecisionTaps) => {
      const prev = dsdTaps;
      if (prev === next) return;
      setDsdTaps(next); // optimistic
      playerSetDsdPrecision(next).catch((err) => {
        console.error("[Settings] set DSD precision failed", err);
        // Only roll back if no newer selection superseded this one —
        // a stale failure must not clobber a later successful click.
        setDsdTaps((cur) => (cur === next ? prev : cur));
      });
    },
    [dsdTaps],
  );

  // Writes are chained rather than fired in parallel: two fast clicks
  // would otherwise race in the backend and could leave the persisted
  // setting on the *first* value. The rollback is likewise conditional,
  // so a stale failure can't clobber a newer click.
  //
  // The queue outlives a profile switch, so each entry also carries the
  // profile it was scheduled under: `audio.dsd_dop` is profile-scoped,
  // and a queued write landing after the switch would put profile A's
  // value into profile B — as would its rollback.
  const dsdDopWrite = useRef<Promise<void>>(Promise.resolve());
  const handleToggleDsdDop = useCallback(() => {
    const prev = dsdDop;
    const next = !prev;
    const profileId = activeProfile?.id;
    setDsdDop(next); // optimistic
    dsdDopWrite.current = dsdDopWrite.current
      .catch(() => {})
      .then(() => {
        if (activeProfileIdRef.current !== profileId) return;
        return playerSetDsdDop(next);
      })
      .catch((err) => {
        console.error("[Settings] set DSD DoP failed", err);
        if (activeProfileIdRef.current !== profileId) return;
        setDsdDop((cur) => (cur === next ? prev : cur));
      });
  }, [dsdDop, activeProfile?.id]);

  // Smart crossfade — skip the fade between two tracks of the same
  // album so concept records / live sets hand off naturally. Persisted
  // backend-side; default OFF (opinionated behaviour, opt-in).
  // Three engine toggles with the same per-profile lifecycle — read on
  // the active profile, disabled until it has answered, rolled back
  // only while it is still active (#698).
  const {
    enabled: smartCrossfade,
    hydrated: smartCrossfadeHydrated,
    toggle: handleToggleSmartCrossfade,
  } = useProfileScopedToggle(getSmartCrossfade, setSmartCrossfade, {
    label: "SettingsView smart crossfade",
    fallback: SETTINGS["audio.smart_crossfade"].defaultValue,
  });

  const {
    enabled: dynamicCrossfade,
    hydrated: dynamicCrossfadeHydrated,
    toggle: handleToggleDynamicCrossfade,
  } = useProfileScopedToggle(getDynamicCrossfade, setDynamicCrossfade, {
    label: "SettingsView dynamic crossfade",
    fallback: SETTINGS["audio.dynamic_crossfade"].defaultValue,
  });

  // Persisted backend-side and pushed live to the decoder thread, so
  // flipping it shows / hides the bars on the next emitted frame.
  const {
    enabled: visualizer,
    hydrated: visualizerHydrated,
    toggle: handleToggleVisualizer,
  } = useProfileScopedToggle(getVisualizerEnabled, setVisualizerEnabled, {
    label: "SettingsView visualizer",
    fallback: SETTINGS["ui.visualizer"].defaultValue,
  });

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

  // Both of these are profile-scoped settings written by a backend
  // command, so they take the same discipline `handleToggleDsdDop`
  // established below: writes chained rather than fired in parallel, so
  // two fast clicks cannot leave the persisted value on the *first* one,
  // and a conditional rollback so a stale failure cannot clobber a newer
  // click. Each entry carries the profile it was scheduled under — the
  // queue outlives a switch, and a late write (or its rollback) would
  // otherwise put profile A's value into profile B.
  const matchSourceRateWrite = useRef<Promise<void>>(Promise.resolve());
  const handleToggleMatchSourceRate = useCallback(() => {
    const prev = matchSourceRate;
    const next = !prev;
    const profileId = activeProfile?.id;
    audioSettingsTouched.current.add("matchSourceRate");
    setMatchSourceRate(next);
    matchSourceRateWrite.current = matchSourceRateWrite.current
      .catch(() => {})
      .then(() => {
        if (activeProfileIdRef.current !== profileId) return;
        return playerSetMatchSourceRate(next);
      })
      .catch((err) => {
        console.error("[Settings] set match source rate failed", err);
        if (activeProfileIdRef.current !== profileId) return;
        setMatchSourceRate((cur) => (cur === next ? prev : cur));
        // …and then ask the backend what it really holds: `prev` is the
        // value this click replaced, which after a failed write may
        // never have been persisted either.
        setAudioSettingsNonce((n) => n + 1);
      });
  }, [matchSourceRate, activeProfile?.id]);

  const pauseOnDeviceLossWrite = useRef<Promise<void>>(Promise.resolve());
  const handleTogglePauseOnDeviceLoss = useCallback(() => {
    const prev = pauseOnDeviceLoss;
    const next = !prev;
    const profileId = activeProfile?.id;
    audioSettingsTouched.current.add("pauseOnDeviceLoss");
    setPauseOnDeviceLoss(next);
    pauseOnDeviceLossWrite.current = pauseOnDeviceLossWrite.current
      .catch(() => {})
      .then(() => {
        if (activeProfileIdRef.current !== profileId) return;
        return playerSetPauseOnDeviceLoss(next);
      })
      .catch((err) => {
        console.error("[Settings] set pause on device loss failed", err);
        if (activeProfileIdRef.current !== profileId) return;
        setPauseOnDeviceLoss((cur) => (cur === next ? prev : cur));
        setAudioSettingsNonce((n) => n + 1);
      });
  }, [pauseOnDeviceLoss, activeProfile?.id]);

  // Same write discipline as the toggle above: serialized, dropped on a
  // profile switch, rolled back on failure.
  const keepAwakeWrite = useRef<Promise<void>>(Promise.resolve());
  const handleToggleKeepAwake = useCallback(() => {
    const prev = keepAwake;
    const next = !prev;
    const profileId = activeProfile?.id;
    audioSettingsTouched.current.add("keepAwake");
    setKeepAwake(next);
    keepAwakeWrite.current = keepAwakeWrite.current
      .catch(() => {})
      .then(() => {
        if (activeProfileIdRef.current !== profileId) return;
        return playerSetKeepAwake(next, profileId ?? null);
      })
      .catch((err) => {
        console.error("[Settings] set keep awake failed", err);
        if (activeProfileIdRef.current !== profileId) return;
        setKeepAwake((cur) => (cur === next ? prev : cur));
        setAudioSettingsNonce((n) => n + 1);
      });
  }, [keepAwake, activeProfile?.id]);

  // Debounce crossfade slider changes to avoid spamming the backend.
  const crossfadeTimerRef = useRef<number | null>(null);
  const handleCrossfadeChange = useCallback((sec: number) => {
    setCrossfadeSec(sec);
    if (crossfadeTimerRef.current != null) {
      window.clearTimeout(crossfadeTimerRef.current);
    }
    crossfadeTimerRef.current = window.setTimeout(() => {
      playerSetCrossfade(sec).catch((err) => {
        console.error("[Settings] set crossfade failed", err);
        // Same as the ReplayGain sliders: re-read what is in force.
        setAudioSettingsNonce((n) => n + 1);
      });
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

  // Bring the page or group a navigation asked for into view, once it is
  // on screen.
  const revealTarget = (group: string | null) => {
    const target = group
      ? (document.getElementById(`settings-group-${group}`) ?? panelRef.current)
      : panelRef.current;
    if (target instanceof HTMLDetailsElement) target.open = true;
    // A category goes back to the top of the page itself, not
    // to `.settings-view` with its scroll margin: under a skin
    // whose page padding exceeds that margin (Studio's 2rem
    // against 1.5rem), the page scrolled the difference — but
    // only for a category long enough to scroll, so switching
    // between a short and a long one jumped by 8 px (#884).
    const page = pageScroll?.current;
    // In a narrow window the navigation sits above the panel,
    // and the top of the page would show the list just clicked
    // with the chosen category below the fold: scroll to the
    // panel there instead.
    const panel = panelRef.current;
    const nav = panel?.previousElementSibling;
    const stacked =
      panel != null &&
      nav != null &&
      panel.getBoundingClientRect().top >= nav.getBoundingClientRect().bottom;
    if (group) {
      target?.scrollIntoView({ block: "start" });
    } else if (stacked) {
      panel.scrollIntoView({ block: "start" });
    } else if (page) {
      page.scrollTo({ top: 0 });
    } else {
      panelRef.current
        ?.closest(".settings-view")
        ?.scrollIntoView({ block: "start" });
    }
    target?.focus({ preventScroll: true });
  };

  // Every page's chunk, fetched right after Settings mounts.
  useEffect(() => {
    const handle = window.setTimeout(preloadSettingsSections, 0);
    return () => window.clearTimeout(handle);
  }, []);

  return (
    <div className="settings-view @container max-w-6xl mx-auto space-y-8 animate-fade-in pb-20 scroll-mt-6">
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

      <div className="grid items-start gap-8 @4xl:grid-cols-[15rem_minmax(0,1fr)] @4xl:gap-10">
        <SettingsNavigation
          activeCategory={activeCategory}
          onSelect={(category, group) => {
            handleCategoryChange(category);
            setTargetGroup(group ?? null);
            setRevealRequest((prev) => ({
              group: group ?? null,
              seq: (prev?.seq ?? 0) + 1,
            }));
          }}
        />
        <div
          ref={panelRef}
          tabIndex={-1}
          className="settings-content min-w-0 scroll-mt-6 outline-none"
        >
          {/* A page that fails leaves the rest of Settings usable. */}
          <ViewErrorBoundary resetKey={activeCategory} size="sm">
            <Suspense fallback={<SectionFallback />}>
              {activeCategory === "general" && (
                <GeneralSection
                  autoStart={autoStart}
                  autoplay={autoplay}
                  autoplayResolved={autoplayResolved}
                  generatorAlbumMode={generatorAlbumMode}
                  generatorAlbumModeResolved={generatorAlbumModeResolved}
                  handleLanguageChange={handleLanguageChange}
                  handleToggleAutoStart={handleToggleAutoStart}
                  handleToggleKeepAwake={handleToggleKeepAwake}
                  handleToggleMinimizeToTray={handleToggleMinimizeToTray}
                  handleToggleSingleClickPlay={handleToggleSingleClickPlay}
                  handleToggleTrackChangeNotif={handleToggleTrackChangeNotif}
                  keepAwake={keepAwake}
                  minimizeToTray={minimizeToTray}
                  setAutoplay={setAutoplay}
                  setGeneratorAlbumMode={setGeneratorAlbumMode}
                  singleClickPlay={singleClickPlay}
                  targetGroup={targetGroup}
                  trackChangeNotif={trackChangeNotif}
                />
              )}
              {activeCategory === "appearance" && (
                <AppearanceSection
                  handleToggleVisualizer={handleToggleVisualizer}
                  handleZoomDelta={handleZoomDelta}
                  handleZoomReset={handleZoomReset}
                  targetGroup={targetGroup}
                  uiZoom={uiZoom}
                  visualizer={visualizer}
                  visualizerHydrated={visualizerHydrated}
                />
              )}
              {activeCategory === "playback" && (
                <AudioSection
                  crossfadeSec={crossfadeSec}
                  dopPlatformSupported={dopPlatformSupported}
                  dsdDop={dsdDop}
                  dsdTaps={dsdTaps}
                  dynamicCrossfade={dynamicCrossfade}
                  dynamicCrossfadeHydrated={dynamicCrossfadeHydrated}
                  gapless={gapless}
                  handleCrossfadeChange={handleCrossfadeChange}
                  handleReplayGainFallbackChange={
                    handleReplayGainFallbackChange
                  }
                  handleReplayGainModeChange={handleReplayGainModeChange}
                  handleReplayGainPreampChange={handleReplayGainPreampChange}
                  handleSetDsdPrecision={handleSetDsdPrecision}
                  handleToggleDsdDop={handleToggleDsdDop}
                  handleToggleDynamicCrossfade={handleToggleDynamicCrossfade}
                  handleToggleGapless={handleToggleGapless}
                  handleToggleMatchSourceRate={handleToggleMatchSourceRate}
                  handleToggleMono={handleToggleMono}
                  handleToggleNormalize={handleToggleNormalize}
                  handleTogglePauseOnDeviceLoss={handleTogglePauseOnDeviceLoss}
                  handleToggleReplayGain={handleToggleReplayGain}
                  handleToggleReplayGainPreventClipping={
                    handleToggleReplayGainPreventClipping
                  }
                  handleToggleSmartCrossfade={handleToggleSmartCrossfade}
                  matchSourceRate={matchSourceRate}
                  mono={mono}
                  normalize={normalize}
                  pauseOnDeviceLoss={pauseOnDeviceLoss}
                  replayGain={replayGain}
                  replayGainFallback={replayGainFallback}
                  replayGainMode={replayGainMode}
                  replayGainPreamp={replayGainPreamp}
                  replayGainPreventClipping={replayGainPreventClipping}
                  smartCrossfade={smartCrossfade}
                  smartCrossfadeHydrated={smartCrossfadeHydrated}
                  targetGroup={targetGroup}
                />
              )}
              {activeCategory === "media" && (
                <LyricsSection
                  handleCancelPrefetchLyrics={handleCancelPrefetchLyrics}
                  handlePreferLrclibToggle={handlePreferLrclibToggle}
                  handlePrefetchLyrics={handlePrefetchLyrics}
                  handleTranslationLangChange={handleTranslationLangChange}
                  isPrefetchingLyrics={isPrefetchingLyrics}
                  lyricsPrefetchProgress={lyricsPrefetchProgress}
                  lyricsResultMsg={lyricsResultMsg}
                  preferLrclib={preferLrclib}
                  preferLrclibBusy={preferLrclibBusy}
                  targetGroup={targetGroup}
                  translationLang={translationLang}
                  translationLangBusy={translationLangBusy}
                />
              )}
              {activeCategory === "library" && (
                <LibrarySection
                  analysisTask={analysisTask}
                  analyzeFailed={analyzeFailed}
                  artistFetchProgress={artistFetchProgress}
                  autoAnalyze={autoAnalyze}
                  coverProgress={coverProgress}
                  coverResultMsg={coverResultMsg}
                  handleAnalyzeLibrary={handleAnalyzeLibrary}
                  handleCancelAnalyzeLibrary={handleCancelAnalyzeLibrary}
                  handleFetchMissingArtistPictures={
                    handleFetchMissingArtistPictures
                  }
                  handleFetchMissingCovers={handleFetchMissingCovers}
                  handleRescan={handleRescan}
                  handleRescanLocalArtistImages={handleRescanLocalArtistImages}
                  handleToggleAutoAnalyze={handleToggleAutoAnalyze}
                  handleToggleIgnoreArticles={handleToggleIgnoreArticles}
                  handleToggleScanOnStart={handleToggleScanOnStart}
                  ignoreArticles={ignoreArticles}
                  isAnalyzingLib={isAnalyzingLib}
                  isFetchingArtists={isFetchingArtists}
                  isFetchingCovers={isFetchingCovers}
                  isRescanning={isRescanning}
                  isRescanningLocalArtists={isRescanningLocalArtists}
                  libraries={libraries}
                  localArtistRescanStatus={localArtistRescanStatus}
                  savingIgnoreArticles={savingIgnoreArticles}
                  scanOnStart={scanOnStart}
                  setIsDuplicatesOpen={setIsDuplicatesOpen}
                  targetGroup={targetGroup}
                />
              )}
              {activeCategory === "integrations" && (
                <IntegrationsSection
                  activeProfile={activeProfile}
                  discordRpc={discordRpc}
                  dlnaConfig={dlnaConfig}
                  dlnaStatus={dlnaStatus}
                  dlnaUrlCopied={dlnaUrlCopied}
                  handleDlnaCopyUrl={handleDlnaCopyUrl}
                  handleGenerateMpdPassword={handleGenerateMpdPassword}
                  handleLastfmLogin={handleLastfmLogin}
                  handleLastfmLogout={handleLastfmLogout}
                  handleMpdCopyAddress={handleMpdCopyAddress}
                  handleSaveLastfmKey={handleSaveLastfmKey}
                  handleToggleDiscordRpc={handleToggleDiscordRpc}
                  handleToggleDlna={handleToggleDlna}
                  handleToggleMpd={handleToggleMpd}
                  handleToggleOfflineMode={handleToggleOfflineMode}
                  lastfmKey={lastfmKey}
                  lastfmKeyVisible={lastfmKeyVisible}
                  lastfmLoggingIn={lastfmLoggingIn}
                  lastfmLoginError={lastfmLoginError}
                  lastfmPassword={lastfmPassword}
                  lastfmSaved={lastfmSaved}
                  lastfmSaving={lastfmSaving}
                  lastfmSecret={lastfmSecret}
                  lastfmSecretVisible={lastfmSecretVisible}
                  lastfmStatus={lastfmStatus}
                  lastfmUsername={lastfmUsername}
                  mpdAddressCopied={mpdAddressCopied}
                  mpdConfig={mpdConfig}
                  mpdConfirmedRef={mpdConfirmedRef}
                  mpdGenerating={mpdGenerating}
                  mpdHydrated={mpdHydrated}
                  mpdPasswordGenerated={mpdPasswordGenerated}
                  mpdPasswordInputRef={mpdPasswordInputRef}
                  mpdPasswordVisible={mpdPasswordVisible}
                  mpdSaveFailed={mpdSaveFailed}
                  mpdStatus={mpdStatus}
                  mpdSuggestDismissed={mpdSuggestDismissed}
                  offlineMode={offlineMode}
                  persistDlna={persistDlna}
                  persistMpd={persistMpd}
                  setDlnaConfig={setDlnaConfig}
                  setLastfmKey={setLastfmKey}
                  setLastfmKeyVisible={setLastfmKeyVisible}
                  setLastfmLoginError={setLastfmLoginError}
                  setLastfmPassword={setLastfmPassword}
                  setLastfmSaved={setLastfmSaved}
                  setLastfmSecret={setLastfmSecret}
                  setLastfmSecretVisible={setLastfmSecretVisible}
                  setLastfmUsername={setLastfmUsername}
                  setMpdConfig={setMpdConfig}
                  setMpdPasswordGenerated={setMpdPasswordGenerated}
                  setMpdPasswordVisible={setMpdPasswordVisible}
                  setMpdSuggestDismissed={setMpdSuggestDismissed}
                  targetGroup={targetGroup}
                />
              )}
              {activeCategory === "shortcuts" && (
                <ShortcutsSection targetGroup={targetGroup} />
              )}
              {activeCategory === "data" && (
                <DataSection
                  activeProfile={activeProfile}
                  handleExportProfile={handleExportProfile}
                  handleImportProfile={handleImportProfile}
                  handleOpenDataFolder={handleOpenDataFolder}
                  handlePruneCachedCovers={handlePruneCachedCovers}
                  handleRegenerateThumbnails={handleRegenerateThumbnails}
                  isPruningCovers={isPruningCovers}
                  isRegeneratingThumbs={isRegeneratingThumbs}
                  profileIoBusy={profileIoBusy}
                  profileIoStatus={profileIoStatus}
                  pruneCoversStatus={pruneCoversStatus}
                  targetGroup={targetGroup}
                  thumbsStatus={thumbsStatus}
                />
              )}
              {activeCategory === "diagnostics" && (
                <DiagnosticsSection
                  copyLogsStatus={copyLogsStatus}
                  handleCopyLogs={handleCopyLogs}
                  handleOpenLogFolder={handleOpenLogFolder}
                  setIsResetOpen={setIsResetOpen}
                  targetGroup={targetGroup}
                />
              )}
              <RevealOnMount request={revealRequest} onReveal={revealTarget} />
            </Suspense>
          </ViewErrorBoundary>
        </div>
      </div>

      <DuplicatesModal
        isOpen={isDuplicatesOpen}
        onClose={() => setIsDuplicatesOpen(false)}
      />
      <ResetAppModal
        isOpen={isResetOpen}
        onClose={() => setIsResetOpen(false)}
      />
      <ProfileArchivePreviewModal
        state={archivePreview}
        onCancel={handleCancelArchivePreview}
        onImport={() => void handleConfirmImportProfile()}
      />
    </div>
  );
}
