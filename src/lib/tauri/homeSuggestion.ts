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

/** A suggestion as kept for its day: the track id instead of the track. */
export type PinnedSuggestion = Omit<HomeSuggestion, "track"> & {
  trackId: number;
};

export function pinSuggestion(suggestion: HomeSuggestion): PinnedSuggestion {
  const { track, ...rest } = suggestion;
  return { ...rest, trackId: track.id };
}

/**
 * Home's suggestion for `day` after `attempt` clicks on "Another
 * suggestion". `pinned`, the suggestion this pair already gave, comes back
 * as long as its track is available; otherwise a new draw is made. `null`
 * when the library has nothing to suggest.
 */
export function getHomeSuggestion(
  day: number,
  attempt: number,
  pinned: PinnedSuggestion | null,
): Promise<HomeSuggestion | null> {
  return invoke<HomeSuggestion | null>("get_home_suggestion", {
    day,
    attempt,
    pinned,
  });
}

/** Days since the epoch for the user's calendar date, not UTC's. */
export function localDayIndex(date: Date): number {
  return Math.floor(
    Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000,
  );
}

/**
 * Which draw Home is on: the day it was made, the clicks since, and the
 * suggestion it gave, so the same track comes back all day even once it
 * has been played and no longer qualifies.
 */
export interface SuggestionDraw {
  /**
   * The profile the draw was made for. On a profile switch the setting
   * hook still holds the outgoing profile's value for a render; the card
   * must not hand that draw to the new profile's database, where its
   * track id may name another track.
   */
  profileId: number | null;
  day: number;
  attempt: number;
  pinned: PinnedSuggestion | null;
}

export const SUGGESTION_DRAW_KEY = "home.suggestion_draw";
export const DEFAULT_SUGGESTION_DRAW: SuggestionDraw = {
  profileId: null,
  day: 0,
  attempt: 0,
  pinned: null,
};

const KINDS: readonly SuggestionKind[] = ["forgotten", "vibe", "unplayed"];

function numberOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function parsePinned(value: unknown): PinnedSuggestion | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (
    !KINDS.includes(v.kind as SuggestionKind) ||
    !Number.isInteger(v.trackId)
  ) {
    return null;
  }
  return {
    kind: v.kind as SuggestionKind,
    trackId: v.trackId as number,
    plays: numberOrNull(v.plays),
    lastPlayedAt: numberOrNull(v.lastPlayedAt),
    anchorArtist: typeof v.anchorArtist === "string" ? v.anchorArtist : null,
    artistPlays: numberOrNull(v.artistPlays),
    addedAt: numberOrNull(v.addedAt),
  };
}

export function parseSuggestionDraw(raw: string | null): SuggestionDraw {
  if (!raw) return DEFAULT_SUGGESTION_DRAW;
  try {
    const value = JSON.parse(raw) as Partial<
      Record<keyof SuggestionDraw, unknown>
    >;
    if (
      Number.isInteger(value.day) &&
      Number.isInteger(value.attempt) &&
      (value.attempt as number) >= 0
    ) {
      return {
        profileId: Number.isInteger(value.profileId)
          ? (value.profileId as number)
          : null,
        day: value.day as number,
        attempt: value.attempt as number,
        pinned: parsePinned(value.pinned),
      };
    }
  } catch {
    // A corrupt value is the default: the day's first suggestion.
  }
  return DEFAULT_SUGGESTION_DRAW;
}

export function serializeSuggestionDraw(draw: SuggestionDraw): string {
  return JSON.stringify(draw);
}
