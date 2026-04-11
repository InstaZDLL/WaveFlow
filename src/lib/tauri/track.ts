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
  album_title: string | null;
  artist_name: string | null;
  duration_ms: number;
  track_number: number | null;
  disc_number: number | null;
  year: number | null;
  bitrate: number | null;
  sample_rate: number | null;
  channels: number | null;
  file_path: string;
  file_size: number;
  added_at: number;
  artwork_path: string | null;
}

export function listTracks(libraryId: number): Promise<Track[]> {
  return invoke<Track[]>("list_tracks", { libraryId });
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
