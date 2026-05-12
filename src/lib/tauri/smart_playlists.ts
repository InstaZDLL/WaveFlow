import { invoke } from "@tauri-apps/api/core";

/**
 * Trigger a regen of every Daily Mix slot from the active profile's
 * listening history. Resolves to the playlist ids in slot order
 * (1, 2, 3) — empty buckets are skipped, so the array length tells the
 * caller how many mixes are now available.
 *
 * Idempotent: a second call against the same listening data returns
 * the same ids and rewrites the same rows in place. Existing tracks
 * inside each Daily Mix are wiped and replaced — manual edits to a
 * smart playlist are NOT preserved.
 */
export function regenerateDailyMixes(): Promise<number[]> {
  return invoke<number[]>("regenerate_daily_mixes");
}

// ── Custom smart playlists ──────────────────────────────────────────

export type CustomSort =
  | "added_desc"
  | "added_asc"
  | "year_desc"
  | "year_asc"
  | "title_asc"
  | "artist_asc"
  | "random";

/** Editable rule set. Every field optional — empty rules match all tracks. */
export interface CustomRules {
  title_contains?: string | null;
  artist_contains?: string | null;
  album_contains?: string | null;
  genre_ids?: number[] | null;
  year_min?: number | null;
  year_max?: number | null;
  bpm_min?: number | null;
  bpm_max?: number | null;
  duration_min_ms?: number | null;
  duration_max_ms?: number | null;
  formats?: string[] | null;
  hi_res_only?: boolean | null;
  liked_only?: boolean | null;
  /** Minimum POPM rating 0-255. Map from 1-5 stars via `Math.round(stars / 5 * 255)`. */
  rating_min?: number | null;
  sort?: CustomSort | null;
  limit?: number | null;
}

export interface CustomSmartPlaylistInput {
  name: string;
  description?: string | null;
  color_id?: string | null;
  icon_id?: string | null;
  rules: CustomRules;
}

export interface CustomSmartPlaylistOutput {
  playlist_id: number;
  track_count: number;
}

export interface RulesPreview {
  total: number;
  track_ids: number[];
}

export function createCustomSmartPlaylist(
  input: CustomSmartPlaylistInput,
): Promise<CustomSmartPlaylistOutput> {
  return invoke<CustomSmartPlaylistOutput>("create_custom_smart_playlist", {
    input,
  });
}

export function updateCustomSmartPlaylist(
  playlistId: number,
  input: CustomSmartPlaylistInput,
): Promise<CustomSmartPlaylistOutput> {
  return invoke<CustomSmartPlaylistOutput>("update_custom_smart_playlist", {
    playlistId,
    input,
  });
}

export function regenerateCustomSmartPlaylist(
  playlistId: number,
): Promise<CustomSmartPlaylistOutput> {
  return invoke<CustomSmartPlaylistOutput>(
    "regenerate_custom_smart_playlist",
    { playlistId },
  );
}

export function getCustomSmartPlaylistRules(
  playlistId: number,
): Promise<CustomRules> {
  return invoke<CustomRules>("get_custom_smart_playlist_rules", {
    playlistId,
  });
}

export function previewCustomSmartPlaylist(
  rules: CustomRules,
): Promise<RulesPreview> {
  return invoke<RulesPreview>("preview_custom_smart_playlist", { rules });
}
