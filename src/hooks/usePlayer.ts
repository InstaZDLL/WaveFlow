import { createContext, useContext } from "react";
import type { Track } from "../lib/tauri/track";
import type { OutputDevice, QueueSource } from "../lib/tauri/player";

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
  isNowPlayingOpen: boolean;
  toggleNowPlaying: () => void;
  isLyricsOpen: boolean;
  toggleLyrics: () => void;
  isDeviceMenuOpen: boolean;
  toggleDeviceMenu: () => void;

  // Output device picker — pre-fetched at boot so the first click on
  // the device button paints instantly. `refreshOutputDevices` is
  // called by `DeviceMenu` in the background on each open to catch
  // hot-plugged USB DACs / Bluetooth sinks without polling.
  outputDevices: OutputDevice[];
  refreshOutputDevices: () => Promise<void>;

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

  // Shuffle / repeat are backend-synced via player_toggle_shuffle /
  // player_cycle_repeat. The UI flips optimistically and rolls back
  // on backend error.
  isShuffled: boolean;
  toggleShuffle: () => Promise<void>;
  repeatMode: RepeatMode;
  cycleRepeatMode: () => Promise<void>;

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
