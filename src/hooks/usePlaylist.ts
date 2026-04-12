import { createContext, useContext } from "react";
import type { Track } from "../lib/tauri/track";
import type {
  CreatePlaylistInput,
  Playlist,
  UpdatePlaylistInput,
} from "../lib/tauri/playlist";

interface PlaylistContextValue {
  /** All playlists belonging to the currently active profile. */
  playlists: Playlist[];
  isLoading: boolean;
  error: string | null;
  /** Re-fetch the list (called after mutations like create / delete). */
  refresh: () => Promise<void>;
  createPlaylist: (input: CreatePlaylistInput) => Promise<Playlist>;
  updatePlaylist: (
    playlistId: number,
    input: UpdatePlaylistInput
  ) => Promise<void>;
  deletePlaylist: (playlistId: number) => Promise<void>;
  /**
   * Append tracks to the end of a playlist. Resolves with the number of
   * rows actually inserted (duplicates are ignored by `INSERT OR IGNORE`
   * in the backend).
   */
  addTracksToPlaylist: (
    playlistId: number,
    trackIds: number[]
  ) => Promise<number>;
  /**
   * One-shot fetch of the tracks in a playlist. Not cached — callers
   * (e.g. PlaylistView) manage their own loading state.
   */
  getPlaylistTracks: (playlistId: number) => Promise<Track[]>;
  /** Remove a single track from a playlist and renumber the tail. */
  removeTrackFromPlaylist: (
    playlistId: number,
    trackId: number
  ) => Promise<void>;
  /**
   * Add all tracks from a source (folder, album, artist) to a playlist.
   * Runs entirely server-side — no track IDs round-trip through IPC.
   */
  addSourceToPlaylist: (
    playlistId: number,
    sourceType: "folder" | "album" | "artist",
    sourceId: number
  ) => Promise<number>;
}

export const PlaylistContext = createContext<PlaylistContextValue | null>(null);

export function usePlaylist() {
  const context = useContext(PlaylistContext);
  if (!context)
    throw new Error("usePlaylist must be used within PlaylistProvider");
  return context;
}
