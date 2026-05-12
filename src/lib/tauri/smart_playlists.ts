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

/**
 * One leaf in the rule tree. `hi_res` and `liked` are unit predicates
 * (no `value` field). `rating_min` carries a POPM byte (0-255); the
 * editor maps stars via `Math.round(stars / 5 * 255)`.
 */
export type Predicate =
  | { kind: "title_contains"; value: string }
  | { kind: "artist_contains"; value: string }
  | { kind: "album_contains"; value: string }
  | { kind: "genre_is"; value: number }
  | { kind: "year_min"; value: number }
  | { kind: "year_max"; value: number }
  | { kind: "bpm_min"; value: number }
  | { kind: "bpm_max"; value: number }
  | { kind: "duration_min_ms"; value: number }
  | { kind: "duration_max_ms"; value: number }
  | { kind: "format"; value: string }
  | { kind: "hi_res" }
  | { kind: "liked" }
  | { kind: "rating_min"; value: number };

export type PredicateKind = Predicate["kind"];

/**
 * Recursive rule tree. Group ops (`all`/`any`) hold children; `not`
 * wraps a single child; `leaf` carries one predicate. An empty `all`
 * matches every available track and is the canonical "blank" root.
 */
export type RuleNode =
  | { type: "all"; children: RuleNode[] }
  | { type: "any"; children: RuleNode[] }
  | { type: "not"; child: RuleNode }
  | { type: "leaf"; predicate: Predicate };

/** Editable rule set. */
export interface CustomRules {
  tree: RuleNode;
  sort?: CustomSort | null;
  limit?: number | null;
}

export function emptyTree(): RuleNode {
  return { type: "all", children: [] };
}

export function emptyRules(): CustomRules {
  return { tree: emptyTree(), sort: null, limit: null };
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
  return invoke<CustomSmartPlaylistOutput>("regenerate_custom_smart_playlist", {
    playlistId,
  });
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
