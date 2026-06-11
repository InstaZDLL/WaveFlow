import type { Track } from "./tauri/track";

/**
 * Discriminate a Web Radio session from a library track and from
 * Spotify. Centralised here so the contract stays in one place — every
 * UI surface that needs to gate behaviour on "is this a live radio
 * stream" (PlayerBar cover icon, PlaybackControls disabling
 * Previous/Next/Shuffle/Repeat, future Now Playing surfaces) imports
 * from here instead of pattern-matching display fields ad-hoc.
 *
 * The contract uses BOTH invariants together so a single drift
 * elsewhere can't break the check:
 *
 * - `id < 0` is the backend sentinel for non-library tracks. Assigned
 *   by `commands::player::next_radio_track_id` (negative monotonic
 *   counter) for radio, and by `spotifyTrackToTrack` (constant `-1`)
 *   for Spotify — so `id < 0` alone can't tell them apart.
 *
 * - `codec === "Web Radio"` is the discriminator. Hardcoded by
 *   `radioMetadataToTrack` in `PlayerContext.tsx`; the equivalent
 *   field for Spotify is `"Spotify"`, for local tracks it's `null`
 *   or a real codec name (`"FLAC"`, `"MP3"`).
 *
 * If either invariant ever changes, update this helper — every caller
 * picks up the new check automatically.
 */
export function isRadioTrack(track: Track | null): boolean {
  if (track === null) return false;
  return track.id < 0 && track.codec === "Web Radio";
}
