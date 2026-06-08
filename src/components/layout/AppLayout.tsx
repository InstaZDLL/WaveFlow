import {
  lazy,
  Suspense,
  useState,
  useCallback,
  useEffect,
  useRef,
} from "react";
import type { ViewId, LibraryTab } from "../../types";
import { useLibrary } from "../../hooks/useLibrary";
import { useProfile } from "../../hooks/useProfile";
import { usePlayer } from "../../hooks/usePlayer";
import { getProfileSetting, setProfileSetting } from "../../lib/tauri/profile";
import { Sidebar } from "./Sidebar";
import { useDragDropImport } from "../../hooks/useDragDropImport";
import { useGlobalShortcuts } from "../../hooks/useGlobalShortcuts";
import { useUiZoom } from "../../hooks/useUiZoom";
import { useTranslation } from "react-i18next";
import { Loader2, Upload } from "lucide-react";
import { AnimatePresence, motion } from "framer-motion";
import { TopBar } from "./TopBar";
import { QueuePanel } from "./QueuePanel";
import { NowPlayingPanel } from "./NowPlayingPanel";
import { LyricsPanel } from "./LyricsPanel";
import { NowPlayingChevronTab } from "./NowPlayingChevronTab";
import { DeviceMenu } from "./DeviceMenu";
import { PlayerBar } from "../player/PlayerBar";
import { ProfileSelectorModal } from "../common/ProfileSelectorModal";
import { LastfmReauthBanner } from "../common/LastfmReauthBanner";
import { UpdateBanner } from "../common/UpdateBanner";
import { ScanProgressToast } from "../common/ScanProgressToast";
import { OnboardingModal } from "../common/OnboardingModal";
import { ViewSuspenseFallback } from "../common/ViewSuspenseFallback";
import { PageScrollContext } from "../../contexts/PageScrollContext";

const HomeView = lazy(() =>
  import("../views/HomeView").then((module) => ({ default: module.HomeView })),
);
const LibraryView = lazy(() =>
  import("../views/LibraryView").then((module) => ({
    default: module.LibraryView,
  })),
);
const SettingsView = lazy(() =>
  import("../views/SettingsView").then((module) => ({
    default: module.SettingsView,
  })),
);
const SpotifyView = lazy(() =>
  import("../views/SpotifyView").then((module) => ({
    default: module.SpotifyView,
  })),
);
const AboutView = lazy(() =>
  import("../views/AboutView").then((module) => ({
    default: module.AboutView,
  })),
);
const FeedbackView = lazy(() =>
  import("../views/FeedbackView").then((module) => ({
    default: module.FeedbackView,
  })),
);
const StatisticsView = lazy(() =>
  import("../views/StatisticsView").then((module) => ({
    default: module.StatisticsView,
  })),
);
const WrappedView = lazy(() =>
  import("../views/WrappedView").then((module) => ({
    default: module.WrappedView,
  })),
);
const LikedView = lazy(() =>
  import("../views/LikedView").then((module) => ({
    default: module.LikedView,
  })),
);
const HistoryView = lazy(() =>
  import("../views/HistoryView").then((module) => ({
    default: module.HistoryView,
  })),
);
const PlaylistView = lazy(() =>
  import("../views/PlaylistView").then((module) => ({
    default: module.PlaylistView,
  })),
);
const AlbumDetailView = lazy(() =>
  import("../views/AlbumDetailView").then((module) => ({
    default: module.AlbumDetailView,
  })),
);
const ArtistDetailView = lazy(() =>
  import("../views/ArtistDetailView").then((module) => ({
    default: module.ArtistDetailView,
  })),
);
const GenreDetailView = lazy(() =>
  import("../views/GenreDetailView").then((module) => ({
    default: module.GenreDetailView,
  })),
);

// Each entry in the navigation history pairs a view id with its payload
// (when relevant) so back/forward can restore the exact target the user
// visited. Payload fields are optional so callers without a target (e.g.
// the initial "home" entry, or navigating to "wrapped" without a year)
// stay valid.
type HistoryEntry =
  | { id: "home" }
  | { id: "library" }
  | { id: "settings" }
  | { id: "spotify" }
  | { id: "about" }
  | { id: "feedback" }
  | { id: "statistics" }
  | { id: "liked" }
  | { id: "recent" }
  | { id: "wrapped"; year?: number | null }
  | { id: "playlist"; playlistId?: number | null }
  | { id: "album-detail"; albumId?: number | null }
  | { id: "artist-detail"; artistId?: number | null }
  | { id: "genre-detail"; genreId?: number | null };

export function AppLayout() {
  const { t } = useTranslation();
  const { activeRightPanel } = usePlayer();
  const dragDrop = useDragDropImport();
  // Global keyboard shortcuts. The hook itself attaches the keydown
  // listener and re-reads bindings whenever Settings emits the
  // shortcuts-changed event.
  useGlobalShortcuts();
  // UI zoom: hydrate the persisted level on boot, apply through
  // Tauri's WebView `setZoom`, and listen for Ctrl+= / Ctrl+- /
  // Ctrl+0 so users can tune density without diving into Settings.
  useUiZoom();
  // History entries carry their payload (album/artist/genre/playlist id,
  // wrapped year) directly so back/forward restore the exact target the
  // user visited — not whatever target was set most recently. Without
  // this, navigating album A → home → album B → back → back lands on
  // "album-detail" with activeAlbumId still pointing at B.
  //
  // History + index live in a single state object so push/replace can
  // update both atomically inside one functional setter. Splitting them
  // would let rapid back-to-back navigations queue setters that all read
  // the same stale index, losing entries and leaving `index` past
  // `history.length - 1`.
  const [navState, setNavState] = useState<{
    history: HistoryEntry[];
    index: number;
  }>({ history: [{ id: "home" }], index: 0 });
  const viewHistory = navState.history;
  const historyIndex = navState.index;
  const [isProfileModalOpen, setIsProfileModalOpen] = useState(false);
  const [libraryTab, setLibraryTab] = useState<LibraryTab>("morceaux");

  // Sidebar visibility toggle (#167). Hidden state is persisted in
  // localStorage rather than `profile_setting` because it's a UI
  // affordance — not data — and users on 1080p screens typically
  // hide once and forget. The Sidebar component stays mounted at
  // `width: 0` so its data fetches / event listeners don't tear
  // down on every toggle. Hydration happens lazily so the initial
  // render matches the persisted choice without a flash.
  const [isSidebarHidden, setIsSidebarHidden] = useState<boolean>(() => {
    try {
      return localStorage.getItem("waveflow.sidebar.hidden") === "1";
    } catch {
      return false;
    }
  });
  const toggleSidebar = useCallback(() => {
    setIsSidebarHidden((prev) => {
      const next = !prev;
      try {
        localStorage.setItem("waveflow.sidebar.hidden", next ? "1" : "0");
      } catch {
        // Private mode / quota — UI still works for this session.
      }
      return next;
    });
  }, []);

  // First-run onboarding: prompt the user to point WaveFlow at a
  // music folder when no library has been populated yet.
  //
  // The decision is **latched once per profile** at the end of the
  // initial fetch and persisted across sessions via the
  // `onboarding.dismissed` profile setting. Concretely:
  //   1. wait for ProfileProvider + LibraryProvider to settle their
  //      first fetch with a non-null `activeProfile`;
  //   2. read `profile_setting['onboarding.dismissed']`. If `true`,
  //      the user has already said "configure later" or completed
  //      the flow on a previous launch — never bother them again
  //      for this profile;
  //   3. otherwise, show the modal iff the library is empty.
  //
  // Why a latched state and not a memo: the loading flags
  // transition through several intermediate values during boot, and
  // a memo recomputed on every render would briefly satisfy "show"
  // mid-boot before flipping back — that's the flash the modal
  // used to do.
  const {
    libraries,
    isLoading: isLibraryLoading,
    loadedProfileId: librariesLoadedFor,
  } = useLibrary();
  const { activeProfile, isLoading: isProfileLoading } = useProfile();
  const [showOnboarding, setShowOnboarding] = useState(false);
  // Tracks the active profile id we've already evaluated against, so
  // a profile switch re-runs the gate exactly once.
  const evaluatedProfileId = useRef<number | null>(null);
  // Ref handed down to virtualized tables so they share the page-level
  // scroller instead of nesting their own.
  const pageScrollRef = useRef<HTMLDivElement>(null);

  // Idle-time chunk warm-up for every lazily-loaded view. Once these
  // imports resolve they're cached in the module registry, so a later
  // navigation hits the Suspense fallback for zero ms instead of
  // pausing for the network round-trip on the first click.
  useEffect(() => {
    const warmup = () => {
      void import("../views/LibraryView");
      void import("../views/LikedView");
      void import("../views/HistoryView");
      void import("../views/PlaylistView");
      void import("../views/AlbumDetailView");
      void import("../views/ArtistDetailView");
      void import("../views/GenreDetailView");
      void import("../views/StatisticsView");
      void import("../views/WrappedView");
      void import("../views/SettingsView");
      void import("../views/SpotifyView");
      void import("../views/AboutView");
      void import("../views/FeedbackView");
    };
    type IdleWindow = Window & {
      requestIdleCallback?: (
        cb: () => void,
        opts?: { timeout?: number },
      ) => number;
      cancelIdleCallback?: (handle: number) => void;
    };
    const w = window as IdleWindow;
    if (typeof w.requestIdleCallback === "function") {
      const handle = w.requestIdleCallback(warmup, { timeout: 2000 });
      return () => w.cancelIdleCallback?.(handle);
    }
    const timer = setTimeout(warmup, 300);
    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    if (isProfileLoading || isLibraryLoading) return;
    if (!activeProfile) return;
    // Defer until LibraryContext has refetched FOR this profile. Without
    // this check the gate would evaluate with the previous profile's
    // libraries during a switch — a fresh profile would silently skip
    // onboarding if the previous one happened to have folders.
    if (librariesLoadedFor !== activeProfile.id) return;
    if (evaluatedProfileId.current === activeProfile.id) return;

    const profileId = activeProfile.id;
    evaluatedProfileId.current = profileId;

    let cancelled = false;
    (async () => {
      let dismissed = false;
      try {
        const raw = await getProfileSetting("onboarding.dismissed");
        dismissed = raw === "true";
      } catch (err) {
        // Read failure is non-fatal — fall back to "not dismissed"
        // so a brand new profile still gets the prompt.
        console.error("[AppLayout] read onboarding.dismissed failed", err);
      }
      if (cancelled || evaluatedProfileId.current !== profileId) return;
      if (dismissed) {
        setShowOnboarding(false);
        return;
      }
      const isEmpty =
        libraries.length === 0 || libraries.every((l) => l.folder_count === 0);
      setShowOnboarding(isEmpty);
    })();

    return () => {
      cancelled = true;
    };
  }, [
    activeProfile,
    isProfileLoading,
    isLibraryLoading,
    librariesLoadedFor,
    libraries,
  ]);

  const dismissOnboarding = useCallback(() => {
    // Persist the choice so the modal doesn't reappear on next
    // launch — the user already told us to leave them alone, even
    // if their library is still empty.
    setShowOnboarding(false);
    setProfileSetting("onboarding.dismissed", "true", "bool").catch((err) =>
      console.error("[AppLayout] persist onboarding.dismissed failed", err),
    );
  }, []);

  const currentEntry = viewHistory[historyIndex];
  const activeView: ViewId = currentEntry.id;
  // Derived from the current history entry so back/forward restore the
  // correct payload. `null` for views without a payload.
  const activeAlbumId =
    currentEntry.id === "album-detail" ? (currentEntry.albumId ?? null) : null;
  const activeArtistId =
    currentEntry.id === "artist-detail"
      ? (currentEntry.artistId ?? null)
      : null;
  const activeGenreId =
    currentEntry.id === "genre-detail" ? (currentEntry.genreId ?? null) : null;
  const activePlaylistId =
    currentEntry.id === "playlist" ? (currentEntry.playlistId ?? null) : null;
  const activeWrappedYear =
    currentEntry.id === "wrapped" ? (currentEntry.year ?? null) : null;

  const pushEntry = useCallback((entry: HistoryEntry) => {
    setNavState(({ history, index }) => ({
      history: [...history.slice(0, index + 1), entry],
      index: index + 1,
    }));
  }, []);

  // Replace the current entry in place (no index bump). Used when the
  // current target no longer exists — e.g. a playlist that was just
  // deleted — so Back doesn't return to a ghost page.
  const replaceEntry = useCallback((entry: HistoryEntry) => {
    setNavState(({ history, index }) => {
      const next = [...history];
      next[index] = entry;
      return { history: next, index };
    });
  }, []);

  // Wrapper used by views that only need a plain id (Home, Settings, …).
  // The cast is safe because every `ViewId` matches a HistoryEntry whose
  // payload fields are optional.
  const setActiveView = useCallback(
    (view: ViewId) => {
      pushEntry({ id: view } as HistoryEntry);
    },
    [pushEntry],
  );

  const canGoBack = historyIndex > 0;
  const canGoForward = historyIndex < viewHistory.length - 1;

  const goBack = useCallback(() => {
    setNavState(({ history, index }) =>
      index > 0 ? { history, index: index - 1 } : { history, index },
    );
  }, []);

  const goForward = useCallback(() => {
    setNavState(({ history, index }) =>
      index < history.length - 1
        ? { history, index: index + 1 }
        : { history, index },
    );
  }, []);

  const navigateToAlbum = useCallback(
    (albumId: number) => {
      pushEntry({ id: "album-detail", albumId });
    },
    [pushEntry],
  );

  const navigateToArtist = useCallback(
    (artistId: number) => {
      pushEntry({ id: "artist-detail", artistId });
    },
    [pushEntry],
  );

  const navigateToGenre = useCallback(
    (genreId: number) => {
      pushEntry({ id: "genre-detail", genreId });
    },
    [pushEntry],
  );

  const navigateToPlaylist = useCallback(
    (playlistId: number) => {
      pushEntry({ id: "playlist", playlistId });
    },
    [pushEntry],
  );

  const navigateToWrapped = useCallback(
    (year: number | null) => {
      pushEntry({ id: "wrapped", year });
    },
    [pushEntry],
  );

  function renderView() {
    switch (activeView) {
      case "home":
        return (
          <HomeView
            onNavigate={setActiveView}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
            onNavigateToPlaylist={navigateToPlaylist}
            onNavigateToWrapped={navigateToWrapped}
          />
        );
      case "wrapped":
        return (
          <WrappedView
            onNavigate={setActiveView}
            initialYear={activeWrappedYear}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "library":
        return (
          <LibraryView
            activeTab={libraryTab}
            setActiveTab={setLibraryTab}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
            onNavigateToGenre={navigateToGenre}
          />
        );
      case "settings":
        return <SettingsView onNavigate={setActiveView} />;
      case "spotify":
        return <SpotifyView onNavigate={setActiveView} />;
      case "about":
        return <AboutView onNavigate={setActiveView} />;
      case "feedback":
        return <FeedbackView onNavigate={setActiveView} />;
      case "statistics":
        return (
          <StatisticsView
            onNavigate={setActiveView}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "liked":
        return (
          <LikedView
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "recent":
        return (
          <HistoryView
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "playlist":
        return (
          <PlaylistView
            playlistId={activePlaylistId}
            onAfterDelete={() => replaceEntry({ id: "home" })}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "album-detail":
        return (
          <AlbumDetailView
            albumId={activeAlbumId}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "artist-detail":
        return (
          <ArtistDetailView
            artistId={activeArtistId}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "genre-detail":
        return (
          <GenreDetailView
            genreId={activeGenreId}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
    }
  }

  return (
    <div className="flex flex-col h-screen font-sans">
      <div className="flex flex-col h-screen bg-app-ambient text-zinc-600 dark:text-zinc-300 relative">
        {/* Drag-and-drop overlay — fades in while the user is dragging
            files over the window, and shows an "importing…" state while
            the backend scan runs. Pointer-events disabled so the drop
            still hits Tauri's native handler underneath. */}
        {(dragDrop.isDraggingOver || dragDrop.isImporting) && (
          <div className="fixed inset-0 z-100 pointer-events-none flex items-center justify-center bg-emerald-500/10 backdrop-blur-sm border-4 border-dashed border-emerald-500/60 animate-fade-in">
            <div className="bg-white dark:bg-zinc-900 rounded-2xl shadow-2xl px-8 py-6 flex items-center gap-4">
              {dragDrop.isImporting ? (
                <Loader2 size={28} className="text-emerald-500 animate-spin" />
              ) : (
                <Upload size={28} className="text-emerald-500" />
              )}
              <div>
                <div className="text-base font-semibold text-zinc-900 dark:text-white">
                  {dragDrop.isImporting
                    ? t("dragDrop.importing")
                    : t("dragDrop.dropHint")}
                </div>
                <div className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("dragDrop.subtitle")}
                </div>
              </div>
            </div>
          </div>
        )}

        {/* Main Container */}
        <div className="flex flex-1 overflow-hidden">
          {/* Sidebar wrapper. Animates `width` between 0 and the
              Sidebar's intrinsic `w-64` (256 px) so the toggle
              actually frees the screen real estate instead of just
              hiding the content. The Sidebar itself stays mounted at
              full width — `overflow-hidden` on the wrapper clips it
              when collapsed. `shrink-0` keeps the wrapper from
              participating in the flex shrink algorithm during the
              animation.

              `inert` removes the collapsed subtree from the focus
              order and pointer events in one step (no `tabIndex=-1`
              cascade needed). `aria-hidden` is set in parallel for
              older screen-reader engines that don't follow `inert`
              yet. React 19 supports `inert` as a boolean prop. */}
          <motion.aside
            initial={false}
            animate={{ width: isSidebarHidden ? 0 : 256 }}
            transition={{ duration: 0.22, ease: "easeOut" }}
            className="shrink-0 overflow-hidden"
            aria-hidden={isSidebarHidden}
            inert={isSidebarHidden}
          >
            <Sidebar
              activeView={activeView}
              setActiveView={setActiveView}
              libraryTab={libraryTab}
              setLibraryTab={setLibraryTab}
              activePlaylistId={activePlaylistId}
              navigateToPlaylist={navigateToPlaylist}
            />
          </motion.aside>

          {/* Center Content. `min-w-0` is required so a long playlist
              title or wide table doesn't blow the flex item's intrinsic
              width past `flex-1` and push the right panel off-screen.
              Background uses `/50` opacity so the theme ambient bleeds
              through both modes — without `/50` on the light side, the
              solid `bg-zinc-50` covered the ambient and themed light
              presets (Sunset Light / Lavender Light…) looked stark
              white in the main area. */}
          <div className="flex flex-col flex-1 min-w-0 relative bg-zinc-50/50 dark:bg-zinc-900/50 overflow-hidden">
            <TopBar
              activeView={activeView}
              setActiveView={setActiveView}
              onOpenProfileSelector={() => setIsProfileModalOpen(true)}
              canGoBack={canGoBack}
              canGoForward={canGoForward}
              onGoBack={goBack}
              onGoForward={goForward}
              isSidebarHidden={isSidebarHidden}
              onToggleSidebar={toggleSidebar}
            />

            {/* Main Scrollable Content */}
            <div
              ref={pageScrollRef}
              className="flex-1 overflow-y-auto p-8 relative"
            >
              <PageScrollContext.Provider value={pageScrollRef}>
                <Suspense fallback={<ViewSuspenseFallback />}>
                  {/* Crossfade + slight slide between views. Keyed on
                      the structural view id (album/artist/genre/playlist
                      keep a single key so the inner view handles its own
                      internal navigation — re-keying on every payload
                      change would jank the transition). */}
                  <AnimatePresence mode="wait" initial={false}>
                    <motion.div
                      key={activeView}
                      initial={{ opacity: 0, y: 6 }}
                      animate={{ opacity: 1, y: 0 }}
                      exit={{ opacity: 0, y: -4 }}
                      transition={{ duration: 0.18, ease: "easeOut" }}
                    >
                      {renderView()}
                    </motion.div>
                  </AnimatePresence>
                </Suspense>
              </PageScrollContext.Provider>
            </div>

            {/* Floating overlays anchored to the center column.
                DeviceMenu = popup from the player bar's speaker icon;
                NowPlayingChevronTab = right-edge handle shown only when
                no right panel is open. Both must stay inside the center
                container so their `right-0` anchors to the content edge,
                not to the right panel when one is mounted as a sibling. */}
            <DeviceMenu />
            <NowPlayingChevronTab />
          </div>

          {/* Right Panels — siblings of the center column so opening
              one shrinks the content area instead of overlapping it
              (Spotify-style responsive layout). Only one is mounted at
              a time; the conditional render is the structural mutex.
              Wrapped in AnimatePresence so width/opacity exit animations
              play before unmount. */}
          <AnimatePresence initial={false}>
            {activeRightPanel === "queue" && <QueuePanel />}
            {activeRightPanel === "nowPlaying" && (
              <NowPlayingPanel onNavigateToArtist={navigateToArtist} />
            )}
            {activeRightPanel === "lyrics" && <LyricsPanel />}
          </AnimatePresence>
        </div>

        {/* Bottom Player Bar */}
        <PlayerBar onNavigateToArtist={navigateToArtist} />
      </div>

      <ProfileSelectorModal
        isOpen={isProfileModalOpen}
        onClose={() => setIsProfileModalOpen(false)}
      />

      <LastfmReauthBanner onGoToSettings={() => setActiveView("settings")} />
      <UpdateBanner />
      <ScanProgressToast />
      {showOnboarding && <OnboardingModal onSkip={dismissOnboarding} />}
    </div>
  );
}
