import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
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
import { listLikedTrackIds, toggleLikeTrack } from "../../lib/tauri/track";
import { formatDuration } from "../../lib/tauri/track";

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
    seek,
    setSeeking,
    activeProvider,
  } = usePlayer();
  const isSpotify = activeProvider === "spotify";

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

  // ── Interactive seek bar ────────────────────────────────────────
  const [dragMs, setDragMs] = useState<number | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const positionFromPointer = useCallback(
    (clientX: number): number => {
      const el = trackRef.current;
      if (!el || durationMs <= 0) return 0;
      const rect = el.getBoundingClientRect();
      const ratio = Math.min(
        Math.max((clientX - rect.left) / rect.width, 0),
        1,
      );
      return Math.round(ratio * durationMs);
    },
    [durationMs],
  );
  const handleSeekDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!currentTrack || durationMs <= 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setSeeking(true);
    setDragMs(positionFromPointer(e.clientX));
  };
  const handleSeekMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (dragMs == null) return;
    setDragMs(positionFromPointer(e.clientX));
  };
  const handleSeekUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (dragMs == null) return;
    const target = dragMs;
    setDragMs(null);
    setSeeking(false);
    e.currentTarget.releasePointerCapture(e.pointerId);
    seek(target).catch(() => {});
  };
  const displayMs = dragMs ?? positionMs;
  const progressPct = durationMs > 0 ? (displayMs / durationMs) * 100 : 0;

  return (
    <div
      className="h-screen w-screen flex flex-col overflow-hidden text-white select-none"
      style={{ background: gradient }}
    >
      {/* Top bar. The middle dot strip is the OS-level drag region;
          everything else captures clicks normally. Splitting the
          drag region this way avoids buttons fighting the move
          gesture on Windows where data-tauri-drag-region on a
          button-bearing parent intermittently swallows clicks. */}
      <div className="flex items-stretch justify-between px-2 py-1 shrink-0">
        <button
          type="button"
          onClick={handleTogglePin}
          aria-label={t("miniPlayer.pin")}
          title={t("miniPlayer.pin")}
          className={`p-1 rounded-full transition-colors ${
            pinned
              ? "text-emerald-400 hover:bg-white/10"
              : "text-white/60 hover:text-white hover:bg-white/10"
          }`}
        >
          <Pin size={12} className={pinned ? "fill-current" : ""} />
        </button>
        <div
          data-tauri-drag-region
          onMouseDown={(e) => {
            // Belt-and-suspenders: data-tauri-drag-region only fires
            // when the EXACT mousedown target carries the attribute,
            // and pointer-events-none on children isn't enough on
            // every platform (notably Windows, where it can race
            // the OS hit-test). Calling startDragging explicitly
            // makes the gesture deterministic regardless.
            if (e.button !== 0) return;
            getCurrentWindow()
              .startDragging()
              .catch((err) =>
                console.error("[MiniPlayer] startDragging failed", err),
              );
          }}
          className="flex-1 flex items-center justify-center gap-0.5 text-white/40 cursor-grab active:cursor-grabbing"
        >
          {Array.from({ length: 6 }).map((_, i) => (
            <span
              key={i}
              className={`pointer-events-none block w-0.5 h-0.5 rounded-full bg-current${i === 3 ? " ml-1" : ""}`}
            />
          ))}
        </div>
        <div className="flex items-center gap-0.5">
          <button
            type="button"
            onClick={handleMaximize}
            aria-label={t("miniPlayer.maximize")}
            title={t("miniPlayer.maximize")}
            className="p-1 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors"
          >
            <Maximize2 size={12} />
          </button>
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("miniPlayer.close")}
            title={t("miniPlayer.close")}
            className="p-1 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors"
          >
            <X size={13} />
          </button>
        </div>
      </div>

      {/* Floating cover with hover overlay */}
      <div className="px-3 pt-1 pb-2 flex justify-center">
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
                rounded="xl"
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
      <div className="px-3 pb-1.5">
        <div
          className="text-sm font-semibold truncate leading-tight"
          title={currentTrack?.title}
        >
          {currentTrack?.title ?? t("miniPlayer.idle")}
        </div>
        <div className="flex items-center justify-between gap-2 mt-0.5">
          <div
            className="text-[11px] text-white/70 truncate"
            title={currentTrack?.artist_name ?? undefined}
          >
            {currentTrack?.artist_name ?? "—"}
          </div>
          {/* Like button is local-library only — Spotify tracks
              don't live in the WaveFlow DB so toggleLikeTrack would
              fail silently on a missing row. */}
          {!isSpotify && (
            <button
              type="button"
              onClick={handleLike}
              disabled={!currentTrack}
              aria-label={t("miniPlayer.like")}
              className="p-0.5 disabled:opacity-30 shrink-0"
            >
              <Heart
                size={14}
                className={
                  isLiked
                    ? "fill-emerald-400 text-emerald-400"
                    : "text-white/60 hover:text-white"
                }
              />
            </button>
          )}
        </div>
      </div>

      {/* Interactive seek bar — Spotify-style: thin idle, thicker
          on hover with timestamps revealed at both ends. */}
      <div className="mt-auto px-3 pb-2 group">
        <div
          ref={trackRef}
          onPointerDown={handleSeekDown}
          onPointerMove={handleSeekMove}
          onPointerUp={handleSeekUp}
          onPointerCancel={handleSeekUp}
          className={`relative h-1 rounded-full bg-white/20 ${currentTrack && durationMs > 0 ? "cursor-pointer" : "cursor-default"}`}
        >
          <div
            className="absolute inset-y-0 left-0 rounded-full bg-white"
            style={{ width: `${Math.min(100, progressPct)}%` }}
          />
          {currentTrack && durationMs > 0 && (
            <div
              className="absolute top-1/2 -translate-y-1/2 w-2.5 h-2.5 rounded-full bg-white shadow opacity-0 group-hover:opacity-100 transition-opacity"
              style={{ left: `calc(${Math.min(100, progressPct)}% - 5px)` }}
            />
          )}
        </div>
        <div className="flex justify-between text-[9px] text-white/60 tabular-nums mt-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <span>{formatDuration(displayMs)}</span>
          <span>{formatDuration(durationMs)}</span>
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
      className="relative aspect-square w-full max-w-64 rounded-xl shadow-2xl overflow-hidden"
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
            {repeatMode === "one" ? (
              <Repeat1 size={14} />
            ) : (
              <Repeat size={14} />
            )}
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
