import { createContext, useContext } from "react";
import type { Track } from "../lib/tauri/track";
import type { OutputDevice, OutputMode, QueueSource } from "../lib/tauri/player";
import type { SpotifyTrackLite } from "../lib/tauri/spotify";
import type { PluginFavorite } from "../lib/tauri/plugins";

export type RepeatMode = "off" | "all" | "one";

/**
 * Something playback needs to tell the user (#597).
 *
 * `severity` keeps the two registers apart: `error` is a fault — the
 * device went away, the track would not open — and `info` is playback
 * quietly becoming something other than what was asked for, which is
 * normal operation and must not read as a fault.
 *
 * `detail` is the backend's own message. It stays out of the visible
 * sentence and goes where a bug report can find it.
 */
export interface PlaybackAlert {
  /** Distinguishes one occurrence from the next, so a repeat re-shows. */
  id: number;
  severity: "error" | "info";
  kind: string;
  detail?: string;
}
export type ActiveProvider = "local" | "spotify";

/** Aggregate `state` field driven by backend `player:state` events. */
export type PlaybackState = "idle" | "loading" | "playing" | "paused" | "ended";

interface PlayerContextValue {
  // UI-only state (not persisted)
  /** Which right-edge panel is currently open. Mutually exclusive by
   *  construction — at most one panel is rendered at a time. */
  activeRightPanel: "queue" | "nowPlaying" | "lyrics" | null;
  isQueueOpen: boolean;
  toggleQueue: () => void;
  isNowPlayingOpen: boolean;
  toggleNowPlaying: () => void;
  isLyricsOpen: boolean;
  toggleLyrics: () => void;
  isDeviceMenuOpen: boolean;
  toggleDeviceMenu: () => void;

  // Immersive view (issue #328) — the now-playing + lyrics overlays
  // merged into one two-column fullscreen view. `immersiveInitialTab`
  // records which entry point opened it (cover / now-playing button vs
  // the lyrics button); only the narrow-window single-column fallback
  // uses it to pick the first column. `openImmersive`/`closeImmersive`
  // are the canonical actions; the four `*Fullscreen*` names are kept
  // as back-compat aliases for existing callsites.
  immersiveOpen: boolean;
  immersiveInitialTab: "nowPlaying" | "lyrics";
  openImmersive: (tab: "nowPlaying" | "lyrics") => void;
  closeImmersive: () => void;
  openFullscreenNowPlaying: () => void;
  closeFullscreenNowPlaying: () => void;
  openFullscreenLyrics: () => void;
  closeFullscreenLyrics: () => void;

  // Output device picker — pre-fetched at boot so the first click on
  // the device button paints instantly. `refreshOutputDevices` is
  // called by `DeviceMenu` in the background on each open to catch
  // hot-plugged USB DACs / Bluetooth sinks without polling.
  outputDevices: OutputDevice[];
  refreshOutputDevices: () => Promise<void>;

  // Backend-synced state
  activeProvider: ActiveProvider;
  playbackState: PlaybackState;
  isPlaying: boolean;
  currentTrack: Track | null;
  /** Stable identity of the live Web Radio station currently playing
   *  (id `url:<stream>` + name + favicon), kept separate from the
   *  now-playing song so the PlayerBar / mini-player "favorite station"
   *  star can save the station even after an ICY title overwrote the
   *  track line. `null` when the current source isn't Web Radio. */
  currentRadioStation: PluginFavorite | null;
  positionMs: number;
  durationMs: number;

  // Volume: UI-owned slider (0-100) debounced into the backend.
  volume: number;
  setVolume: (value: number) => void;
  toggleMute: () => void;

  // Playback speed multiplier, clamped to [0.5, 2.0]. Pitch follows
  // speed (no time-stretching). Persisted per-profile.
  playbackSpeed: number;
  setPlaybackSpeed: (value: number) => void;

  // Live output-device fields, refreshed on every `player:track-changed`
  // so WASAPI exclusive re-opens are reflected. Both are `null` before
  // the first stream has opened; UI must skip the "0 kHz" display.
  deviceSampleRate: number | null;
  deviceChannels: number | null;

  /**
   * What the output really engaged as (#597) — read from the engine, not
   * derived here, and refreshed on `player:audio-mode-changed` because a
   * rebuild can change it without anyone touching a setting.
   */
  outputMode: OutputMode;

  /**
   * The last thing playback had to say for itself (#597): a failure, or
   * a silent degradation the user chose against. `null` once dismissed.
   */
  playbackAlert: PlaybackAlert | null;
  dismissPlaybackAlert: () => void;

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
    source: { type: QueueSource; id: number | null },
  ) => Promise<void>;
  playSpotifyTrack: (track: SpotifyTrackLite) => Promise<void>;
  playSpotifyContext: (contextUri: string) => Promise<void>;
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
  if (!context) throw new Error("usePlayer must be used within PlayerProvider");
  return context;
}
