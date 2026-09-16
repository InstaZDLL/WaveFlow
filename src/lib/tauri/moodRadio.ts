import { invoke } from "@tauri-apps/api/core";

export type Mood = "focus" | "chill" | "workout" | "party" | "sleep";

export interface MoodCounts {
  focus: number;
  chill: number;
  workout: number;
  party: number;
  sleep: number;
  /** Tracks carrying a tempo measurement — what every mood draws from. */
  analysed_tracks: number;
  /** Playable tracks in the library, analysed or not. */
  total_tracks: number;
}

export interface MoodRadioSession {
  trackIds: number[];
  /**
   * Whole records in their own order, rather than individual tracks.
   *
   * What the backend actually produced: album mode is a preference and
   * a mood with no qualifying record falls back to tracks on purpose,
   * so this is `false` on that path even with the setting on. Pass it
   * to `playerPlayTracks` — it is what tells automatic ReplayGain the
   * session is a record playing through (#647).
   */
  albumOrdered: boolean;
}

/**
 * Build a mood-based radio queue (~40 tracks).
 *
 * The tempo range gates what may be considered; loudness and genre
 * then **rank** it, so the queue is the best-fitting forty of the pool
 * rather than the first forty drawn (#616). Hand the result to
 * `playerPlayTracks("radio", null, session.trackIds, 0,
 * session.albumOrdered)` to play it.
 *
 * `trackIds` is empty if no analysed track matches the mood (the UI
 * should disable the corresponding tile when the count is zero).
 */
export function startMoodRadio(mood: Mood): Promise<MoodRadioSession> {
  return invoke<MoodRadioSession>("start_mood_radio", { mood });
}

/**
 * How many tracks each mood could draw from, plus how much of the
 * library has been analysed at all.
 *
 * The per-mood numbers answer the tempo gate only — loudness and genre
 * rank rather than exclude, so they cannot make a mood empty. The
 * coverage pair is what lets the UI say *why* a mood is thin instead
 * of leaving the user to guess.
 */
export function moodRadioCounts(): Promise<MoodCounts> {
  return invoke<MoodCounts>("mood_radio_counts");
}
