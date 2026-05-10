import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Pause,
  SkipBack,
  SkipForward,
  Heart,
  Maximize2,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { formatDuration } from "../../lib/tauri/track";
import {
  listLikedTrackIds,
  toggleLikeTrack,
} from "../../lib/tauri/track";
import { useState } from "react";

/**
 * Compact always-on-top window. Shows artwork, title/artist, and
 * the bare minimum playback controls (prev / play-pause / next /
 * like). A maximize button restores focus to the main window and
 * closes the mini-player.
 *
 * Mounted in `main.tsx` when the URL search params carry `?mini=1`,
 * which is set by the launcher in [`useMiniPlayer`].
 */
export function MiniPlayer() {
  const { t } = useTranslation();
  const {
    currentTrack,
    isPlaying,
    togglePlayback,
    next,
    previous,
    positionMs,
    durationMs,
  } = usePlayer();

  // Standalone like-state mirror — the mini-player runs in its own
  // webview and doesn't share React state with the main window, so
  // we hydrate from the backend directly.
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [currentTrack?.id]);

  const isLiked = currentTrack ? likedIds.has(currentTrack.id) : false;
  const handleLike = async () => {
    if (!currentTrack) return;
    try {
      const nowLiked = await toggleLikeTrack(currentTrack.id);
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(currentTrack.id);
        else next.delete(currentTrack.id);
        return next;
      });
    } catch (err) {
      console.error("[MiniPlayer] toggle like failed", err);
    }
  };

  const handleMaximize = async () => {
    try {
      // Show + focus the main window, then close ours. Both windows
      // share an AppState on the backend so the player keeps
      // running through the swap.
      const main = await TauriWindow.getByLabel("main");
      if (main) {
        await main.show();
        await main.unminimize();
        await main.setFocus();
      }
      await getCurrentWindow().close();
    } catch (err) {
      console.error("[MiniPlayer] maximize failed", err);
    }
  };

  return (
    <div className="h-screen flex flex-col bg-zinc-900 text-white overflow-hidden">
      {/* Artwork — fills the top, square aspect. */}
      <div className="relative flex-1 min-h-0">
        {currentTrack ? (
          <Artwork
            path={currentTrack.artwork_path}
            path1x={currentTrack.artwork_path_1x}
            path2x={currentTrack.artwork_path_2x}
            alt={currentTrack.title}
            className="w-full h-full object-cover"
            rounded="md"
          />
        ) : (
          <div className="w-full h-full flex items-center justify-center text-zinc-700">
            <Play size={64} />
          </div>
        )}
        {/* Maximize button overlay */}
        <button
          type="button"
          onClick={handleMaximize}
          aria-label={t("miniPlayer.maximize")}
          title={t("miniPlayer.maximize")}
          className="absolute top-2 right-2 p-2 rounded-full bg-black/40 hover:bg-black/60 backdrop-blur-sm transition-colors"
        >
          <Maximize2 size={16} />
        </button>
      </div>

      {/* Track meta */}
      <div className="px-4 pt-3 pb-1">
        <div className="text-sm font-semibold truncate">
          {currentTrack?.title ?? t("miniPlayer.idle")}
        </div>
        <div className="text-xs text-zinc-400 truncate">
          {currentTrack ? (
            <ArtistLink
              name={currentTrack.artist_name}
              artistIds={currentTrack.artist_ids}
              onNavigate={() => {}}
              fallback="—"
              className="hover:underline"
            />
          ) : (
            "—"
          )}
        </div>
      </div>

      {/* Progress (read-only — clicking-to-seek would steal taps from
          the like button on a tiny window). */}
      <div className="px-4 py-2">
        <div className="h-1 rounded-full bg-zinc-700 overflow-hidden">
          <div
            className="h-full bg-emerald-500"
            style={{
              width: `${
                durationMs > 0
                  ? Math.min(100, (positionMs / durationMs) * 100)
                  : 0
              }%`,
            }}
          />
        </div>
        <div className="flex justify-between text-[10px] text-zinc-500 tabular-nums mt-1">
          <span>{formatDuration(positionMs)}</span>
          <span>{formatDuration(durationMs)}</span>
        </div>
      </div>

      {/* Controls */}
      <div className="flex items-center justify-between px-4 pb-4">
        <button
          type="button"
          onClick={handleLike}
          aria-label={t("miniPlayer.like")}
          disabled={!currentTrack}
          className="p-2 disabled:opacity-30"
        >
          <Heart
            size={18}
            className={isLiked ? "fill-emerald-500 text-emerald-500" : "text-zinc-400 hover:text-white"}
          />
        </button>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => previous()}
            aria-label={t("playerBar.previous")}
            className="p-2 text-zinc-300 hover:text-white"
          >
            <SkipBack size={20} />
          </button>
          <button
            type="button"
            onClick={() => togglePlayback()}
            aria-label={isPlaying ? t("player.pause") : t("player.play")}
            className="p-3 rounded-full bg-emerald-500 hover:bg-emerald-600 text-white"
          >
            {isPlaying ? <Pause size={20} /> : <Play size={20} />}
          </button>
          <button
            type="button"
            onClick={() => next()}
            aria-label={t("playerBar.next")}
            className="p-2 text-zinc-300 hover:text-white"
          >
            <SkipForward size={20} />
          </button>
        </div>
        <div className="w-9" /> {/* spacer to balance the like button */}
      </div>
    </div>
  );
}
