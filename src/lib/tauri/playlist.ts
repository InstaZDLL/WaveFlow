import { invoke } from "@tauri-apps/api/core";
import type { Track } from "./track";

/**
 * Playlist row returned by the Rust backend (mirrors
 * `commands::playlist::Playlist`). `track_count` and `total_duration_ms`
 * are computed by the SELECT, not cached columns.
 */
export interface Playlist {
  id: number;
  name: string;
  description: string | null;
  color_id: string;
  icon_id: string;
  is_smart: number;
  position: number;
  created_at: number;
  updated_at: number;
  track_count: number;
  total_duration_ms: number;
}

export interface CreatePlaylistInput {
  name: string;
  description?: string | null;
  color_id?: string;
  icon_id?: string;
}

/** Partial update payload — any omitted field is preserved. */
export interface UpdatePlaylistInput {
  name?: string;
  description?: string | null;
  color_id?: string;
  icon_id?: string;
}

export function listPlaylists(): Promise<Playlist[]> {
  return invoke<Playlist[]>("list_playlists");
}

export function getPlaylist(playlistId: number): Promise<Playlist> {
  return invoke<Playlist>("get_playlist", { playlistId });
}

export function createPlaylist(input: CreatePlaylistInput): Promise<Playlist> {
  return invoke<Playlist>("create_playlist", { input });
}

export function updatePlaylist(
  playlistId: number,
  input: UpdatePlaylistInput
): Promise<void> {
  return invoke<void>("update_playlist", { playlistId, input });
}

export function deletePlaylist(playlistId: number): Promise<void> {
  return invoke<void>("delete_playlist", { playlistId });
}

export function listPlaylistTracks(playlistId: number): Promise<Track[]> {
  return invoke<Track[]>("list_playlist_tracks", { playlistId });
}

export function addTrackToPlaylist(
  playlistId: number,
  trackId: number
): Promise<void> {
  return invoke<void>("add_track_to_playlist", { playlistId, trackId });
}

export function addTracksToPlaylist(
  playlistId: number,
  trackIds: number[]
): Promise<number> {
  return invoke<number>("add_tracks_to_playlist", { playlistId, trackIds });
}

export function removeTrackFromPlaylist(
  playlistId: number,
  trackId: number
): Promise<void> {
  return invoke<void>("remove_track_from_playlist", { playlistId, trackId });
}

export function reorderPlaylistTrack(
  playlistId: number,
  trackId: number,
  newPosition: number
): Promise<void> {
  return invoke<void>("reorder_playlist_track", {
    playlistId,
    trackId,
    newPosition,
  });
}

/**
 * Add every track belonging to a source (folder, album, artist) to a
 * playlist. The SELECT + INSERT runs server-side in one transaction so
 * no track IDs travel through the IPC bridge.
 */
export function addSourceToPlaylist(
  playlistId: number,
  sourceType: "folder" | "album" | "artist",
  sourceId: number
): Promise<number> {
  return invoke<number>("add_source_to_playlist", {
    playlistId,
    sourceType,
    sourceId,
  });
}

export interface ImportPlaylistResult {
  playlist_id: number;
  imported: number;
  missing: number;
  /** Up to 20 unmatched paths from the imported file. */
  missing_paths: string[];
}

/** Write the playlist out as a UTF-8 .m3u8 file at `destPath`. */
export function exportPlaylistM3u(
  playlistId: number,
  destPath: string,
): Promise<void> {
  return invoke<void>("export_playlist_m3u", { playlistId, destPath });
}

/**
 * Parse an .m3u/.m3u8 file, match its entries against the active
 * library, and create a new playlist holding the resolved tracks.
 */
export function importPlaylistM3u(
  sourcePath: string,
): Promise<ImportPlaylistResult> {
  return invoke<ImportPlaylistResult>("import_playlist_m3u", { sourcePath });
}
