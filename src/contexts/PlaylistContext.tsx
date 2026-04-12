import { useCallback, useEffect, useState, type ReactNode } from "react";
import { PlaylistContext } from "../hooks/usePlaylist";
import { useProfile } from "../hooks/useProfile";
import {
  addSourceToPlaylist as apiAddSourceToPlaylist,
  addTracksToPlaylist as apiAddTracksToPlaylist,
  createPlaylist as apiCreatePlaylist,
  deletePlaylist as apiDeletePlaylist,
  listPlaylists,
  listPlaylistTracks,
  removeTrackFromPlaylist as apiRemoveTrackFromPlaylist,
  updatePlaylist as apiUpdatePlaylist,
  type CreatePlaylistInput,
  type Playlist,
  type UpdatePlaylistInput,
} from "../lib/tauri/playlist";

export function PlaylistProvider({ children }: { children: ReactNode }) {
  const { activeProfile } = useProfile();
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!activeProfile) {
      setPlaylists([]);
      return;
    }
    try {
      const list = await listPlaylists();
      setPlaylists(list);
      setError(null);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      console.error("[PlaylistContext] refresh failed", err);
    }
  }, [activeProfile]);

  // Re-fetch whenever the active profile changes — playlists are scoped
  // to the profile's `data.db` which is swapped on profile switch.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        if (!activeProfile) {
          if (!cancelled) setPlaylists([]);
          return;
        }
        const list = await listPlaylists();
        if (cancelled) return;
        setPlaylists(list);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        console.error("[PlaylistContext] initial load failed", err);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeProfile]);

  const createPlaylist = useCallback(
    async (input: CreatePlaylistInput) => {
      const created = await apiCreatePlaylist(input);
      await refresh();
      return created;
    },
    [refresh]
  );

  const updatePlaylist = useCallback(
    async (playlistId: number, input: UpdatePlaylistInput) => {
      await apiUpdatePlaylist(playlistId, input);
      await refresh();
    },
    [refresh]
  );

  const deletePlaylist = useCallback(
    async (playlistId: number) => {
      await apiDeletePlaylist(playlistId);
      await refresh();
    },
    [refresh]
  );

  const addTracksToPlaylist = useCallback(
    async (playlistId: number, trackIds: number[]) => {
      const inserted = await apiAddTracksToPlaylist(playlistId, trackIds);
      // Refresh to update track_count / total_duration_ms on the row.
      await refresh();
      return inserted;
    },
    [refresh]
  );

  const removeTrackFromPlaylist = useCallback(
    async (playlistId: number, trackId: number) => {
      await apiRemoveTrackFromPlaylist(playlistId, trackId);
      await refresh();
    },
    [refresh]
  );

  const getPlaylistTracks = useCallback((playlistId: number) => {
    return listPlaylistTracks(playlistId);
  }, []);

  const addSourceToPlaylist = useCallback(
    async (
      playlistId: number,
      sourceType: "folder" | "album" | "artist",
      sourceId: number
    ) => {
      const inserted = await apiAddSourceToPlaylist(
        playlistId,
        sourceType,
        sourceId
      );
      await refresh();
      return inserted;
    },
    [refresh]
  );

  return (
    <PlaylistContext.Provider
      value={{
        playlists,
        isLoading,
        error,
        refresh,
        createPlaylist,
        updatePlaylist,
        deletePlaylist,
        addTracksToPlaylist,
        removeTrackFromPlaylist,
        getPlaylistTracks,
        addSourceToPlaylist,
      }}
    >
      {children}
    </PlaylistContext.Provider>
  );
}
