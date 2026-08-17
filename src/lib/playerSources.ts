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

/**
 * A track from the remote play queue (RFC-005). Shares the negative-id
 * URL-stream path with radio but is one entry of a queue that advances, so
 * it must NOT inherit radio's Previous/Next disabling — those drive the
 * remote queue in the backend. Same two-invariant contract as
 * {@link isRadioTrack}; the `codec` sentinel is `"Remote server"`, set by
 * `radioMetadataToTrack` from the event's `is_remote` flag.
 *
 * Shuffle stays off for it (the remote queue has no shuffle yet); Previous
 * / Next / Repeat are on.
 */
export function isRemoteTrack(track: Track | null): boolean {
  if (track === null) return false;
  return track.id < 0 && track.codec === "Remote server";
}

/**
 * Either kind of URL stream — radio or a remote-queue track. Neither has a
 * library row, so surfaces that gate a library affordance (♥ like, artist
 * slideshow, "go to album") on `!isRadioTrack` should use this instead, or
 * they light those up for remote tracks where they would fail. The
 * transport controls are the deliberate exception: a remote track advances,
 * so it checks the two apart.
 */
export function isStreamTrack(track: Track | null): boolean {
  return isRadioTrack(track) || isRemoteTrack(track);
}
