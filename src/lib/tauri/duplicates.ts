import { invoke } from "@tauri-apps/api/core";

export interface DuplicateTrack {
  id: number;
  title: string;
  artist_name: string | null;
  album_title: string | null;
  file_path: string;
  file_size: number;
  bitrate: number | null;
  sample_rate: number | null;
  duration_ms: number;
  added_at: number;
}

export interface DuplicateGroup {
  file_hash: string;
  tracks: DuplicateTrack[];
}

/** Group every available track by `file_hash`, returning the buckets of size > 1. */
export function findDuplicates(): Promise<DuplicateGroup[]> {
  return invoke<DuplicateGroup[]>("find_duplicates");
}

/**
 * Drop a list of tracks from the database. Audio files on disk are
 * left untouched — the user can clean them up via the OS. Returns
 * the actual count of rows deleted.
 */
export function deleteTracks(trackIds: number[]): Promise<number> {
  return invoke<number>("delete_tracks", { trackIds });
}
