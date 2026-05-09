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
