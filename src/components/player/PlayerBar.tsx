import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Menu,
  MonitorSpeaker,
  Heart,
  Star,
  Mic2,
  Maximize2,
  PictureInPicture2,
  Radio,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { useSleepTimer } from "../../hooks/useSleepTimer";
import { usePlayerBarLayout } from "../../hooks/usePlayerBarLayout";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { HiResBadge } from "../common/HiResBadge";
import { MarqueeText } from "../common/MarqueeText";
import { isRadioTrack } from "../../lib/playerSources";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { SleepTimerMenu } from "./SleepTimerMenu";
import { AbLoopButton } from "./AbLoopButton";
import { VolumeControl } from "./VolumeControl";
import { EqPresetButton } from "./EqPresetButton";
import { MoreActionsMenu } from "./MoreActionsMenu";
import { AudioQualityFooter } from "./AudioQualityFooter";
import { ImmersiveView } from "./ImmersiveView";
import { toggleLikeTrack, listLikedTrackIds } from "../../lib/tauri/track";

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
    currentRadioStation,
    volume,
    setVolume,
    activeProvider,
    immersiveOpen,
    immersiveInitialTab,
    openFullscreenNowPlaying,
    closeImmersive,
    toggleNowPlaying,
  } = usePlayer();

  // Web Radio favorites — for a live stream the like ♥ is replaced by a
  // station-favorite ★ (a radio track has no library row to "like").
  const radioFavorites = useWebRadioFavorites();

  const sleepTimer = useSleepTimer({ currentVolume: volume, setVolume });

  // Per-profile player-bar layout: which optional buttons render in
  // the primary cluster, which fall through to the "⋯" overflow,
  // and what clicking the small cover thumbnail does. Defaults
  // match the historical hard-coded behaviour so the upgrade is
  // invisible until the user visits Settings → Playback.
  const layout = usePlayerBarLayout();

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
  const stationFavorited =
    currentRadioStation != null &&
    radioFavorites.isFavorite(currentRadioStation.id);

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

  // Cover-thumbnail click action — defaults to opening the immersive
  // overlay (Apple Music style), can be reconfigured to open the Now
  // Playing right-side panel (Spotify style) or do nothing.
  const coverDisabled = !currentTrack || layout.coverAction === "none";
  const coverLabelKey =
    layout.coverAction === "now_playing"
      ? "playerBar.toggleNowPlayingPanel"
      : "playerBar.openFullscreen";
  const handleCoverClick = () => {
    if (!currentTrack) return;
    if (layout.coverAction === "now_playing") toggleNowPlaying();
    else if (layout.coverAction === "immersive") openFullscreenNowPlaying();
    // `none` → no-op (button is disabled, this branch is defensive).
  };

  return (
    <>
      <footer className="flex flex-col z-50 border-t bg-white border-zinc-200 text-zinc-600 dark:bg-surface-dark-elevated dark:border-zinc-800 dark:text-zinc-300">
        <div className="h-24 px-4 flex items-center justify-between">
          {/* Left: Track Info */}
          <div className="w-1/3 flex items-center space-x-3 min-w-0">
            {/* Click the cover — action driven by `ui.cover_action`
              (immersive / now-playing panel / none). Disabled when
              no track is loaded OR the user opted out via Settings. */}
            <button
              type="button"
              onClick={handleCoverClick}
              disabled={coverDisabled}
              aria-label={t(coverLabelKey)}
              title={t(coverLabelKey)}
              className="shrink-0 rounded-xl focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-default"
            >
              <Artwork
                path={currentTrack?.artwork_path ?? null}
                path1x={currentTrack?.artwork_path_1x ?? null}
                path2x={currentTrack?.artwork_path_2x ?? null}
                size="1x"
                className="w-14 h-14 shadow-sm"
                iconSize={24}
                alt={title}
                rounded="xl"
                // Web Radio streams that ship no cover URL fall back
                // to a radio icon instead of the generic CD disc so
                // the source type is recognisable at a glance.
                placeholderIcon={isRadioTrack(currentTrack) ? Radio : undefined}
              />
            </button>
            <div className="flex flex-col min-w-0">
              <MarqueeText
                text={title}
                className="text-sm font-semibold text-zinc-900 dark:text-zinc-100"
              />
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
              {/* Spotify-style minimal quality label under the artist.
                  HiResBadge renders null when the track isn't Hi-Res /
                  DSD OR when the user has hidden the badge from
                  Settings → Appearance, so the row collapses naturally
                  for lossy / lossless-16-bit content. */}
              {currentTrack && (
                <HiResBadge
                  bitDepth={currentTrack.bit_depth}
                  sampleRate={currentTrack.sample_rate}
                  codec={currentTrack.codec}
                  variant="text"
                />
              )}
              {/* Live radio: surface the station identity under the
                  now-playing song (the title/artist rows above show the
                  ICY song). HiResBadge renders null for radio so this
                  reuses the freed third row. */}
              {currentRadioStation && (
                <span className="flex items-center gap-1 text-[11px] text-zinc-500 dark:text-zinc-400 truncate">
                  <Radio size={11} className="shrink-0" />
                  <span className="truncate">
                    {currentRadioStation.artist
                      ? `${currentRadioStation.title} · ${currentRadioStation.artist}`
                      : currentRadioStation.title}
                  </span>
                </span>
              )}
            </div>
            {currentRadioStation ? (
              // Live radio: favorite the STATION, not the now-playing
              // song (whose id is a negative sentinel with no library
              // row to like).
              <button
                type="button"
                onClick={() =>
                  radioFavorites.toggleFavorite(currentRadioStation)
                }
                aria-label={
                  stationFavorited
                    ? t("webRadio.removeFavorite")
                    : t("webRadio.addFavorite")
                }
                aria-pressed={stationFavorited}
                className={`p-2 rounded-full transition-colors shrink-0 ${
                  stationFavorited
                    ? "text-amber-500"
                    : "text-zinc-300 dark:text-zinc-600 hover:text-amber-500"
                }`}
              >
                <Star
                  size={18}
                  fill={stationFavorited ? "currentColor" : "none"}
                />
              </button>
            ) : currentTrack && !isSpotify && !isRadioTrack(currentTrack) ? (
              // `!isRadioTrack` guards the hydration race + the idle
              // tail: a radio sentinel track (negative id) must never
              // show a ♥ like (it has no library row), even in the brief
              // window before `currentRadioStation` arrives.
              <button
                type="button"
                onClick={handleToggleLike}
                aria-label={isLiked ? t("liked.unlike") : t("liked.like")}
                className={`p-2 rounded-full transition-colors shrink-0 ${
                  isLiked
                    ? "text-pink-500"
                    : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
                }`}
              >
                <Heart size={18} className={isLiked ? "fill-current" : ""} />
              </button>
            ) : null}
          </div>

          {/* Center: Controls */}
          <div className="w-1/3 flex flex-col items-center max-w-md">
            <PlaybackControls />
            <ProgressBar />
          </div>

          {/* Right: Extra Controls */}
          <div className="w-1/3 flex items-center justify-end space-x-2">
            {/* A-B repeat (primary slot — opt-in pin via Settings).
              When unpinned, the entry lives in the "⋯" overflow. */}
            {layout.showAbLoop && <AbLoopButton />}

            {/* Sleep timer (primary slot — opt-in pin via Settings).
              Same overflow-by-default rule as A-B loop. */}
            {layout.showSleepTimer && (
              <SleepTimerMenu
                status={sleepTimer.status}
                onSetDuration={sleepTimer.setDurationMinutes}
                onSetEndOfTrack={sleepTimer.setEndOfTrack}
                onCancel={sleepTimer.cancel}
              />
            )}

            {/* EQ preset popover (primary slot — opt-in pin via
              Settings). Quick switcher between the 20 built-in
              presets without opening the full EQ card. */}
            {layout.showEqPreset && !isSpotify && <EqPresetButton />}

            {/* Lyrics panel toggle */}
            {layout.showLyrics && (
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
            )}

            {layout.showQueue && (
              <button
                type="button"
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
            )}

            {layout.showDevice && !isSpotify && (
              <div className="relative">
                <button
                  onClick={toggleDeviceMenu}
                  aria-label={t("playerBar.devices")}
                  title={t("playerBar.devices")}
                  aria-expanded={isDeviceMenuOpen}
                  className={`p-1.5 rounded-lg transition-colors border ${
                    isDeviceMenuOpen
                      ? "border-emerald-500 text-emerald-500 bg-emerald-500/10"
                      : "border-transparent text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-800"
                  }`}
                >
                  <MonitorSpeaker size={20} />
                </button>
              </div>
            )}

            {/* Overflow menu — hosts playback speed, EQ presets,
              Sleep timer, A-B loop (each appears here only when NOT
              pinned to the primary cluster). Hidden when nothing
              would go inside (Spotify mode + every feature pinned). */}
            {(!isSpotify ||
              !layout.showSleepTimer ||
              !layout.showAbLoop ||
              !layout.showEqPreset) && (
              <MoreActionsMenu
                pinAbLoop={layout.showAbLoop}
                pinSleepTimer={layout.showSleepTimer}
                pinEqPreset={layout.showEqPreset}
                showSpeed={!isSpotify}
                showEq={!isSpotify}
                sleepTimer={{
                  status: sleepTimer.status,
                  onSetDuration: sleepTimer.setDurationMinutes,
                  onSetEndOfTrack: sleepTimer.setEndOfTrack,
                  onCancel: sleepTimer.cancel,
                }}
              />
            )}

            <VolumeControl />

            {/* Spotify-style right cluster: mini-player + immersive
              full-screen as primary icon buttons after volume. Both
              are now opt-out via Settings → Playback → Player bar
              layout. Mini-player stays unavailable in Spotify mode
              (Web Playback SDK can't drive a second webview). */}
            {layout.showMiniPlayer && !isSpotify && (
              <button
                type="button"
                onClick={() => {
                  import("../../lib/miniPlayer").then((m) =>
                    m.openMiniPlayer().catch((err) => {
                      console.error("[PlayerBar] open mini-player failed", err);
                    }),
                  );
                }}
                aria-label={t("playerBar.miniPlayer")}
                title={t("playerBar.miniPlayer")}
                className="p-1.5 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
              >
                <PictureInPicture2 size={20} />
              </button>
            )}

            {layout.showImmersive && (
              <button
                type="button"
                onClick={() => currentTrack && openFullscreenNowPlaying()}
                disabled={!currentTrack}
                aria-label={t("playerBar.openFullscreen")}
                title={t("playerBar.openFullscreen")}
                className="p-1.5 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              >
                <Maximize2 size={20} />
              </button>
            )}
          </div>
        </div>
        {layout.showAudioQualityFooter && (
          <AudioQualityFooter
            track={isSpotify ? null : (currentTrack ?? null)}
          />
        )}
      </footer>
      {immersiveOpen && (
        <ImmersiveView
          initialTab={immersiveInitialTab}
          onClose={closeImmersive}
          onNavigateToArtist={onNavigateToArtist}
          isLiked={isLiked}
          onToggleLike={handleToggleLike}
        />
      )}
    </>
  );
}
