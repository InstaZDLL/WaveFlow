import { useState, useCallback, useEffect, useRef } from "react";
import type { ViewId, LibraryTab } from "../../types";
import { useTheme } from "../../hooks/useTheme";
import { useLibrary } from "../../hooks/useLibrary";
import { useProfile } from "../../hooks/useProfile";
import {
  getProfileSetting,
  setProfileSetting,
} from "../../lib/tauri/profile";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { QueuePanel } from "./QueuePanel";
import { NowPlayingPanel } from "./NowPlayingPanel";
import { LyricsPanel } from "./LyricsPanel";
import { DeviceMenu } from "./DeviceMenu";
import { PlayerBar } from "../player/PlayerBar";
import { HomeView } from "../views/HomeView";
import { LibraryView } from "../views/LibraryView";
import { SettingsView } from "../views/SettingsView";
import { AboutView } from "../views/AboutView";
import { FeedbackView } from "../views/FeedbackView";
import { StatisticsView } from "../views/StatisticsView";
import { LikedView } from "../views/LikedView";
import { RecentView } from "../views/RecentView";
import { PlaylistView } from "../views/PlaylistView";
import { AlbumDetailView } from "../views/AlbumDetailView";
import { ArtistDetailView } from "../views/ArtistDetailView";
import { ProfileSelectorModal } from "../common/ProfileSelectorModal";
import { LastfmReauthBanner } from "../common/LastfmReauthBanner";
import { UpdateBanner } from "../common/UpdateBanner";
import { OnboardingModal } from "../common/OnboardingModal";

export function AppLayout() {
  const { isDark } = useTheme();
  const [viewHistory, setViewHistory] = useState<ViewId[]>(["home"]);
  const [historyIndex, setHistoryIndex] = useState(0);
  const [isProfileModalOpen, setIsProfileModalOpen] = useState(false);
  const [libraryTab, setLibraryTab] = useState<LibraryTab>("morceaux");
  // Currently focused playlist for the "playlist" view. The view itself
  // re-fetches when this id changes; the sidebar uses it to highlight
  // the active row.
  const [activePlaylistId, setActivePlaylistId] = useState<number | null>(null);
  const [activeAlbumId, setActiveAlbumId] = useState<number | null>(null);
  const [activeArtistId, setActiveArtistId] = useState<number | null>(null);

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
  const { libraries, isLoading: isLibraryLoading } = useLibrary();
  const { activeProfile, isLoading: isProfileLoading } = useProfile();
  const [showOnboarding, setShowOnboarding] = useState(false);
  // Tracks the active profile id we've already evaluated against, so
  // a profile switch re-runs the gate exactly once.
  const evaluatedProfileId = useRef<number | null>(null);

  useEffect(() => {
    if (isProfileLoading || isLibraryLoading) return;
    if (!activeProfile) return;
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
        libraries.length === 0 ||
        libraries.every((l) => l.folder_count === 0);
      setShowOnboarding(isEmpty);
    })();

    return () => {
      cancelled = true;
    };
  }, [activeProfile, isProfileLoading, isLibraryLoading, libraries]);

  const dismissOnboarding = useCallback(() => {
    // Persist the choice so the modal doesn't reappear on next
    // launch — the user already told us to leave them alone, even
    // if their library is still empty.
    setShowOnboarding(false);
    setProfileSetting("onboarding.dismissed", "true", "bool").catch((err) =>
      console.error("[AppLayout] persist onboarding.dismissed failed", err),
    );
  }, []);

  const activeView = viewHistory[historyIndex];

  const setActiveView = useCallback(
    (view: ViewId) => {
      setViewHistory((prev) => [...prev.slice(0, historyIndex + 1), view]);
      setHistoryIndex((prev) => prev + 1);
    },
    [historyIndex]
  );

  const canGoBack = historyIndex > 0;
  const canGoForward = historyIndex < viewHistory.length - 1;

  const goBack = useCallback(() => {
    if (canGoBack) setHistoryIndex((i) => i - 1);
  }, [canGoBack]);

  const goForward = useCallback(() => {
    if (canGoForward) setHistoryIndex((i) => i + 1);
  }, [canGoForward]);

  const navigateToAlbum = useCallback(
    (albumId: number) => {
      setActiveAlbumId(albumId);
      setActiveView("album-detail");
    },
    [setActiveView],
  );

  const navigateToArtist = useCallback(
    (artistId: number) => {
      setActiveArtistId(artistId);
      setActiveView("artist-detail");
    },
    [setActiveView],
  );

  function renderView() {
    switch (activeView) {
      case "home":
        return <HomeView onNavigate={setActiveView} />;
      case "library":
        return (
          <LibraryView
            activeTab={libraryTab}
            setActiveTab={setLibraryTab}
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "settings":
        return <SettingsView onNavigate={setActiveView} />;
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
          <RecentView
            onNavigateToAlbum={navigateToAlbum}
            onNavigateToArtist={navigateToArtist}
          />
        );
      case "playlist":
        return (
          <PlaylistView
            playlistId={activePlaylistId}
            onAfterDelete={() => {
              setActivePlaylistId(null);
              setActiveView("home");
            }}
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
          />
        );
    }
  }

  return (
    <div
      className={`flex flex-col h-screen font-sans ${isDark ? "dark" : ""}`}
    >
      <div className="flex flex-col h-screen bg-white text-zinc-600 dark:bg-surface-dark dark:text-zinc-300">
        {/* Main Container */}
        <div className="flex flex-1 overflow-hidden">
          <Sidebar
            activeView={activeView}
            setActiveView={setActiveView}
            libraryTab={libraryTab}
            setLibraryTab={setLibraryTab}
            activePlaylistId={activePlaylistId}
            setActivePlaylistId={setActivePlaylistId}
          />

          {/* Center Content */}
          <div className="flex flex-col flex-1 relative bg-zinc-50 dark:bg-zinc-900/50 overflow-hidden">
            <TopBar
              activeView={activeView}
              setActiveView={setActiveView}
              onOpenProfileSelector={() => setIsProfileModalOpen(true)}
              canGoBack={canGoBack}
              canGoForward={canGoForward}
              onGoBack={goBack}
              onGoForward={goForward}
            />

            {/* Main Scrollable Content */}
            <div className="flex-1 overflow-y-auto p-8 relative">
              {renderView()}
            </div>

            {/* Right Panels (Overlays) — mutually exclusive via PlayerContext */}
            <DeviceMenu />
            <QueuePanel />
            <NowPlayingPanel onNavigateToArtist={navigateToArtist} />
            <LyricsPanel />
          </div>
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
      {showOnboarding && <OnboardingModal onSkip={dismissOnboarding} />}
    </div>
  );
}
