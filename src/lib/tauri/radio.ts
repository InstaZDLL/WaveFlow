import { invoke } from "@tauri-apps/api/core";

/**
 * Build a Spotify-style radio queue seeded by the given track.
 * Returns ~40 ordered track IDs: the seed first, a handful of other
 * tracks by the seed's primary artist, then tracks from similar
 * artists (resolved via the cached Last.fm / Deezer suggestions).
 *
 * Hand the result to `playerPlayTracks("radio", null, ids, 0)` to
 * play it immediately.
 */
export function startRadio(seedTrackId: number): Promise<number[]> {
  return invoke<number[]>("start_radio", { seedTrackId });
}
