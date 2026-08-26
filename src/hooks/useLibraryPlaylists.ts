import { useMemo } from "react";
import { useRemoteSource } from "./useRemoteSource";
import type { LibrarySourceFilter } from "./useLibrarySource";
import type { LibrarySource } from "../lib/tauri/browse";
import type { Playlist } from "../lib/tauri/playlist";

/**
 * A playlist of the library, from either source.
 *
 * Built here rather than fetched: unlike the other three library tabs, both
 * playlist surfaces already sorted in the browser, so there is no SQL ordering
 * to unify and a compound select would buy nothing. The two shapes are merged
 * where they are read.
 */
export interface LibraryPlaylistRow {
  source: LibrarySource;
  /** Local rowid as text, or the server's playlist identifier. */
  id: string;
  name: string;
  track_count: number;
  total_duration_ms: number;
  /** Local only: the server's summary carries no modification time. */
  updated_at: number | null;
  /** Local only: the sidebar's manual order. */
  position: number | null;
  color_id: string;
  icon_id: string | null;
  cover_path: string | null;
  /** Remote only: created here and not yet sent to the server. */
  pending_creation: boolean;
}

/**
 * The local playlists given, plus the bound server's, as one list.
 *
 * `localPlaylists` is passed in rather than read from the context because the
 * two consumers want different sets: the library tab shows the user's own
 * playlists only, the sidebar shows the smart ones too.
 *
 * `source` narrows the result. The sidebar passes `"all"` on purpose — it is
 * navigation, not a filtered view, and hiding half of it because a tab is
 * filtered would make the filter reach somewhere it was never pointed.
 */
export function useLibraryPlaylists(
  localPlaylists: Playlist[],
  source: LibrarySourceFilter,
): LibraryPlaylistRow[] {
  const remote = useRemoteSource();
  return useMemo(() => {
    const rows: LibraryPlaylistRow[] = [];
    if (source !== "remote") {
      for (const playlist of localPlaylists) {
        rows.push({
          source: "local",
          id: String(playlist.id),
          name: playlist.name,
          track_count: playlist.track_count,
          total_duration_ms: playlist.total_duration_ms,
          updated_at: playlist.updated_at,
          position: playlist.position,
          color_id: playlist.color_id,
          icon_id: playlist.icon_id,
          cover_path: playlist.cover_path,
          pending_creation: false,
        });
      }
    }
    if (source !== "local" && remote.available) {
      for (const playlist of remote.playlists) {
        rows.push({
          source: "remote",
          id: playlist.id,
          name: playlist.name,
          track_count: playlist.track_count,
          total_duration_ms: playlist.duration_ms,
          // The server's summary carries neither; consumers file them last
          // rather than reading a missing key as zero.
          updated_at: null,
          position: null,
          color_id: "",
          icon_id: null,
          cover_path: null,
          pending_creation: playlist.pending_creation,
        });
      }
    }
    return rows;
  }, [localPlaylists, remote.available, remote.playlists, source]);
}
