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

/** Why a copy stayed where it was. */
export type TrashRefusal =
  | { kind: "playing" }
  | { kind: "failed"; message: string }
  /** The copy kept for it is missing, changed, or the same file. */
  | { kind: "kept_copy_gone" }
  /** Not on disk: deleted by hand or on an unplugged drive. */
  | { kind: "missing" }
  /** In the trash, but its row could not be removed. */
  | { kind: "row_kept"; message: string };

export interface TrashFailure {
  track_id: number;
  file_path: string;
  reason: TrashRefusal;
}

export interface TrashReport {
  /** Copies moved to the trash, their rows removed with them. */
  trashed: number;
  /** Copies left in place, rows included. */
  failed: TrashFailure[];
}

/**
 * Move copies to the system's trash, then remove their rows (#878).
 * Only a copy whose file actually left loses its row, so a rescan
 * cannot bring it back and a refusal leaves the library as it was.
 */
export interface TrashGroup {
  keep_id: number;
  trash_ids: number[];
}

/**
 * Each group names the copy that stays: the backend checks, right before
 * moving a copy, that the kept one is still there and still identical.
 */
export function moveTracksToTrash(groups: TrashGroup[]): Promise<TrashReport> {
  return invoke<TrashReport>("move_tracks_to_trash", { groups });
}
