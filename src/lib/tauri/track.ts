import { invoke } from "@tauri-apps/api/core";

/**
 * Track row joined with album title + primary artist name, as returned by
 * the `list_tracks` command. The IDs of the joined rows are intentionally
 * omitted — once the UI needs album/artist pages we'll switch to returning
 * full relations.
 *
 * `artwork_path` is the absolute filesystem path to the album cover image
 * that the scanner extracted and wrote under `<profile>/artwork/<hash>.<ext>`.
 * It's `null` when the album has no cover yet — the UI falls back to a
 * placeholder tile in that case.
 */
export interface Track {
  id: number;
  library_id: number;
  title: string;
  album_id: number | null;
  album_title: string | null;
  artist_id: number | null;
  artist_name: string | null;
  /** Comma-joined artist IDs matching the `", "`-joined `artist_name`. */
  artist_ids: string | null;
  duration_ms: number;
  track_number: number | null;
  disc_number: number | null;
  year: number | null;
  bitrate: number | null;
  sample_rate: number | null;
  channels: number | null;
  /** Bits per sample. `null` for lossy codecs (MP3, AAC) and for
   *  pre-migration rows that haven't been re-scanned yet. */
  bit_depth: number | null;
  /** Short codec / container label, e.g. `"FLAC"`, `"MP3"`. */
  codec: string | null;
  file_path: string;
  file_size: number;
  added_at: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Raw POPM byte (0-255). `null` when no rating is set. */
  rating: number | null;
}

/** Sort spec accepted by `listTracks` / `listAlbums` / `listArtists`. */
export interface SortSpec {
  orderBy?: string;
  direction?: "asc" | "desc";
}

export function listTracks(
  libraryId: number | null,
  sort?: SortSpec,
): Promise<Track[]> {
  return invoke<Track[]>("list_tracks", {
    libraryId,
    orderBy: sort?.orderBy ?? null,
    direction: sort?.direction ?? null,
  });
}

/** Full-text search across title, album and artist. Returns up to 50 results. */
export function searchTracks(query: string): Promise<Track[]> {
  return invoke<Track[]>("search_tracks", { query });
}

/** Toggle liked state. Returns `true` if the track is now liked. */
export function toggleLikeTrack(trackId: number): Promise<boolean> {
  return invoke<boolean>("toggle_like_track", { trackId });
}

/** Set or clear a track rating. `rating` is the raw POPM byte 0-255, or null to clear. */
export function setTrackRating(trackId: number, rating: number | null): Promise<void> {
  return invoke<void>("set_track_rating", { trackId, rating });
}

/** All liked track IDs (cheap — no full rows, just IDs). */
export function listLikedTrackIds(): Promise<number[]> {
  return invoke<number[]>("list_liked_track_ids");
}

/** All liked tracks with full metadata, ordered by most recently liked. */
export function listLikedTracks(): Promise<Track[]> {
  return invoke<Track[]>("list_liked_tracks");
}

/**
 * Format a duration in milliseconds as `m:ss` or `h:mm:ss`. Used by the
 * library views that display track durations in a column.
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "0:00";
  const totalSeconds = Math.round(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const secondsStr = seconds.toString().padStart(2, "0");
  if (hours > 0) {
    const minutesStr = minutes.toString().padStart(2, "0");
    return `${hours}:${minutesStr}:${secondsStr}`;
  }
  return `${minutes}:${secondsStr}`;
}
