import { createContext, useContext } from "react";
import type { Track } from "../lib/tauri/track";
import type { QueueSource } from "../lib/tauri/player";

export type RepeatMode = "off" | "all" | "one";

/** Aggregate `state` field driven by backend `player:state` events. */
export type PlaybackState =
  | "idle"
  | "loading"
  | "playing"
  | "paused"
  | "ended";

interface PlayerContextValue {
  // UI-only state (not persisted)
  isQueueOpen: boolean;
  toggleQueue: () => void;
  isDeviceMenuOpen: boolean;
  toggleDeviceMenu: () => void;

  // Backend-synced state
  playbackState: PlaybackState;
  isPlaying: boolean;
  currentTrack: Track | null;
  positionMs: number;
  durationMs: number;

  // Volume: UI-owned slider (0-100) debounced into the backend.
  volume: number;
  setVolume: (value: number) => void;
  toggleMute: () => void;

  // Shuffle / repeat are UI flags for checkpoint 11 — checkpoint 12
  // will wire them to new player_toggle_shuffle / player_cycle_repeat
  // commands.
  isShuffled: boolean;
  toggleShuffle: () => void;
  repeatMode: RepeatMode;
  cycleRepeatMode: () => void;

  // Backend actions
  playTracks: (
    tracks: Track[],
    startIndex: number,
    source: { type: QueueSource; id: number | null }
  ) => Promise<void>;
  togglePlayback: () => Promise<void>;
  next: () => Promise<void>;
  previous: () => Promise<void>;
  seek: (ms: number) => Promise<void>;
  /** Register that the user is dragging the progress bar — suppresses
   *  incoming `player:position` updates while dragging. */
  setSeeking: (value: boolean) => void;
}

export const PlayerContext = createContext<PlayerContextValue | null>(null);

export function usePlayer() {
  const context = useContext(PlayerContext);
  if (!context)
    throw new Error("usePlayer must be used within PlayerProvider");
  return context;
}
