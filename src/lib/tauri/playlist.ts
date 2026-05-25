import { invoke } from "@tauri-apps/api/core";
import {
  expandTrackResponse,
  type ListTracksResponse,
  type Track,
} from "./track";

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
  /** Blake3 hash of the cover image. Always paired with `cover_path`
   * when the file is materialized; consumers should prefer `cover_path`
   * for rendering and only fall back to `cover_hash` for cache busting. */
  cover_hash: string | null;
  /** Absolute on-disk path resolved by the backend. Pass through
   * `convertFileSrc` to render. `null` means "no custom cover —
   * draw the icon + color gradient instead". */
  cover_path: string | null;
  /** `1` when the cover is auto-managed (regenerates from the first
   * 4 album artworks after every mutation). `0` when the user
   * uploaded their own image; mutations leave it alone until they
   * click "Remove photo" to switch back to auto. */
  cover_is_auto: number;
  position: number;
  created_at: number;
  updated_at: number;
  track_count: number;
  total_duration_ms: number;
  /** Raw JSON payload from `playlist.smart_rules`. `null` for user
   * playlists. For smart playlists the frontend parses the `kind`
   * discriminant to distinguish Daily Mix slots, On Repeat, and
   * custom rule sets — see {@link smartPlaylistKind}. */
  smart_rules: string | null;
}

/** Family discriminator parsed from {@link Playlist.smart_rules}. */
export type SmartPlaylistKind =
  | { kind: "daily_mix"; slot: number }
  | { kind: "on_repeat" }
  | { kind: "custom" }
  | null;

/**
 * Parse a playlist's `smart_rules` JSON into its family discriminant.
 * Returns `null` for user playlists or smart playlists with an
 * unrecognised payload — callers should treat that as "fall back to
 * the generic smart-playlist styling".
 */
export function smartPlaylistKind(p: Playlist): SmartPlaylistKind {
  if (p.is_smart !== 1 || !p.smart_rules) return null;
  try {
    const parsed = JSON.parse(p.smart_rules) as { kind?: string; slot?: number };
    if (parsed.kind === "daily_mix" && typeof parsed.slot === "number") {
      return { kind: "daily_mix", slot: parsed.slot };
    }
    if (parsed.kind === "on_repeat") return { kind: "on_repeat" };
    if (parsed.kind === "custom") return { kind: "custom" };
    return null;
  } catch {
    return null;
  }
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
  input: UpdatePlaylistInput,
): Promise<void> {
  return invoke<void>("update_playlist", { playlistId, input });
}

export function deletePlaylist(playlistId: number): Promise<void> {
  return invoke<void>("delete_playlist", { playlistId });
}

export async function listPlaylistTracks(playlistId: number): Promise<Track[]> {
  const resp = await invoke<ListTracksResponse>("list_playlist_tracks", {
    playlistId,
  });
  return expandTrackResponse(resp);
}

export function addTrackToPlaylist(
  playlistId: number,
  trackId: number,
): Promise<void> {
  return invoke<void>("add_track_to_playlist", { playlistId, trackId });
}

export function addTracksToPlaylist(
  playlistId: number,
  trackIds: number[],
): Promise<number> {
  return invoke<number>("add_tracks_to_playlist", { playlistId, trackIds });
}

export function removeTrackFromPlaylist(
  playlistId: number,
  trackId: number,
): Promise<void> {
  return invoke<void>("remove_track_from_playlist", { playlistId, trackId });
}

/**
 * Return the IDs of every user playlist that currently contains the track.
 * Smart playlists are excluded — their membership is rule-driven and not
 * a toggle target. Used by the `+` popover to mark existing memberships
 * with a checkmark and switch the click handler from add to remove.
 */
export function listPlaylistsContainingTrack(
  trackId: number,
): Promise<number[]> {
  return invoke<number[]>("list_playlists_containing_track", { trackId });
}

export function reorderPlaylistTrack(
  playlistId: number,
  trackId: number,
  newPosition: number,
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
  sourceId: number,
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

/**
 * Upload a user-supplied image as the playlist cover. Validates the file
 * by magic bytes (jpg/png/webp), normalises through the same compositor
 * used for auto-covers (re-encodes to a 640×640 JPEG), and flips
 * `cover_is_auto` to 0 so subsequent mutations stop overwriting it.
 */
export function setPlaylistCoverFromFile(
  playlistId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_playlist_cover_from_file", { playlistId, filePath });
}

/**
 * Force a regen of the auto-cover (Spotify-style 2×2 grid of the first
 * 4 album artworks). Normally the auto pipeline runs implicitly after
 * every mutation; this command is the "refresh now" escape hatch.
 */
export function regeneratePlaylistAutoCover(playlistId: number): Promise<void> {
  return invoke<void>("regenerate_playlist_auto_cover", { playlistId });
}

/**
 * Drop the manual cover and switch back to auto mode. Immediately
 * re-runs the auto-cover so the visual feedback is instant.
 */
export function clearPlaylistCover(playlistId: number): Promise<void> {
  return invoke<void>("clear_playlist_cover", { playlistId });
}
