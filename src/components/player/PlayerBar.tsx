import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Menu, MonitorSpeaker, Heart, Mic2, PanelRight } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";
import { AudioQualityFooter } from "./AudioQualityFooter";
import { toggleLikeTrack, listLikedTrackIds } from "../../lib/tauri/track";

interface PlayerBarProps {
  onNavigateToArtist: (artistId: number) => void;
}

export function PlayerBar({ onNavigateToArtist }: PlayerBarProps) {
  const { t } = useTranslation();
  const {
    isQueueOpen,
    toggleQueue,
    isNowPlayingOpen,
    toggleNowPlaying,
    isLyricsOpen,
    toggleLyrics,
    isDeviceMenuOpen,
    toggleDeviceMenu,
    currentTrack,
  } = usePlayer();

  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());

  // Load liked IDs on mount + refresh when track changes (the user
  // might have liked/unliked from the library view).
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [currentTrack?.id]);

  const isLiked = currentTrack != null && likedIds.has(currentTrack.id);

  const handleToggleLike = async () => {
    if (!currentTrack) return;
    const nowLiked = await toggleLikeTrack(currentTrack.id);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(currentTrack.id);
      else next.delete(currentTrack.id);
      return next;
    });
  };

  const title = currentTrack?.title ?? t("player.noTrack");

  return (
    <div className="flex flex-col z-50 border-t bg-[#FAFAFA] border-zinc-200 text-zinc-600 dark:bg-surface-dark-elevated dark:border-zinc-800 dark:text-zinc-300">
    <div className="h-24 px-6 flex items-center justify-between">
      {/* Left: Track Info */}
      <div className="w-1/3 flex items-center space-x-4 min-w-0">
        <Artwork
          path={currentTrack?.artwork_path ?? null}
          path1x={currentTrack?.artwork_path_1x ?? null}
          path2x={currentTrack?.artwork_path_2x ?? null}
          size="1x"
          className="w-14 h-14 shadow-sm border border-zinc-200 dark:border-transparent"
          iconSize={24}
          alt={title}
          rounded="xl"
        />
        <div className="flex flex-col min-w-0">
          <span className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 truncate">
            {title}
          </span>
          <span className="text-[11px] text-zinc-500 dark:text-zinc-400 truncate">
            {currentTrack?.artist_name ? (
              <ArtistLink
                name={currentTrack.artist_name}
                artistIds={currentTrack.artist_ids}
                onNavigate={onNavigateToArtist}
              />
            ) : (
              (currentTrack?.album_title ?? t("player.inactive"))
            )}
          </span>
        </div>
        {currentTrack && (
          <button
            type="button"
            onClick={handleToggleLike}
            aria-label={isLiked ? t("liked.unlike") : t("liked.like")}
            className={`p-1.5 rounded-full transition-colors shrink-0 ${
              isLiked
                ? "text-pink-500"
                : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
            }`}
          >
            <Heart
              size={16}
              className={isLiked ? "fill-current" : ""}
            />
          </button>
        )}
      </div>

      {/* Center: Controls */}
      <div className="w-1/3 flex flex-col items-center max-w-md">
        <PlaybackControls />
        <ProgressBar />
      </div>

      {/* Right: Extra Controls */}
      <div className="w-1/3 flex items-center justify-end space-x-4">
        {/* Lyrics panel toggle */}
        <button
          type="button"
          onClick={toggleLyrics}
          aria-label={t("playerBar.lyrics")}
          title={t("playerBar.lyrics")}
          className={`p-2 rounded-lg transition-colors ${
            isLyricsOpen
              ? "text-emerald-500"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
          }`}
        >
          <Mic2 size={20} />
        </button>

        {/* Now Playing panel */}
        <button
          type="button"
          onClick={toggleNowPlaying}
          aria-label={t("playerBar.nowPlaying")}
          title={t("playerBar.nowPlaying")}
          className={`p-2 rounded-lg transition-colors ${
            isNowPlayingOpen
              ? "text-emerald-500"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
          }`}
        >
          <PanelRight size={20} />
        </button>

        <button
          onClick={toggleQueue}
          aria-label={t("playerBar.queue")}
          title={t("playerBar.queue")}
          className={`p-2 rounded-lg transition-colors ${
            isQueueOpen
              ? "text-emerald-500"
              : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
          }`}
        >
          <Menu size={20} />
        </button>

        <div className="relative">
          <button
            onClick={toggleDeviceMenu}
            className={`p-2 rounded-lg transition-colors border ${
              isDeviceMenuOpen
                ? "border-emerald-500 text-emerald-500 bg-emerald-500/10"
                : "border-transparent text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-800"
            }`}
          >
            <MonitorSpeaker size={20} />
          </button>
        </div>

        <VolumeControl />
      </div>
      </div>
      <AudioQualityFooter track={currentTrack ?? null} />
    </div>
  );
}
