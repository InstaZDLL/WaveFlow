import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { PlayerContext, type PlaybackState, type RepeatMode } from "../hooks/usePlayer";
import type { Track } from "../lib/tauri/track";
import {
  playerGetState,
  playerNext,
  playerPause,
  playerPlayTracks,
  playerPrevious,
  playerResume,
  playerSeek,
  playerSetVolume,
  type PlayerErrorPayload,
  type PlayerPositionPayload,
  type PlayerStatePayload,
  type QueueSource,
} from "../lib/tauri/player";

/**
 * Provides the audio player state + actions to the whole React tree.
 *
 * The provider:
 * 1. fetches an initial snapshot from the backend at mount,
 * 2. listens for four Tauri events (`player:position`, `player:state`,
 *    `player:track-ended`, `player:error`) and reflects them in local
 *    React state,
 * 3. wraps `invoke()` calls behind convenient actions,
 * 4. debounces volume slider changes into the backend at 60 ms so a
 *    fast drag doesn't flood the command channel.
 */
export function PlayerProvider({ children }: { children: ReactNode }) {
  // UI-only state
  const [isQueueOpen, setIsQueueOpen] = useState(false);
  const [isDeviceMenuOpen, setIsDeviceMenuOpen] = useState(false);

  // Backend-synced state
  const [playbackState, setPlaybackState] = useState<PlaybackState>("idle");
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const [positionMs, setPositionMs] = useState(0);
  const [durationMs, setDurationMs] = useState(0);

  // Volume: local + debounced backend push
  const [volume, setVolumeState] = useState(80);
  const previousVolumeRef = useRef(80);
  const volumeDebounceRef = useRef<number | null>(null);

  // Shuffle / repeat — local for checkpoint 11, backend-wired in CP12
  const [isShuffled, setIsShuffled] = useState(false);
  const [repeatMode, setRepeatMode] = useState<RepeatMode>("off");

  // Suppress incoming position events while the user drags the
  // progress bar, so the thumb doesn't fight the mouse.
  const isSeekingRef = useRef(false);

  const isPlaying = playbackState === "playing";

  // --- initial snapshot ---
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const snap = await playerGetState();
        if (cancelled) return;
        setPlaybackState(snap.state);
        setPositionMs(snap.position_ms);
        // Volume arrives as 0..1 from the snapshot; UI uses 0..100.
        setVolumeState(Math.round(snap.volume * 100));
        previousVolumeRef.current = Math.round(snap.volume * 100);
      } catch (err) {
        console.error("[PlayerContext] initial snapshot failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // --- Tauri event listeners ---
  useEffect(() => {
    const unlisten: UnlistenFn[] = [];
    let cancelled = false;

    (async () => {
      try {
        unlisten.push(
          await listen<PlayerPositionPayload>("player:position", (e) => {
            if (!isSeekingRef.current) {
              setPositionMs(e.payload.ms);
            }
          })
        );
        unlisten.push(
          await listen<PlayerStatePayload>("player:state", (e) => {
            setPlaybackState(e.payload.state);
            if (e.payload.state === "ended") {
              // Keep currentTrack in view so the PlayerBar still
              // shows metadata until auto-advance swaps it.
            }
          })
        );
        unlisten.push(
          await listen<PlayerErrorPayload>("player:error", (e) => {
            console.error("[player:error]", e.payload.message);
          })
        );
      } catch (err) {
        console.error("[PlayerContext] listen setup failed", err);
      }
      if (cancelled) unlisten.forEach((u) => u());
    })();

    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, []);

  // --- Volume debounce ---
  const setVolume = useCallback((value: number) => {
    const clamped = Math.max(0, Math.min(100, Math.round(value)));
    setVolumeState(clamped);
    if (clamped > 0) previousVolumeRef.current = clamped;

    if (volumeDebounceRef.current != null) {
      window.clearTimeout(volumeDebounceRef.current);
    }
    volumeDebounceRef.current = window.setTimeout(() => {
      playerSetVolume(clamped / 100).catch((err) =>
        console.error("[PlayerContext] set volume failed", err)
      );
      volumeDebounceRef.current = null;
    }, 60);
  }, []);

  const toggleMute = useCallback(() => {
    setVolumeState((current) => {
      const next = current > 0 ? 0 : previousVolumeRef.current || 50;
      if (current > 0) previousVolumeRef.current = current;
      // Mute is immediate — no debounce.
      playerSetVolume(next / 100).catch((err) =>
        console.error("[PlayerContext] toggle mute failed", err)
      );
      return next;
    });
  }, []);

  // --- Tear down pending debounce on unmount ---
  useEffect(() => {
    return () => {
      if (volumeDebounceRef.current != null) {
        window.clearTimeout(volumeDebounceRef.current);
      }
    };
  }, []);

  // --- Backend actions ---
  const playTracks = useCallback(
    async (
      tracks: Track[],
      startIndex: number,
      source: { type: QueueSource; id: number | null }
    ) => {
      if (tracks.length === 0 || startIndex < 0 || startIndex >= tracks.length) {
        return;
      }
      const chosen = tracks[startIndex];
      // Optimistic UI: show the clicked track + duration immediately
      // so the PlayerBar doesn't lag the invoke round-trip.
      setCurrentTrack(chosen);
      setDurationMs(chosen.duration_ms);
      setPositionMs(0);
      setPlaybackState("loading");

      try {
        await playerPlayTracks(
          source.type,
          source.id,
          tracks.map((t) => t.id),
          startIndex
        );
      } catch (err) {
        console.error("[PlayerContext] play tracks failed", err);
        setPlaybackState("idle");
      }
    },
    []
  );

  const togglePlayback = useCallback(async () => {
    try {
      if (playbackState === "playing") {
        await playerPause();
      } else if (playbackState === "paused") {
        await playerResume();
      }
      // When state is idle/ended, there's nothing to resume — the
      // user must pick a track first. Checkpoint 13 will restore the
      // last-played track so this path becomes "resume from last".
    } catch (err) {
      console.error("[PlayerContext] toggle playback failed", err);
    }
  }, [playbackState]);

  const next = useCallback(async () => {
    try {
      await playerNext();
    } catch (err) {
      console.error("[PlayerContext] next failed", err);
    }
  }, []);

  const previous = useCallback(async () => {
    try {
      await playerPrevious();
    } catch (err) {
      console.error("[PlayerContext] previous failed", err);
    }
  }, []);

  const seek = useCallback(async (ms: number) => {
    // Optimistic: update the UI position immediately; the backend
    // will also emit player:position after the seek lands.
    setPositionMs(ms);
    try {
      await playerSeek(ms);
    } catch (err) {
      console.error("[PlayerContext] seek failed", err);
    }
  }, []);

  const setSeeking = useCallback((value: boolean) => {
    isSeekingRef.current = value;
  }, []);

  // --- Shuffle / repeat (local only at checkpoint 11) ---
  const toggleShuffle = useCallback(() => {
    setIsShuffled((prev) => !prev);
  }, []);
  const cycleRepeatMode = useCallback(() => {
    setRepeatMode((prev) => {
      if (prev === "off") return "all";
      if (prev === "all") return "one";
      return "off";
    });
  }, []);

  const toggleQueue = useCallback(() => setIsQueueOpen((p) => !p), []);
  const toggleDeviceMenu = useCallback(
    () => setIsDeviceMenuOpen((p) => !p),
    []
  );

  return (
    <PlayerContext.Provider
      value={{
        isQueueOpen,
        toggleQueue,
        isDeviceMenuOpen,
        toggleDeviceMenu,
        playbackState,
        isPlaying,
        currentTrack,
        positionMs,
        durationMs,
        volume,
        setVolume,
        toggleMute,
        isShuffled,
        toggleShuffle,
        repeatMode,
        cycleRepeatMode,
        playTracks,
        togglePlayback,
        next,
        previous,
        seek,
        setSeeking,
      }}
    >
      {children}
    </PlayerContext.Provider>
  );
}
