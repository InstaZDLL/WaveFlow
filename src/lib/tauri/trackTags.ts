import { invoke } from "@tauri-apps/api/core";

/** A custom tag key the library holds, and how many tracks carry it. */
export interface TrackTagKey {
  key: string;
  count: number;
}

/**
 * Which custom tag keys the user's files actually carry (#588).
 *
 * The count is what makes the picker usable: it separates the tag on
 * every track from the one on three. Only moves when a scan has run, so
 * it is read once per library change rather than per render.
 */
export function listTrackTagKeys(): Promise<TrackTagKey[]> {
  return invoke<TrackTagKey[]>("list_track_tag_keys");
}

/**
 * Values of the chosen tag keys, for every track at once.
 *
 * Keyed by track id (as text, matching the listing's ids) then by tag
 * key. Called only while a `tag:` column is shown — the values are not
 * part of the listing query, because its column list has to stay in
 * step with a struct and a variable number of joined columns cannot.
 */
export function listTrackTagValues(
  keys: string[],
): Promise<Record<string, Record<string, string>>> {
  return invoke<Record<string, Record<string, string>>>(
    "list_track_tag_values",
    { keys },
  );
}
