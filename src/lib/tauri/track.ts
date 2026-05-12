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
  /** Tagged musical key (e.g. `"Am"`, `"F#"`, Camelot `"8A"`).
   *  Read at scan time from `TKEY` (ID3v2) or `INITIALKEY` (Vorbis,
   *  MP4, APE, WavPack). `null` when the file has no key tag. */
  musical_key: string | null;
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

/** Fetch a single track by id. Returns null when the row was deleted. */
export function getTrack(trackId: number): Promise<Track | null> {
  return invoke<Track | null>("get_track", { trackId });
}

/**
 * Multi-criteria filters layered on top of the FTS5 search. Every field
 * is optional — `query` itself can be omitted to run a pure-filter
 * browse (e.g. "all my Hi-Res FLACs from the 90s").
 */
export interface SearchFilters {
  query?: string | null;
  genre_ids?: number[] | null;
  year_min?: number | null;
  year_max?: number | null;
  bpm_min?: number | null;
  bpm_max?: number | null;
  duration_min_ms?: number | null;
  duration_max_ms?: number | null;
  formats?: string[] | null;
  min_sample_rate?: number | null;
  min_bit_depth?: number | null;
  hi_res_only?: boolean | null;
  liked_only?: boolean | null;
}

/** Advanced search — combines FTS5 with structured filters. Returns up to 200 rows. */
export function searchTracksAdvanced(filters: SearchFilters): Promise<Track[]> {
  return invoke<Track[]>("search_tracks_advanced", { filters });
}

/** Toggle liked state. Returns `true` if the track is now liked. */
export function toggleLikeTrack(trackId: number): Promise<boolean> {
  return invoke<boolean>("toggle_like_track", { trackId });
}

/** Set or clear a track rating. `rating` is the raw POPM byte 0-255, or null to clear. */
export function setTrackRating(
  trackId: number,
  rating: number | null,
): Promise<void> {
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
 * Editable track-tag fields. Every property is optional — `null`
 * (not transmitted) means "leave this field untouched"; an empty
 * string means "clear this field" where the format allows it.
 *
 * `artist` accepts a comma-separated multi-artist string ("A, B"),
 * which the backend splits via the same parser the scanner uses.
 */
export interface TrackEdit {
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  year?: number | null;
  track_number?: number | null;
  disc_number?: number | null;
  genre?: string | null;
}

/** Persist the edited tags both to the audio file and to the database. */
export function updateTrackTags(
  trackId: number,
  edit: TrackEdit,
): Promise<void> {
  return invoke<void>("update_track_tags", { trackId, edit });
}

/** Summary returned by `updateTracksBatch`. */
export interface BatchUpdateSummary {
  updated: number;
  /** `[trackId, reason]` for each track that couldn't be updated. */
  errors: [number, string][];
}

/**
 * Apply the same `TrackEdit` to every track in `trackIds`. Per-track
 * failures don't abort the batch — they land in `errors`. Useful for
 * bulk-renaming an album / setting genre on a selection / etc.
 */
export function updateTracksBatch(
  trackIds: number[],
  edit: TrackEdit,
): Promise<BatchUpdateSummary> {
  return invoke<BatchUpdateSummary>("update_tracks_batch", { trackIds, edit });
}

/**
 * Replace the embedded cover for a track. The image is written into
 * the audio file's tag AND copied into the per-profile artwork
 * cache. Cover is per-album in WaveFlow's data model so this also
 * repaints every sibling track sharing the same album.
 */
export function updateTrackCover(
  trackId: number,
  imagePath: string,
): Promise<void> {
  return invoke<void>("update_track_cover", { trackId, imagePath });
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
