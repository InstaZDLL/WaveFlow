import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Pause,
  SkipBack,
  SkipForward,
  Heart,
  Maximize2,
  X,
  Pin,
  Repeat,
  Repeat1,
  Shuffle,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { dominantColor, darken, rgb } from "../../lib/dominantColor";
import {
  listLikedTrackIds,
  toggleLikeTrack,
} from "../../lib/tauri/track";

/**
 * Spotify-style always-on-top widget. Square cover floats centered
 * with a shadow; the window background takes a gradient sampled from
 * the cover's dominant colour so the whole widget feels colour-aware.
 *
 * Hovering the cover reveals a translucent control bar (shuffle / prev
 * / play / next / repeat) — the "minimal" idle state shows just the
 * artwork. Title, artist and a like button live below, plus a top bar
 * with always-on-top toggle, the macOS-style drag dots, and close.
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
    repeatMode,
    cycleRepeatMode,
    isShuffled,
    toggleShuffle,
  } = usePlayer();

  // ── Like-state mirror (own webview = own React state) ───────────
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
        const n = new Set(prev);
        if (nowLiked) n.add(currentTrack.id);
        else n.delete(currentTrack.id);
        return n;
      });
    } catch (err) {
      console.error("[MiniPlayer] like failed", err);
    }
  };

  // ── Cover-derived background gradient ───────────────────────────
  const artworkUrl = useMemo(() => {
    if (!currentTrack) return null;
    return resolveArtwork(
      {
        full: currentTrack.artwork_path,
        x1: currentTrack.artwork_path_1x,
        x2: currentTrack.artwork_path_2x,
      },
      "full",
    );
  }, [currentTrack]);

  const [bgColor, setBgColor] = useState<{ r: number; g: number; b: number }>({
    r: 39,
    g: 39,
    b: 42,
  });
  useEffect(() => {
    let cancelled = false;
    if (!artworkUrl) {
      /* eslint-disable react-hooks/set-state-in-effect */
      setBgColor({ r: 39, g: 39, b: 42 });
      /* eslint-enable react-hooks/set-state-in-effect */
      return;
    }
    dominantColor(artworkUrl)
      .then((c) => {
        if (!cancelled) setBgColor(c);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [artworkUrl]);

  const gradient = `linear-gradient(160deg, ${rgb(bgColor)} 0%, ${rgb(darken(bgColor, 0.45))} 70%, ${rgb(darken(bgColor, 0.2))} 100%)`;

  // ── Window controls (always-on-top toggle persisted; close ≠ exit
  //    — we just close the mini window, the main app keeps running) ─
  const [pinned, setPinned] = useState(true);
  const handleTogglePin = async () => {
    try {
      const win = getCurrentWindow();
      const next = !pinned;
      await win.setAlwaysOnTop(next);
      setPinned(next);
    } catch (err) {
      console.error("[MiniPlayer] pin toggle failed", err);
    }
  };

  const handleMaximize = async () => {
    try {
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

  const handleClose = async () => {
    try {
      const main = await TauriWindow.getByLabel("main");
      if (main) await main.show();
      await getCurrentWindow().close();
    } catch (err) {
      console.error("[MiniPlayer] close failed", err);
    }
  };

  const [showControls, setShowControls] = useState(false);

  const progressPct = durationMs > 0 ? (positionMs / durationMs) * 100 : 0;

  return (
    <div
      className="h-screen w-screen flex flex-col overflow-hidden text-white select-none"
      style={{ background: gradient }}
    >
      {/* Top bar — drag region. The middle dot cluster is the macOS
          / WinAppSDK drag handle hint; Tauri's data-tauri-drag-region
          turns it into an OS-level move grip. */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between px-2 py-1.5 shrink-0"
      >
        <button
          type="button"
          onClick={handleTogglePin}
          aria-label={t("miniPlayer.pin")}
          title={t("miniPlayer.pin")}
          className={`p-1.5 rounded-full transition-colors ${
            pinned
              ? "text-emerald-400 hover:bg-white/10"
              : "text-white/60 hover:text-white hover:bg-white/10"
          }`}
        >
          <Pin size={14} className={pinned ? "fill-current" : ""} />
        </button>
        <div
          data-tauri-drag-region
          className="flex-1 flex items-center justify-center gap-0.5 text-white/40"
        >
          <span className="block w-1 h-1 rounded-full bg-current" />
          <span className="block w-1 h-1 rounded-full bg-current" />
          <span className="block w-1 h-1 rounded-full bg-current" />
          <span className="block w-1 h-1 rounded-full bg-current ml-1" />
          <span className="block w-1 h-1 rounded-full bg-current" />
          <span className="block w-1 h-1 rounded-full bg-current" />
        </div>
        <div className="flex items-center gap-0.5">
          <button
            type="button"
            onClick={handleMaximize}
            aria-label={t("miniPlayer.maximize")}
            title={t("miniPlayer.maximize")}
            className="p-1.5 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors"
          >
            <Maximize2 size={13} />
          </button>
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("miniPlayer.close")}
            title={t("miniPlayer.close")}
            className="p-1.5 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {/* Floating cover with hover overlay */}
      <div className="px-4 pt-2 pb-3 flex justify-center">
        <CoverWithControls
          showControls={showControls}
          onMouseEnter={() => setShowControls(true)}
          onMouseLeave={() => setShowControls(false)}
          isPlaying={isPlaying}
          repeatMode={repeatMode}
          isShuffled={isShuffled}
          onPlayPause={togglePlayback}
          onPrev={previous}
          onNext={next}
          onCycleRepeat={cycleRepeatMode}
          onToggleShuffle={toggleShuffle}
          artworkSlot={
            currentTrack ? (
              <Artwork
                path={currentTrack.artwork_path}
                path1x={currentTrack.artwork_path_1x}
                path2x={currentTrack.artwork_path_2x}
                size="full"
                alt={currentTrack.title}
                className="w-full h-full object-cover"
                rounded="2xl"
              />
            ) : (
              <div className="w-full h-full rounded-2xl bg-white/10 flex items-center justify-center">
                <Play size={48} className="text-white/40" />
              </div>
            )
          }
        />
      </div>

      {/* Title + artist */}
      <div className="px-4 pb-1">
        <div
          className="text-base font-bold truncate"
          title={currentTrack?.title}
        >
          {currentTrack?.title ?? t("miniPlayer.idle")}
        </div>
        <div className="flex items-center justify-between gap-2 mt-0.5">
          <div
            className="text-xs text-white/70 truncate"
            title={currentTrack?.artist_name ?? undefined}
          >
            {currentTrack?.artist_name ?? "—"}
          </div>
          <button
            type="button"
            onClick={handleLike}
            disabled={!currentTrack}
            aria-label={t("miniPlayer.like")}
            className="p-1 disabled:opacity-30 shrink-0"
          >
            <Heart
              size={16}
              className={
                isLiked
                  ? "fill-emerald-400 text-emerald-400"
                  : "text-white/60 hover:text-white"
              }
            />
          </button>
        </div>
      </div>

      {/* Slim progress bar at the very bottom */}
      <div className="mt-auto px-4 pb-3">
        <div className="h-0.5 rounded-full bg-white/15 overflow-hidden">
          <div
            className="h-full bg-white transition-[width] duration-200"
            style={{ width: `${Math.min(100, progressPct)}%` }}
          />
        </div>
      </div>
    </div>
  );
}

interface CoverWithControlsProps {
  showControls: boolean;
  onMouseEnter: () => void;
  onMouseLeave: () => void;
  isPlaying: boolean;
  repeatMode: "off" | "all" | "one";
  isShuffled: boolean;
  onPlayPause: () => void;
  onPrev: () => void;
  onNext: () => void;
  onCycleRepeat: () => void;
  onToggleShuffle: () => void;
  artworkSlot: React.ReactNode;
}

function CoverWithControls({
  showControls,
  onMouseEnter,
  onMouseLeave,
  isPlaying,
  repeatMode,
  isShuffled,
  onPlayPause,
  onPrev,
  onNext,
  onCycleRepeat,
  onToggleShuffle,
  artworkSlot,
}: CoverWithControlsProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  return (
    <div
      ref={ref}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className="relative aspect-square w-full max-w-65 rounded-2xl shadow-2xl overflow-hidden"
    >
      {artworkSlot}
      {/* Dimming layer + control bar fade in on hover. */}
      <div
        className={`absolute inset-0 flex items-center justify-center transition-opacity duration-150 ${
          showControls ? "opacity-100 bg-black/40" : "opacity-0"
        }`}
      >
        <div className="flex items-center gap-2">
          <IconButton
            onClick={onToggleShuffle}
            label="shuffle"
            active={isShuffled}
          >
            <Shuffle size={14} />
          </IconButton>
          <IconButton onClick={onPrev} label="previous">
            <SkipBack size={16} />
          </IconButton>
          <button
            type="button"
            onClick={onPlayPause}
            aria-label={isPlaying ? "pause" : "play"}
            className="w-11 h-11 rounded-full bg-white text-black flex items-center justify-center hover:scale-105 transition-transform"
          >
            {isPlaying ? (
              <Pause size={18} className="fill-current" />
            ) : (
              <Play size={18} className="fill-current ml-0.5" />
            )}
          </button>
          <IconButton onClick={onNext} label="next">
            <SkipForward size={16} />
          </IconButton>
          <IconButton
            onClick={onCycleRepeat}
            label="repeat"
            active={repeatMode !== "off"}
          >
            {repeatMode === "one" ? <Repeat1 size={14} /> : <Repeat size={14} />}
          </IconButton>
        </div>
      </div>
    </div>
  );
}

function IconButton({
  onClick,
  label,
  active,
  children,
}: {
  onClick: () => void;
  label: string;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      className={`p-2 rounded-full transition-colors ${
        active
          ? "text-emerald-400 hover:bg-white/10"
          : "text-white/80 hover:text-white hover:bg-white/10"
      }`}
    >
      {children}
    </button>
  );
}
