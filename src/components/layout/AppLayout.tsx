import { useState, useCallback } from "react";
import type { ViewId, LibraryTab } from "../../types";
import { useTheme } from "../../hooks/useTheme";
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
    </div>
  );
}
