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
  color_mode: "auto" | "manual";
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
  /** Entries of an imported playlist the library does not hold yet
   * (placeholders). Counted apart: `track_count` and
   * `total_duration_ms` cover the tracks the user has, only. */
  placeholder_count: number;
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
  | { kind: "genre_mix"; genre_id: number }
  | { kind: "decade_mix"; decade: number }
  | { kind: "artist_mix"; artist_id: number }
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
    const parsed = JSON.parse(p.smart_rules) as {
      kind?: string;
      slot?: number;
      genre_id?: number;
      decade?: number;
      artist_id?: number;
    };
    if (parsed.kind === "daily_mix" && typeof parsed.slot === "number") {
      return { kind: "daily_mix", slot: parsed.slot };
    }
    if (parsed.kind === "on_repeat") return { kind: "on_repeat" };
    if (parsed.kind === "genre_mix" && typeof parsed.genre_id === "number") {
      return { kind: "genre_mix", genre_id: parsed.genre_id };
    }
    if (parsed.kind === "decade_mix" && typeof parsed.decade === "number") {
      return { kind: "decade_mix", decade: parsed.decade };
    }
    if (parsed.kind === "artist_mix" && typeof parsed.artist_id === "number") {
      return { kind: "artist_mix", artist_id: parsed.artist_id };
    }
    if (parsed.kind === "custom") return { kind: "custom" };
    return null;
  } catch {
    return null;
  }
}

/**
 * The eyebrow a generated playlist carries over its cover, by family
 * (an i18n key), and the colour its card falls back to without one.
 */
export function smartFamilyStyle(kind: SmartPlaylistKind): {
  labelKey: string;
  gradient: string;
  ring: string;
} {
  switch (kind?.kind) {
    case "on_repeat":
      return {
        labelKey: "home.onRepeat.label",
        gradient: "bg-linear-to-br from-emerald-400 to-emerald-700",
        ring: "focus-visible:ring-emerald-500",
      };
    case "genre_mix":
      return {
        labelKey: "home.genreMix.label",
        gradient: "bg-linear-to-br from-amber-400 to-orange-700",
        ring: "focus-visible:ring-amber-500",
      };
    case "artist_mix":
      return {
        labelKey: "home.artistMix.label",
        gradient: "bg-linear-to-br from-rose-400 to-fuchsia-700",
        ring: "focus-visible:ring-rose-500",
      };
    case "decade_mix":
      return {
        labelKey: "home.decadeMix.label",
        gradient: "bg-linear-to-br from-sky-400 to-indigo-700",
        ring: "focus-visible:ring-sky-500",
      };
    case "custom":
      return {
        labelKey: "playlistView.smartLabelCustom",
        gradient: "bg-linear-to-br from-violet-400 to-violet-700",
        ring: "focus-visible:ring-violet-500",
      };
    default:
      return {
        labelKey: "home.dailyMix.label",
        gradient: "bg-linear-to-br from-violet-400 to-violet-700",
        ring: "focus-visible:ring-violet-500",
      };
  }
}

export interface CreatePlaylistInput {
  name: string;
  description?: string | null;
  color_id?: string;
  color_mode?: "auto" | "manual";
  icon_id?: string;
}

/** Partial update payload — any omitted field is preserved. */
export interface UpdatePlaylistInput {
  name?: string;
  description?: string | null;
  color_id?: string;
  color_mode?: "auto" | "manual";
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
  name: string;
  /** Tracks the library holds, now in the playlist. */
  imported: number;
  /** Entries kept as placeholders, filled in when a scan brings them. */
  placeholders: number;
  /** Entries naming a track the playlist already held. */
  duplicates: number;
}

/**
 * An entry of an imported playlist the library does not hold (yet). It
 * keeps its place: `position` is in the numbering of the tracks'
 * positions, and the placeholder sits just before the track holding the
 * same position (at the end when none does); `ordinal` orders the
 * placeholders sharing a position.
 */
export interface PlaylistPlaceholder {
  id: number;
  playlist_id: number;
  position: number;
  ordinal: number;
  title: string;
  artist: string | null;
  album: string | null;
  duration_ms: number | null;
  isrc: string | null;
  locator: string | null;
  source: "m3u" | "csv" | "json" | "deezer";
  added_at: number;
  /** The available track the placeholder is shown before; `null` at the
   * end of the playlist. */
  before_track_id: number | null;
}

/** Write the playlist out as a UTF-8 .m3u8 file at `destPath`. */
export function exportPlaylistM3u(
  playlistId: number,
  destPath: string,
): Promise<void> {
  return invoke<void>("export_playlist_m3u", { playlistId, destPath });
}

/**
 * Import an .m3u / .m3u8, .csv or .json playlist file into a new
 * playlist: what the library holds becomes tracks, the rest placeholders.
 */
export function importPlaylistFile(
  sourcePath: string,
): Promise<ImportPlaylistResult> {
  return invoke<ImportPlaylistResult>("import_playlist_file", { sourcePath });
}

/**
 * The translation key for an import refusal the backend names, `null`
 * for anything else (shown as is). The prefixes are the `ERR_*`
 * constants of `commands::playlist_import`.
 */
export function playlistImportErrorKey(err: unknown): string | null {
  const message = String(err);
  const known: [string, string][] = [
    ["playlist-import: unsupported file", "playlistImport.errors.unsupported"],
    ["playlist-import: unreadable", "playlistImport.errors.unreadable"],
    ["playlist-import: no title column", "playlistImport.errors.noTitleColumn"],
    ["playlist-import: empty", "playlistImport.errors.empty"],
    ["playlist-import: not a deezer playlist link", "playlistImport.errors.notALink"],
    ["playlist-import: offline", "playlistImport.errors.offline"],
    ["playlist-import: not found", "playlistImport.errors.notFound"],
    ["playlist-import: unreachable", "playlistImport.errors.unreachable"],
  ];
  return known.find(([prefix]) => message.includes(prefix))?.[1] ?? null;
}

/** Import a public Deezer playlist from its link (or its id). */
export function importPlaylistDeezer(
  link: string,
): Promise<ImportPlaylistResult> {
  return invoke<ImportPlaylistResult>("import_playlist_deezer", { link });
}

/** The placeholders of a playlist, in its order. */
export function listPlaylistPlaceholders(
  playlistId: number,
): Promise<PlaylistPlaceholder[]> {
  return invoke<PlaylistPlaceholder[]>("list_playlist_placeholders", {
    playlistId,
  });
}

/** Remove one placeholder from its playlist. */
export function removePlaylistPlaceholder(
  placeholderId: number,
): Promise<void> {
  return invoke<void>("remove_playlist_placeholder", { placeholderId });
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

/** Safe, downscaled preview for a picked image outside the asset scope. */
export function previewPlaylistCoverFromFile(
  filePath: string,
): Promise<string> {
  return invoke<string>("preview_playlist_cover_from_file", { filePath });
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
