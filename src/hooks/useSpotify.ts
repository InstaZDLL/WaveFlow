import { createContext, useContext } from "react";
import type {
  SpotifyPlaylistLite,
  SpotifyStatus,
  SpotifyTrackLite,
} from "../lib/tauri/spotify";

export type SpotifyPlaybackState =
  "idle" | "loading" | "playing" | "paused" | "ended";

interface SpotifyContextValue {
  status: SpotifyStatus | null;
  isConnected: boolean;
  isSdkReady: boolean;
  deviceId: string | null;
  error: string | null;
  currentTrack: SpotifyTrackLite | null;
  playbackState: SpotifyPlaybackState;
  positionMs: number;
  durationMs: number;
  volume: number;
  refreshStatus: () => Promise<void>;
  login: () => Promise<void>;
  logout: () => Promise<void>;
  playTrack: (track: SpotifyTrackLite) => Promise<void>;
  playContext: (contextUri: string) => Promise<void>;
  togglePlayback: () => Promise<void>;
  next: () => Promise<void>;
  previous: () => Promise<void>;
  seek: (ms: number) => Promise<void>;
  setVolume: (value: number) => Promise<void>;
  loadPlaylistTracks: (
    playlist: SpotifyPlaylistLite,
  ) => Promise<SpotifyTrackLite[]>;
}

export const SpotifyContext = createContext<SpotifyContextValue | null>(null);

export function useSpotify() {
  const context = useContext(SpotifyContext);
  if (!context)
    throw new Error("useSpotify must be used within SpotifyProvider");
  return context;
}
