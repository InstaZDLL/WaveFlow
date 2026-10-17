import { invoke } from "@tauri-apps/api/core";

/** What a file is, as the confirmation shows it. */
export interface FileFacts {
  path: string;
  codec: string | null;
  duration_ms: number;
  sample_rate: number | null;
  bit_depth: number | null;
}

/** Why a replacement cannot go ahead. */
export type ReplaceRefusal =
  | { kind: "playing" }
  | { kind: "same_file" }
  | { kind: "not_audio" }
  | { kind: "duration_mismatch"; current_ms: number; replacement_ms: number }
  | { kind: "target_taken"; path: string };

export interface ReplacementCheck {
  current: FileFacts;
  replacement: FileFacts | null;
  /** The old file's folder and name, with the new file's extension. */
  target_path: string;
  /** The chosen file already sits at the target: adopted in place. Its
   *  own library entry, and that entry's short history, is removed; this
   *  track takes its file. */
  adopts: boolean;
  refusal: ReplaceRefusal | null;
}

export type OldFileFate =
  { kind: "trashed" } | { kind: "kept_aside"; path: string; message: string };

/**
 * What a confirmed replacement did. `refused` when a check that passed
 * for the confirmation no longer does (the track started playing).
 */
export type ReplaceOutcome =
  | { kind: "replaced"; file_path: string; old_file: OldFileFate }
  | { kind: "refused"; refusal: ReplaceRefusal };

/** What replacing the track's file with `sourcePath` would do. */
export function checkTrackReplacement(
  trackId: number,
  sourcePath: string,
): Promise<ReplacementCheck> {
  return invoke<ReplacementCheck>("check_track_replacement", {
    trackId,
    sourcePath,
  });
}

/**
 * Swap the track's file for `sourcePath`, keeping the track — its plays,
 * likes and playlists. The new file takes the old one's place and the
 * old one goes to the system's trash.
 */
export function replaceTrackFile(
  trackId: number,
  sourcePath: string,
): Promise<ReplaceOutcome> {
  return invoke<ReplaceOutcome>("replace_track_file", {
    trackId,
    sourcePath,
  });
}
