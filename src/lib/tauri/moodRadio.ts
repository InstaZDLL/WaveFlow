import { invoke } from "@tauri-apps/api/core";

export type Mood = "focus" | "chill" | "workout" | "party" | "sleep";

export interface MoodCounts {
  focus: number;
  chill: number;
  workout: number;
  party: number;
  sleep: number;
}

/**
 * Build a mood-based radio queue (~40 tracks). Filters by BPM range
 * and an optional LUFS ceiling pulled from `track_analysis`. Hand the
 * result to `playerPlayTracks("radio", null, ids, 0)` to play it.
 *
 * Returns an empty array if no analysed track matches the mood (the
 * UI should disable the corresponding tile when the count is zero).
 */
export function startMoodRadio(mood: Mood): Promise<number[]> {
  return invoke<number[]>("start_mood_radio", { mood });
}

/** How many qualifying tracks each mood would yield, given the
 * library's current state of BPM/loudness analysis. */
export function moodRadioCounts(): Promise<MoodCounts> {
  return invoke<MoodCounts>("mood_radio_counts");
}
