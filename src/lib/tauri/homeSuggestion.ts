import { invoke } from "@tauri-apps/api/core";
import type { Track } from "./track";

/** Why Home suggests a track — see `waveflow_core::home_suggestion`. */
export type SuggestionKind = "forgotten" | "vibe" | "unplayed";

export interface HomeSuggestion {
  kind: SuggestionKind;
  track: Track;
  /** Forgotten: plays that counted, all time. */
  plays: number | null;
  /** Forgotten: the last of them, in ms. */
  lastPlayedAt: number | null;
  /** Vibe: the recently played artist the pick comes from. */
  anchorArtist: string | null;
  /** Unplayed: plays of the track's artist, all time. */
  artistPlays: number | null;
  /** Unplayed: when the track was added, in ms. */
  addedAt: number | null;
}

/**
 * Home's suggestion for `day` after `attempt` clicks on "Another
 * suggestion": the same pair always gives the same track. `null` when the
 * library has nothing to suggest.
 */
export function getHomeSuggestion(
  day: number,
  attempt: number,
): Promise<HomeSuggestion | null> {
  return invoke<HomeSuggestion | null>("get_home_suggestion", {
    day,
    attempt,
  });
}

/** Days since the epoch for the user's calendar date, not UTC's. */
export function localDayIndex(date: Date): number {
  return Math.floor(
    Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000,
  );
}

/** Which draw Home is on: the day it was made and the clicks since. */
export interface SuggestionDraw {
  day: number;
  attempt: number;
}

export const SUGGESTION_DRAW_KEY = "home.suggestion_draw";
export const DEFAULT_SUGGESTION_DRAW: SuggestionDraw = { day: 0, attempt: 0 };

export function parseSuggestionDraw(raw: string | null): SuggestionDraw {
  if (!raw) return DEFAULT_SUGGESTION_DRAW;
  try {
    const value = JSON.parse(raw) as Partial<SuggestionDraw>;
    if (
      Number.isInteger(value.day) &&
      Number.isInteger(value.attempt) &&
      (value.attempt as number) >= 0
    ) {
      return { day: value.day as number, attempt: value.attempt as number };
    }
  } catch {
    // A corrupt value is the default: the day's first suggestion.
  }
  return DEFAULT_SUGGESTION_DRAW;
}

export function serializeSuggestionDraw(draw: SuggestionDraw): string {
  return JSON.stringify(draw);
}
