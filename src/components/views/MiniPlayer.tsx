import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Pause,
  SkipBack,
  SkipForward,
  Heart,
  Star,
  Maximize2,
  X,
  Pin,
  Repeat,
  Repeat1,
  Shuffle,
  ListMusic,
  Mic2,
  Radio,
  Volume1,
  Volume2,
  VolumeX,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlayer } from "../../hooks/usePlayer";
import { useLikedTracks } from "../../hooks/useLikedTracks";
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import { useKaraokeWordFill } from "../../hooks/useKaraokeWordFill";
import { useLyricsHighlightColor } from "../../hooks/useLyricsHighlightColor";
import { useSlideshowLayer } from "../../hooks/useSlideshowLayer";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import {
  isRadioTrack,
  isRemoteTrack,
  isStreamTrack,
} from "../../lib/playerSources";
import { Artwork } from "../common/Artwork";
import { CoverSlideshow } from "../player/CoverSlideshow";
import { CanvasStage } from "../player/CanvasStage";
import { MotionCoverOverlay } from "../player/MotionCoverOverlay";
import { useTrackCanvas } from "../../hooks/useTrackCanvas";
import {
  setCanvasEnabled,
  useCanvasEnabled,
} from "../../hooks/useCanvasEnabled";
import { CanvasToggleButton } from "../player/CanvasToggleButton";
import { useAlbumMotionArtwork } from "../../hooks/useAlbumMotionArtwork";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { dominantColor, darken, rgb } from "../../lib/dominantColor";
import { formatDuration } from "../../lib/tauri/track";
import { setMiniPlayerBounds } from "../../lib/tauri/preferences";
import {
  playerGetQueue,
  playerJumpToIndex,
  type PlayerQueueSnapshot,
} from "../../lib/tauri/player";
import {
  remoteGetPlayQueue,
  remoteQueueJump,
  type RemotePlayQueue,
} from "../../lib/tauri/remoteServer";

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
    currentRadioStation,
    volume,
    setVolume,
    toggleMute,
  } = usePlayer();
  // Live radio has no seekable timeline — the seek bar + timestamps are
  // hidden (matching the PlayerBar / immersive ProgressBar).
  const isRadio = isRadioTrack(currentTrack);

  // Web Radio favorites — a live stream swaps the ♥ for a station ★.
  const radioFavorites = useWebRadioFavorites();
  const stationFavorited =
    currentRadioStation != null &&
    radioFavorites.isFavorite(currentRadioStation.id);

  // ── Like state (own webview, kept in step with the main window by
  //    the hook's `track:liked-changed` subscription, #523) ─────────
  const { likedIds, toggleLike } = useLikedTracks(currentTrack?.id);
  const isLiked = currentTrack ? likedIds.has(currentTrack.id) : false;
  const handleLike = () => {
    if (currentTrack) void toggleLike(currentTrack.id);
  };

  // ── Content overlays (one slot, not a flag each) ────────────────
  // Up-next and lyrics both cover the whole content area, so two
  // independent booleans would let them stack. Mirrors how
  // `PlayerContext` mutexes the main window's three right-edge panels.
  const [overlay, setOverlay] = useState<MiniOverlay>("none");
  const showQueue = overlay === "queue";

  // ── Up-next queue (own webview = own fetch + event subscription) ─
  // Two sources, because a remote session's queue is not in the local
  // `queue_item` table — it lives in memory on the backend. That is the
  // same split `QueuePanel` makes between its own body and
  // `RemoteQueueView`, and without it this list showed whatever local
  // queue happened to be sitting there while a remote track played
  // (#685). Only one is live at a time.
  const isRemoteSession = isRemoteTrack(currentTrack);

  // Local: load once, refetch on `player:queue-changed`, guarded by a
  // seq counter so overlapping refetches (rapid Next) never resolve out
  // of order.
  const [queue, setQueue] = useState<PlayerQueueSnapshot | null>(null);
  const queueSeqRef = useRef(0);

  const fetchQueue = useCallback(() => {
    const seq = ++queueSeqRef.current;
    playerGetQueue()
      .then((q) => {
        if (seq === queueSeqRef.current) setQueue(q);
      })
      .catch((err) => {
        console.error("[MiniPlayer] queue fetch failed", err);
        if (seq === queueSeqRef.current) setQueue(null);
      });
  }, []);

  useEffect(() => {
    if (isRemoteSession) return;
    fetchQueue();
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const fn = await listen("player:queue-changed", fetchQueue);
        // Cleanup may have run before `listen()` resolved — tear the
        // subscription down right away so it doesn't leak past unmount.
        if (cancelled) fn();
        else unlisten = fn;
      } catch (err) {
        console.error("[MiniPlayer] queue listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [fetchQueue, isRemoteSession]);

  // Remote: there is no `player:queue-changed` for it, and
  // `RemoteQueueView` does not invent one — it refetches when the playing
  // track's id changes, because an advance or a jump moves the backend
  // cursor and flips the negative sentinel id with it. Same signal here,
  // so the two surfaces stay in step.
  const [remoteQueue, setRemoteQueue] = useState<RemotePlayQueue | null>(null);
  const remoteSeqRef = useRef(0);
  const currentTrackId = currentTrack?.id ?? null;

  // Drop the previous session's entries when this one ends. Without
  // this, a new remote session renders — and can be clicked into — with
  // the LAST session's queue for the frames before its own fetch lands,
  // and `remote_queue_jump` would take an index from the wrong list. It
  // is a cleanup rather than a call in the body on purpose: this has to
  // happen on the way out of a session, not on every track change
  // inside one, which would blank the list on each advance.
  useEffect(() => {
    if (!isRemoteSession) return;
    return () => setRemoteQueue(null);
  }, [isRemoteSession]);

  useEffect(() => {
    if (!isRemoteSession) return;
    // `currentTrackId` is a dependency on purpose — it is the change
    // signal, not a value this effect reads.
    void currentTrackId;
    // Two guards, because they answer different questions. The seq drops
    // a fetch that a LATER one has overtaken; `ended` drops one whose
    // session is over — clearing the state above does not stop a request
    // already in flight, and without this it would land afterwards and
    // put the dead session's queue back.
    let ended = false;
    const seq = ++remoteSeqRef.current;
    remoteGetPlayQueue()
      .then((q) => {
        if (!ended && seq === remoteSeqRef.current) setRemoteQueue(q);
      })
      .catch((err) => {
        console.error("[MiniPlayer] remote queue fetch failed", err);
        if (!ended && seq === remoteSeqRef.current) setRemoteQueue(null);
      });
    return () => {
      ended = true;
    };
  }, [isRemoteSession, currentTrackId]);

  const currentIndex = isRemoteSession
    ? (remoteQueue?.index ?? -1)
    : (queue?.current_index ?? -1);

  // One row shape for both sources, so the list below renders once. A
  // remote entry can arrive before its metadata does, which is what
  // `remote.common.awaitingMetadata` is for.
  const upNext = useMemo(() => {
    const from = Math.max(0, currentIndex + 1);
    if (isRemoteSession) {
      if (!remoteQueue) return [];
      return remoteQueue.entries.slice(from).map((entry, i) => ({
        key: `${entry.id}:${from + i}`,
        absoluteIndex: from + i,
        title: entry.title ?? t("remote.common.awaitingMetadata"),
        artist: entry.artist,
      }));
    }
    if (!queue) return [];
    return queue.items.slice(from).map((item, i) => ({
      key: String(from + i),
      absoluteIndex: from + i,
      title: item.title,
      artist: item.artist_name,
    }));
  }, [isRemoteSession, remoteQueue, queue, currentIndex, t]);

  const handleJump = useCallback(
    (absoluteIndex: number) => {
      // The remote queue has its own cursor; `player_jump_to_index` acts
      // on the local one and would end the session on an unrelated track.
      const jump = isRemoteSession
        ? remoteQueueJump(absoluteIndex)
        : playerJumpToIndex(absoluteIndex);
      jump.catch((err) => console.error("[MiniPlayer] jump failed", err));
    },
    [isRemoteSession],
  );

  // ── Canvas and motion cover (issue #717) ──────────────
  // The same chain as the Now Playing panel and the immersive view:
  // Canvas > motion cover > slideshow > still cover. This window used to
  // stop at the slideshow, which made the gap more visible, not less.
  //
  // It is a second video decode while the main window may be playing the
  // same clip — the cost of showing it here at all. The Show Canvas
  // toggle and reduced motion gate it exactly as they do elsewhere, and
  // the clip is cropped into the square slot: at this size a tall frame
  // would push the controls out of the window.
  const canvasEnabled = useCanvasEnabled();
  const reducedMotion = usePrefersReducedMotion();
  const canvasPath = useTrackCanvas(currentTrack);
  // A closed mini-player is only hidden (see the close handler), so its
  // clips are taken down while it is parked instead of decoding for a
  // window nobody sees, and come back when it is shown and focused.
  const [parked, setParked] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) setParked(false);
      })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch((err) => {
        console.error("[MiniPlayer] focus listener failed", err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  const clipsOn = !parked;
  const canvasActive =
    clipsOn && canvasEnabled && !reducedMotion && !!canvasPath;
  const motionCover = useAlbumMotionArtwork(
    currentTrack?.artist_name,
    currentTrack?.album_title,
    currentTrack?.album_id,
  );

  // ── Cover ↔ artist slideshow (issue #702) ──────────────
  // The window people leave on screen while doing something else is
  // arguably where a slowly alternating cover is nicest — a beta
  // tester asked for it (#700). One rung below the clips above.
  //
  // The photo is resolved from this webview, which means a second
  // `enrich_artist_deezer` for the same artist while the main window
  // shows one too — a cache hit against `app.metadata_artist`, not a
  // second network fetch. It costs one IPC round-trip per artist
  // change, and only for a profile that turned the slideshow on.
  const slideshow = useSlideshowLayer(currentTrack, {
    blocked: canvasActive || !!motionCover,
  });

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

  // ── Persist window bounds (position + size) ─────────────────────
  // Debounced because onMoved / onResized fire continuously while the
  // user drags or resizes — without this we'd hammer SQLite at 60 Hz.
  // 300 ms after the last gesture is short enough that closing the
  // window with Alt-F4 still captures the final position.
  useEffect(() => {
    const win = getCurrentWindow();
    let timer: number | null = null;
    let unlistenMoved: (() => void) | null = null;
    let unlistenResized: (() => void) | null = null;

    const scheduleSave = () => {
      if (timer != null) window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        try {
          const scale = await win.scaleFactor();
          const pos = await win.outerPosition();
          const size = await win.outerSize();
          await setMiniPlayerBounds({
            x: pos.x / scale,
            y: pos.y / scale,
            width: size.width / scale,
            height: size.height / scale,
          });
        } catch (err) {
          console.error("[MiniPlayer] persist bounds failed", err);
        }
      }, 300);
    };

    win
      .onMoved(scheduleSave)
      .then((fn) => {
        unlistenMoved = fn;
      })
      .catch((err) => console.error("[MiniPlayer] onMoved listen failed", err));
    win
      .onResized(scheduleSave)
      .then((fn) => {
        unlistenResized = fn;
      })
      .catch((err) =>
        console.error("[MiniPlayer] onResized listen failed", err),
      );

    return () => {
      if (timer != null) window.clearTimeout(timer);
      unlistenMoved?.();
      unlistenResized?.();
    };
  }, []);

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

  // Both hide this window rather than close it: `openMiniPlayer` shows
  // the same one again. A mini-player destroyed and re-created under the
  // same label received its first replies on the old webview's dead
  // handle, and opened on "No track playing" after a track change.
  const handleMaximize = async () => {
    try {
      const main = await TauriWindow.getByLabel("main");
      if (main) {
        await main.show();
        await main.unminimize();
        await main.setFocus();
      }
      setParked(true);
      await getCurrentWindow().hide();
    } catch (err) {
      console.error("[MiniPlayer] maximize failed", err);
    }
  };

  const handleClose = async () => {
    try {
      const main = await TauriWindow.getByLabel("main");
      if (main) await main.show();
      setParked(true);
      await getCurrentWindow().hide();
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

  // The up-next overlay visually covers the cover / title / seek controls
  // — mark that subtree inert so keyboard and screen-reader focus can't
  // reach the hidden buttons behind it. Lyrics are no longer in that list:
  // since #697 they are a mode of the player, not a sheet over it, and
  // everything around them stays usable.
  const contentInert = showQueue;

  return (
    <div
      // `wf-mini-player-surface` is the hook high contrast needs: the
      // background below is sampled from the cover, so a pale album
      // gives white text on a pale ground -- in the one mode whose
      // entire promise is that it will not. The class lets a stylesheet
      // replace it, which an inline style otherwise makes impossible.
      className="wf-mini-player-surface relative h-screen w-screen flex flex-col overflow-hidden text-white select-none"
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
          {/* The same Show Canvas toggle as the Now Playing panel, shown
              under the same condition: only when the track has a clip and
              motion is not reduced, so it is never a dead control. The
              preference is shared, so it flips in the main window too. */}
          {canvasPath && !reducedMotion && (
            <CanvasToggleButton
              enabled={canvasEnabled}
              onToggle={() => setCanvasEnabled(!canvasEnabled)}
              size={12}
              className={`p-1 rounded-full transition-colors ${
                canvasEnabled
                  ? "text-emerald-400 hover:bg-white/10"
                  : "text-white/60 hover:text-white hover:bg-white/10"
              }`}
            />
          )}
          <button
            type="button"
            onClick={() =>
              setOverlay((v) => (v === "lyrics" ? "none" : "lyrics"))
            }
            aria-label={t("lyrics.title")}
            title={t("lyrics.title")}
            aria-pressed={overlay === "lyrics"}
            className={`p-1 rounded-full transition-colors ${
              overlay === "lyrics"
                ? "text-emerald-400 hover:bg-white/10"
                : "text-white/60 hover:text-white hover:bg-white/10"
            }`}
          >
            <Mic2 size={12} />
          </button>
          <button
            type="button"
            onClick={() =>
              setOverlay((v) => (v === "queue" ? "none" : "queue"))
            }
            aria-label={t("miniPlayer.upNext.toggle")}
            title={t("miniPlayer.upNext.toggle")}
            aria-pressed={showQueue}
            className={`p-1 rounded-full transition-colors ${
              showQueue
                ? "text-emerald-400 hover:bg-white/10"
                : "text-white/60 hover:text-white hover:bg-white/10"
            }`}
          >
            <ListMusic size={12} />
          </button>
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

      {/* Content (cover or lyrics, then title + seek). Inert while the
          up-next overlay is open so focus can't reach the controls behind
          it; the top bar above stays interactive. */}
      <div
        className="flex-1 flex flex-col min-h-0"
        inert={contentInert}
        aria-hidden={contentInert || undefined}
      >
        {/* Lyrics take the cover's slot rather than covering the whole
            widget (#697): the title, the seek bar and the transport row
            below stay reachable, so pausing or skipping no longer means
            closing the lyrics first. */}
        {overlay === "lyrics" ? (
          <div className="flex-1 min-h-0 px-3 pt-1 pb-2">
            <MiniLyricsStage artworkUrl={artworkUrl} />
          </div>
        ) : (
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
              shuffleDisabled={isRemoteTrack(currentTrack)}
              volume={volume}
              onSetVolume={setVolume}
              onToggleMute={toggleMute}
              artworkSlot={
                currentTrack ? (
                  <>
                    <Artwork
                      path={currentTrack.artwork_path}
                      path1x={currentTrack.artwork_path_1x}
                      path2x={currentTrack.artwork_path_2x}
                      size="full"
                      alt={currentTrack.title}
                      className="w-full h-full object-cover"
                      rounded="xl"
                    />
                    {/* Siblings of the cover inside the slot's own
                        `relative` box, so the clips and the crossfade
                        land under the hover controls rather than over
                        them. */}
                    {clipsOn && !canvasActive && (
                      <MotionCoverOverlay
                        artist={currentTrack.artist_name}
                        album={currentTrack.album_title}
                        albumId={currentTrack.album_id}
                        rounded="xl"
                      />
                    )}
                    <CanvasStage
                      path={canvasPath}
                      enabled={clipsOn && canvasEnabled && !reducedMotion}
                      rounded="xl"
                    />
                    <CoverSlideshow
                      artistSrc={slideshow.artistSrc}
                      enabled={slideshow.active}
                      rounded="xl"
                    />
                  </>
                ) : (
                  <div className="w-full h-full rounded-2xl bg-white/10 flex items-center justify-center">
                    <Play size={48} className="text-white/40" />
                  </div>
                )
              }
            />
          </div>
        )}

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
            {/* Live radio: favorite the STATION (★). Otherwise the
              local-library like (♥) — streamed tracks are excluded
              because they have no WaveFlow DB row to like. */}
            {currentRadioStation ? (
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
                className="p-0.5 shrink-0"
              >
                <Star
                  size={14}
                  fill={stationFavorited ? "currentColor" : "none"}
                  className={
                    stationFavorited
                      ? "text-amber-400"
                      : "text-white/60 hover:text-white"
                  }
                />
              </button>
            ) : currentTrack && !isStreamTrack(currentTrack) ? (
              // Guard the radio sentinel track (negative id) during the
              // hydration race / idle tail — no ♥ like without a library
              // row. `currentTrack &&` also drops the disabled ♥ when
              // nothing is playing (idle), matching the PlayerBar.
              <button
                type="button"
                onClick={handleLike}
                aria-label={t("miniPlayer.like")}
                aria-pressed={isLiked}
                className="p-0.5 shrink-0"
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
            ) : null}
          </div>
          {/* Live radio: station identity under the now-playing ICY song
              (title/artist rows above), matching the PlayerBar + immersive. */}
          {currentRadioStation && (
            <div
              className="flex items-center gap-1 text-[10px] text-white/55 truncate mt-0.5"
              title={
                currentRadioStation.artist
                  ? `${currentRadioStation.title} · ${currentRadioStation.artist}`
                  : currentRadioStation.title
              }
            >
              <Radio size={10} className="shrink-0" />
              <span className="truncate">
                {currentRadioStation.artist
                  ? `${currentRadioStation.title} · ${currentRadioStation.artist}`
                  : currentRadioStation.title}
              </span>
            </div>
          )}
        </div>

        {/* Interactive seek bar — Spotify-style: thin idle, thicker
          on hover with timestamps revealed at both ends. Hidden for live
          radio (no seekable timeline). */}
        {!isRadio && (
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
        )}

        {/* Transport, visible only in lyrics mode. The usual controls live
            on the cover and appear on hover; with the cover gone there has
            to be something to press, and a row that only exists in this
            mode keeps the cover view exactly as it was. */}
        {overlay === "lyrics" && (
          <div className="flex items-center justify-center gap-4 px-3 pb-2">
            <IconButton
              onClick={previous}
              label={t("player.controls.previous")}
            >
              <SkipBack size={16} />
            </IconButton>
            <button
              type="button"
              onClick={togglePlayback}
              aria-label={
                isPlaying
                  ? t("player.controls.pause")
                  : t("player.controls.play")
              }
              className="flex h-9 w-9 items-center justify-center rounded-full bg-white text-black transition-transform hover:scale-105"
            >
              {isPlaying ? (
                <Pause size={16} className="fill-current" />
              ) : (
                <Play size={16} className="fill-current ml-0.5" />
              )}
            </button>
            <IconButton onClick={next} label={t("player.controls.next")}>
              <SkipForward size={16} />
            </IconButton>
          </div>
        )}
      </div>

      {/* Up-next overlay — slides over the content area below the top
          bar (which stays reachable so the toggle/close still work).
          Gated with the toggle button above. */}
      {showQueue && (
        <div className="absolute inset-x-0 bottom-0 top-7 z-20 flex flex-col bg-black/55 backdrop-blur-md animate-fade-in wf-glass wf-mini-player-surface">
          <div className="flex items-center justify-between px-3 py-2 shrink-0">
            <span className="text-[10px] font-bold uppercase tracking-widest text-white/70">
              {t("miniPlayer.upNext.title", { count: upNext.length })}
            </span>
            <button
              type="button"
              onClick={() => setOverlay("none")}
              aria-label={t("common.close")}
              className="p-1 -mr-1 rounded-full text-white/60 hover:text-white hover:bg-white/10 transition-colors"
            >
              <X size={13} />
            </button>
          </div>
          {upNext.length === 0 ? (
            <div className="flex-1 flex items-center justify-center px-4 text-center text-[11px] text-white/50">
              {t("miniPlayer.upNext.empty")}
            </div>
          ) : (
            <div className="flex-1 overflow-y-auto scrollbar-hide px-2 pb-2 space-y-0.5">
              {upNext.map((row) => (
                <button
                  key={row.key}
                  type="button"
                  onClick={() => handleJump(row.absoluteIndex)}
                  title={`${row.title} — ${row.artist ?? ""}`}
                  className="w-full flex items-center gap-2 px-2 py-1.5 rounded-lg text-left hover:bg-white/10 transition-colors"
                >
                  <span className="w-4 shrink-0 text-right text-[10px] tabular-nums text-white/40">
                    {row.absoluteIndex - currentIndex}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-xs text-white">
                      {row.title}
                    </div>
                    <div className="truncate text-[10px] text-white/60">
                      {row.artist ?? "—"}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The mini-player's content area holds one full-cover overlay at a time. */
type MiniOverlay = "none" | "queue" | "lyrics";

/** Fades the first and last lines out instead of slicing them in half. */
const LYRICS_MASK =
  "linear-gradient(to bottom, transparent 0%, #000 20%, #000 80%, transparent 100%)";

/**
 * Lyrics inside the mini-player (issue #580), reworked into a mode of the
 * player rather than a sheet over it (#697).
 *
 * **Mounted only while open, and that is the point.** `useTrackLyrics`
 * fetches on every track change, so keeping it mounted behind a closed
 * overlay would fire a second `fetch_lyrics` per track from this webview
 * on top of the main window's. Unmounting means the cost lands only when
 * the user actually asked for lyrics, and the backend cache absorbs the
 * overlap when both surfaces are open at once. The rework kept that: this
 * takes the cover's slot, it does not sit mounted behind it.
 *
 * **Few lines, one obvious.** At a uniform 12 px the active line differed
 * from its neighbours by a font weight, which at that size the eye cannot
 * find. The current line is now noticeably larger and its neighbours
 * smaller and dimmer, so the word-level fill (`useKaraokeWordFill`) is
 * finally legible — it was always wired in, just invisible.
 *
 * **Centred**, like the desktop lyrics window (#582): both are small
 * glanceable surfaces rather than reading columns, and the two now agree.
 * The side panel and the immersive column stay left-aligned.
 *
 * The window defaults to 280x380 and can be dragged down to 240x320, so
 * there is no room for the side panel's source label, provider picker or
 * the import / refetch / clear actions. Those stay in the main window —
 * this is a reading surface, not an editing one. There is no header row
 * either: the toggle in the top bar already shows which mode is open, and
 * closing is one click on the same button.
 */
function MiniLyricsStage({ artworkUrl }: { artworkUrl: string | null }) {
  const { t } = useTranslation();
  const { currentTrack } = usePlayer();
  const reducedMotion = usePrefersReducedMotion();
  const {
    payload,
    isFetching,
    error,
    lrcLines,
    isSynced,
    radioPlainText,
    activeIndex,
    activeWordIndex,
    seekToLine,
  } = useTrackLyrics();
  // The colour for the line being sung, chosen in the main window's
  // Settings (#751) — the cross-window bridge (#741) brings a change here
  // without reopening the mini-player.
  const { color: highlightColor } = useLyricsHighlightColor();

  // Auto-scroll is view-local by the hook's contract: it owns
  // `activeIndex`, each consumer scrolls its own nodes. This one has its
  // own scroller, so it needs its own ref array.
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  useEffect(() => {
    if (!isSynced || activeIndex < 0) return;
    lineRefs.current[activeIndex]?.scrollIntoView({
      // A reader who asked for less motion gets the jump, not the glide.
      behavior: reducedMotion ? "auto" : "smooth",
      block: "center",
    });
  }, [activeIndex, isSynced, reducedMotion]);

  // Progressive word fill on the active word only — the same hook the
  // immersive column uses, so the sweep stays continuous between the
  // 4 Hz `player:position` events instead of stepping every 250 ms.
  const wordFillRef = useKaraokeWordFill(
    isSynced ? lrcLines[activeIndex]?.words?.[activeWordIndex] : undefined,
  );

  // Plain text covers three cases that all render the same way: an
  // unsynced payload, a radio session (whose position can't align to a
  // song joined mid-play, so the hook hands back a timestamp-stripped
  // read), and a synced payload we failed to parse into lines.
  const plainText =
    radioPlainText ?? (isSynced ? null : (payload?.content ?? null));

  const message = (text: string) => (
    <div className="flex h-full items-center justify-center px-4 text-center text-[11px] text-white/60">
      {text}
    </div>
  );

  return (
    <div className="relative h-full w-full overflow-hidden rounded-xl bg-black/30">
      {/* The cover this replaced, blurred behind the words, so the surface
          keeps the colour the window's gradient was sampled from instead of
          going to a flat black sheet. Decorative, and dropped in high
          contrast for the same reason the gradient is: this window's
          legibility must not be decided by the user's music. */}
      {artworkUrl && (
        <img
          src={artworkUrl}
          alt=""
          aria-hidden="true"
          className="wf-mini-lyrics-wash pointer-events-none absolute inset-0 h-full w-full scale-125 object-cover opacity-40 blur-xl"
        />
      )}
      <div className="pointer-events-none absolute inset-0 bg-black/35" />

      <div className="relative h-full">
        {currentTrack == null ? (
          message(t("lyrics.noTrack"))
        ) : isFetching && !payload ? (
          message(t("lyrics.loading"))
        ) : error ? (
          message(t("lyrics.fetchError"))
        ) : isSynced && lrcLines.length > 0 ? (
          <ul
            className="h-full overflow-y-auto scrollbar-hide px-3 space-y-2"
            style={{ maskImage: LYRICS_MASK, WebkitMaskImage: LYRICS_MASK }}
          >
            {/* Half-height spacers so the first and last lines can sit in the
              centre like any other. A fixed padding would be a guess about
              a window height the user can drag. */}
            <li aria-hidden="true" className="pointer-events-none h-1/2" />
            {lrcLines.map((line, index) => {
              const isActive = index === activeIndex;
              const isPast = activeIndex >= 0 && index < activeIndex;
              const hasWords = isActive && (line.words?.length ?? 0) > 0;
              return (
                <li
                  key={`${line.timeMs}-${index}`}
                  ref={(el) => {
                    lineRefs.current[index] = el;
                  }}
                >
                  <button
                    type="button"
                    onClick={() => seekToLine(line)}
                    className={`block w-full rounded text-center leading-snug transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-white/70 ${
                      isActive
                        ? "text-[15px] font-semibold text-white"
                        : isPast
                          ? "text-[11px] text-white/30"
                          : "text-[11px] text-white/55 hover:text-white/85"
                    } ${isActive && highlightColor ? "wf-lyrics-highlight" : ""}`}
                    style={
                      isActive && highlightColor
                        ? { color: highlightColor }
                        : undefined
                    }
                  >
                    {hasWords ? (
                      <span>
                        {line.words!.map((word, wi) => {
                          const isActiveWord = wi === activeWordIndex;
                          // A literal space between boxes: `inline-block`
                          // strips the JSX whitespace, and many Enhanced
                          // LRC sources omit spaces between word stamps.
                          return (
                            <Fragment key={wi}>
                              <span className="karaoke-word">
                                {/* Opacity lives on the layers, never on
                                  the box — a parent's opacity applies to
                                  its whole subtree, so dimming the box
                                  would dim the sung overlay with it and
                                  no fill could ever read as brighter. */}
                                <span
                                  style={{
                                    opacity:
                                      wi < activeWordIndex
                                        ? 0.8
                                        : isActiveWord
                                          ? 0.5
                                          : 0.45,
                                    transition: "opacity 150ms ease",
                                  }}
                                >
                                  {word.text}
                                </span>
                                {isActiveWord && (
                                  // `aria-hidden` because the base layer
                                  // already carries the text — without it
                                  // a screen reader reads the word twice.
                                  <span
                                    ref={wordFillRef}
                                    aria-hidden="true"
                                    className="karaoke-word__fill"
                                  >
                                    {word.text}
                                  </span>
                                )}
                              </span>
                              {wi < line.words!.length - 1 && " "}
                            </Fragment>
                          );
                        })}
                      </span>
                    ) : (
                      line.text || " "
                    )}
                  </button>
                </li>
              );
            })}
            <li aria-hidden="true" className="pointer-events-none h-1/2" />
          </ul>
        ) : plainText ? (
          <div
            className="h-full overflow-y-auto scrollbar-hide px-3 py-3"
            style={{ maskImage: LYRICS_MASK, WebkitMaskImage: LYRICS_MASK }}
          >
            <p className="text-center text-xs leading-relaxed text-white/80 whitespace-pre-line">
              {plainText}
            </p>
          </div>
        ) : (
          message(t("miniPlayer.lyrics.empty"))
        )}
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
  /** Remote-queue tracks have no shuffle (matches PlaybackControls). */
  shuffleDisabled: boolean;
  volume: number;
  onSetVolume: (value: number) => void;
  onToggleMute: () => void;
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
  shuffleDisabled,
  volume,
  onSetVolume,
  onToggleMute,
  artworkSlot,
}: CoverWithControlsProps) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement | null>(null);
  return (
    <div
      ref={ref}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      // The overlay is hover-revealed, but its controls stay in the tab
      // order — a keyboard user would otherwise be operating buttons and
      // a volume slider they can't see. Focus reveals it too, and a focus
      // leaving the subtree entirely hides it again.
      onFocusCapture={onMouseEnter}
      onBlurCapture={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) onMouseLeave();
      }}
      className="relative aspect-square w-full max-w-64 rounded-xl shadow-2xl overflow-hidden"
    >
      {artworkSlot}
      {/* Dimming layer + control bar fade in on hover. */}
      <div
        className={`absolute inset-0 flex flex-col items-center justify-center gap-3 transition-opacity duration-150 ${
          showControls ? "opacity-100 bg-black/40" : "opacity-0"
        }`}
      >
        <div className="flex items-center gap-2">
          <IconButton
            onClick={onToggleShuffle}
            label={t(
              isShuffled
                ? "player.controls.shuffleTracks"
                : "player.controls.shuffleOff",
            )}
            active={isShuffled}
            disabled={shuffleDisabled}
          >
            <Shuffle size={14} />
          </IconButton>
          <IconButton onClick={onPrev} label={t("player.controls.previous")}>
            <SkipBack size={16} />
          </IconButton>
          <button
            type="button"
            onClick={onPlayPause}
            aria-label={
              isPlaying ? t("player.controls.pause") : t("player.controls.play")
            }
            className="w-11 h-11 rounded-full bg-white text-black flex items-center justify-center hover:scale-105 transition-transform"
          >
            {isPlaying ? (
              <Pause size={18} className="fill-current" />
            ) : (
              <Play size={18} className="fill-current ml-0.5" />
            )}
          </button>
          <IconButton onClick={onNext} label={t("player.controls.next")}>
            <SkipForward size={16} />
          </IconButton>
          <IconButton
            onClick={onCycleRepeat}
            label={t(
              repeatMode === "one"
                ? "player.controls.repeatOne"
                : repeatMode === "all"
                  ? "player.controls.repeatAll"
                  : "player.controls.repeatOff",
            )}
            active={repeatMode !== "off"}
          >
            {repeatMode === "one" ? (
              <Repeat1 size={14} />
            ) : (
              <Repeat size={14} />
            )}
          </IconButton>
        </div>
        <MiniVolume
          volume={volume}
          onSetVolume={onSetVolume}
          onToggleMute={onToggleMute}
        />
      </div>
    </div>
  );
}

/** Volume step for the wheel and the arrow keys, matching the
 *  PlayerBar's [`VolumeControl`](../player/VolumeControl.tsx). */
const VOLUME_STEP = 5;

/**
 * Compact volume slider + mute for the cover overlay (#511).
 *
 * Same interaction contract as the PlayerBar control — pointer drag,
 * wheel, arrows / Home / End — restyled for the widget's translucent
 * palette, and sitting under the transport row so the idle state stays
 * "just the artwork". Volume itself is engine-wide: the backend echoes
 * every change on `player:volume-changed`, so this slider and the main
 * window's stay in step.
 */
function MiniVolume({
  volume,
  onSetVolume,
  onToggleMute,
}: {
  volume: number;
  onSetVolume: (value: number) => void;
  onToggleMute: () => void;
}) {
  const { t } = useTranslation();
  const trackRef = useRef<HTMLDivElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);

  // React attaches `wheel` passively at the root, so a JSX `onWheel`
  // can't `preventDefault`. Bind directly to keep the gesture from
  // scrolling anything behind the widget.
  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      if (e.deltaY === 0) return;
      e.preventDefault();
      onSetVolume(volume + (e.deltaY < 0 ? VOLUME_STEP : -VOLUME_STEP));
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [volume, onSetVolume]);

  const updateFromClientX = (clientX: number) => {
    const el = trackRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0) return;
    onSetVolume(((clientX - rect.left) / rect.width) * 100);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    // Suppress the WebView's image/text drag fallback, which otherwise
    // hijacks the pointer stream mid-drag (same reason as VolumeControl).
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    updateFromClientX(e.clientX);
  };
  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
    updateFromClientX(e.clientX);
  };
  const handlePointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  };

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    switch (e.key) {
      case "ArrowLeft":
      case "ArrowDown":
        e.preventDefault();
        onSetVolume(volume - VOLUME_STEP);
        break;
      case "ArrowRight":
      case "ArrowUp":
        e.preventDefault();
        onSetVolume(volume + VOLUME_STEP);
        break;
      case "Home":
        e.preventDefault();
        onSetVolume(0);
        break;
      case "End":
        e.preventDefault();
        onSetVolume(100);
        break;
    }
  };

  const Icon = volume === 0 ? VolumeX : volume < 50 ? Volume1 : Volume2;

  return (
    <div ref={hostRef} className="flex items-center gap-2 w-2/3 max-w-40">
      <button
        type="button"
        onClick={onToggleMute}
        aria-label={
          volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")
        }
        title={
          volume === 0 ? t("player.volume.unmute") : t("player.volume.mute")
        }
        className="p-1 -m-1 shrink-0 rounded-full text-white/80 hover:text-white transition-colors"
      >
        <Icon size={14} />
      </button>
      <div
        ref={trackRef}
        role="slider"
        tabIndex={0}
        aria-label={t("player.volume.label")}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={volume}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
        onDragStart={(e) => e.preventDefault()}
        onKeyDown={handleKeyDown}
        className="group flex-1 flex items-center h-5 cursor-pointer touch-none select-none rounded-full focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
      >
        <div className="relative w-full h-1 rounded-full bg-white/25">
          <div
            className="h-full rounded-full bg-white"
            style={{ width: `${volume}%` }}
          />
          <div
            className="absolute top-1/2 w-2.5 h-2.5 rounded-full bg-white shadow -translate-y-1/2 -translate-x-1/2 opacity-0 group-hover:opacity-100 transition-opacity"
            style={{ left: `${volume}%` }}
          />
        </div>
      </div>
    </div>
  );
}

function IconButton({
  onClick,
  label,
  active,
  disabled,
  children,
}: {
  onClick: () => void;
  label: string;
  active?: boolean;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      className={`p-2 rounded-full transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
        active
          ? "text-emerald-400 hover:bg-white/10"
          : "text-white/80 hover:text-white hover:bg-white/10"
      }`}
    >
      {children}
    </button>
  );
}
