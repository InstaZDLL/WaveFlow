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
  Star,
  Maximize2,
  X,
  Pin,
  ListMusic,
  Mic2,
  Radio,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Window as TauriWindow } from "@tauri-apps/api/window";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlayerPosition } from "../../hooks/usePlayerPosition";
import { useLikedTracks } from "../../hooks/useLikedTracks";
import { useSlideshowLayer } from "../../hooks/useSlideshowLayer";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import {
  isFileTrack,
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
import {
  setMiniPlayerBounds,
  syncWebviewMemory,
} from "../../lib/tauri/preferences";
import { resetMiniPlayerBounds } from "../../lib/miniPlayer";
import { displayBackendStatus } from "../../lib/tauri/displayBackend";
import {
  playerGetQueue,
  playerJumpToIndex,
  type PlayerQueueSnapshot,
} from "../../lib/tauri/player";
import { type RemotePlayQueue } from "../../lib/tauri/remoteServer";
import { DETACHED_QUEUES } from "../../lib/detachedQueues";
import { MiniLyricsStage } from "./mini/MiniLyricsStage";
import { CoverWithControls, IconButton } from "./mini/MiniControls";

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
  const positionMs = usePlayerPosition();
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
  // Files played without importing them have an in-memory queue of the
  // same shape, read and jumped through their own commands.
  const detachedSource = isRemoteTrack(currentTrack)
    ? "remote"
    : isFileTrack(currentTrack)
      ? "file"
      : null;
  const isRemoteSession = detachedSource !== null;

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
  // Keyed on the source, not just on there being one: going from a remote
  // session straight to files (or back) ends a session too.
  useEffect(() => {
    if (detachedSource === null) return;
    return () => setRemoteQueue(null);
  }, [detachedSource]);

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
    DETACHED_QUEUES[detachedSource ?? "remote"]
      .read()
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
  }, [isRemoteSession, detachedSource, currentTrackId]);

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
      const jump = detachedSource
        ? DETACHED_QUEUES[detachedSource].jump(absoluteIndex)
        : playerJumpToIndex(absoluteIndex);
      jump.catch((err) => console.error("[MiniPlayer] jump failed", err));
    },
    [detachedSource],
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
  // On a native Wayland session the compositor ignores the pin (#894):
  // the button says so, and where the fix lives, instead of seeming
  // broken.
  const [pinIgnored, setPinIgnored] = useState(false);
  const pinLabel = pinIgnored
    ? t("miniPlayer.pinWayland")
    : t("miniPlayer.pin");
  useEffect(() => {
    let cancelled = false;
    displayBackendStatus()
      .then((s) => {
        if (!cancelled) setPinIgnored(s.available && !s.active);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);
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
      void syncWebviewMemory().catch(() => {});
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
      void syncWebviewMemory().catch(() => {});
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
          aria-label={pinLabel}
          title={pinLabel}
          className={`p-1 rounded-full transition-colors ${
            pinned
              ? "text-emerald-400 hover:bg-white/10"
              : "text-white/60 hover:text-white hover:bg-white/10"
          }`}
        >
          <Pin size={12} className={pinned ? "fill-current" : ""} />
        </button>
        <div
          title={t("miniPlayer.dragHandle")}
          onMouseDown={(e) => {
            // The drag is started here rather than through
            // data-tauri-drag-region: that attribute only fires when
            // the EXACT mousedown target carries it, and
            // pointer-events-none on children isn't enough on every
            // platform (notably Windows, where it can race the OS
            // hit-test). It would also turn a double-click into a
            // maximise, which on a 280-pixel widget only ever meant a
            // player blown up to full screen; a double-click puts it
            // back to its default size and corner instead.
            if (e.button !== 0) return;
            if (e.detail === 2) {
              resetMiniPlayerBounds().catch((err) =>
                console.error("[MiniPlayer] reset bounds failed", err),
              );
              return;
            }
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
              shuffleDisabled={
                isRemoteTrack(currentTrack) || isFileTrack(currentTrack)
              }
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
