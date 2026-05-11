import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Menu,
  MonitorSpeaker,
  Heart,
  Mic2,
  PictureInPicture2,
  Maximize2,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { useSleepTimer } from "../../hooks/useSleepTimer";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { SleepTimerMenu } from "./SleepTimerMenu";
import { AbLoopButton } from "./AbLoopButton";
import { VolumeControl } from "./VolumeControl";
import { AudioQualityFooter } from "./AudioQualityFooter";
import { FullscreenNowPlaying } from "./FullscreenNowPlaying";
import { toggleLikeTrack, listLikedTrackIds } from "../../lib/tauri/track";
import { getProfileSetting } from "../../lib/tauri/profile";

interface PlayerBarProps {
  onNavigateToArtist: (artistId: number) => void;
}

export function PlayerBar({ onNavigateToArtist }: PlayerBarProps) {
  const { t } = useTranslation();
  const {
    isQueueOpen,
    toggleQueue,
    isLyricsOpen,
    toggleLyrics,
    isDeviceMenuOpen,
    toggleDeviceMenu,
    currentTrack,
    volume,
    setVolume,
    activeProvider,
  } = usePlayer();

  const sleepTimer = useSleepTimer({ currentVolume: volume, setVolume });

  // Per-profile preference: hide the sleep-timer / A-B loop icons.
  // Default: hidden — both are niche features that mostly clutter
  // the player bar for typical users; opt-in via Settings.
  // SettingsView dispatches `waveflow:sleep-timer-visibility` /
  // `waveflow:ab-loop-visibility` window events after toggling so
  // we re-read without polling.
  const [showSleepTimer, setShowSleepTimer] = useState(false);
  const [showAbLoop, setShowAbLoop] = useState(false);
  useEffect(() => {
    const refreshSleep = () => {
      getProfileSetting("ui.show_sleep_timer")
        .then((v) => {
          // Missing key → treat as "false" (off by default).
          setShowSleepTimer(v == null ? false : v === "1" || v === "true");
        })
        .catch(() => {});
    };
    const refreshAb = () => {
      getProfileSetting("ui.show_ab_loop")
        .then((v) => {
          setShowAbLoop(v == null ? false : v === "1" || v === "true");
        })
        .catch(() => {});
    };
    refreshSleep();
    refreshAb();
    window.addEventListener("waveflow:sleep-timer-visibility", refreshSleep);
    window.addEventListener("waveflow:ab-loop-visibility", refreshAb);
    return () => {
      window.removeEventListener(
        "waveflow:sleep-timer-visibility",
        refreshSleep,
      );
      window.removeEventListener("waveflow:ab-loop-visibility", refreshAb);
    };
  }, []);

  // Subscribe to the backend's track-ended event so the sleep timer
  // in "end of track" mode triggers when the current track finishes
  // naturally (not when the user skips). Listening to the event is
  // more reliable than diffing currentTrack.id because the player
  // can flip directly from track A to track B without ever passing
  // through null in the auto-advance path.
  useEffect(() => {
    let unlistenFn: (() => void) | null = null;
    let cancelled = false;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      const off = await listen("player:track-ended", () => {
        sleepTimer.notifyTrackEnded();
      });
      if (cancelled) off();
      else unlistenFn = off;
    })();
    return () => {
      cancelled = true;
      unlistenFn?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());

  // Load liked IDs on mount + refresh when track changes (the user
  // might have liked/unliked from the library view).
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [currentTrack?.id]);

  const isSpotify = activeProvider === "spotify";
  const isLiked =
    currentTrack != null && !isSpotify && likedIds.has(currentTrack.id);

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

  // Apple-Music-style immersive Now Playing overlay. Local UI state —
  // not in PlayerContext because no other view needs to know about it
  // (unlike the side panels which other components query).
  const [isFullscreenOpen, setIsFullscreenOpen] = useState(false);

  return (
    <>
    <div className="flex flex-col z-50 border-t bg-[#FAFAFA] border-zinc-200 text-zinc-600 dark:bg-surface-dark-elevated dark:border-zinc-800 dark:text-zinc-300">
      <div className="h-24 px-6 flex items-center justify-between">
        {/* Left: Track Info */}
        <div className="w-1/3 flex items-center space-x-4 min-w-0">
          {/* Click the cover to open the immersive Now Playing
              overlay (mirrors Apple Music). Disabled when no track is
              loaded so the user doesn't open an empty card. */}
          <button
            type="button"
            onClick={() => currentTrack && setIsFullscreenOpen(true)}
            disabled={!currentTrack}
            aria-label={t("playerBar.openFullscreen")}
            title={t("playerBar.openFullscreen")}
            className="shrink-0 rounded-xl focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-default"
          >
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
          </button>
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
          {currentTrack && !isSpotify && (
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
              <Heart size={16} className={isLiked ? "fill-current" : ""} />
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
          {/* A-B repeat — sits left of the sleep timer. */}
          {showAbLoop && <AbLoopButton />}

          {/* Sleep timer (sits left of Lyrics; user-hideable from
            Settings via `ui.show_sleep_timer`). */}
          {showSleepTimer && (
            <SleepTimerMenu
              status={sleepTimer.status}
              onSetDuration={sleepTimer.setDurationMinutes}
              onSetEndOfTrack={sleepTimer.setEndOfTrack}
              onCancel={sleepTimer.cancel}
            />
          )}

          {/* Fullscreen Now Playing trigger — sits next to the
              lyrics toggle so the two "expand" actions cluster
              together. Disabled when no track is loaded. */}
          <button
            type="button"
            onClick={() => setIsFullscreenOpen(true)}
            disabled={!currentTrack}
            aria-label={t("playerBar.openFullscreen")}
            title={t("playerBar.openFullscreen")}
            className="p-2 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            <Maximize2 size={18} />
          </button>

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

          {/* Mini-player available in both local and Spotify modes.
              In Spotify mode, the mini doesn't attach the Web Playback
              SDK (it can only bind to one webview) — it mirrors the
              main window's state via the `spotify:state` Tauri event
              and routes its controls through the Connect API. */}
          <button
            onClick={() => {
              import("../../lib/miniPlayer").then((m) =>
                m.openMiniPlayer().catch((err) => {
                  console.error("[PlayerBar] open mini-player failed", err);
                }),
              );
            }}
            aria-label={t("playerBar.miniPlayer")}
            title={t("playerBar.miniPlayer")}
            className="p-2 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
          >
            <PictureInPicture2 size={20} />
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

          {!isSpotify && (
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
          )}

          <VolumeControl />
        </div>
      </div>
      <AudioQualityFooter track={isSpotify ? null : (currentTrack ?? null)} />
    </div>
    {isFullscreenOpen && currentTrack && (
      <FullscreenNowPlaying
        onClose={() => setIsFullscreenOpen(false)}
        onNavigateToArtist={onNavigateToArtist}
        isLiked={isLiked}
        onToggleLike={handleToggleLike}
      />
    )}
    </>
  );
}
