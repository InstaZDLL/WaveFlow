import type { Track } from "./tauri/track";

/**
 * Discriminate a Web Radio session from a library track and from the
 * remote queue. Centralised here so the contract stays in one place — every
 * UI surface that needs to gate behaviour on "is this a live radio
 * stream" (PlayerBar cover icon, PlaybackControls disabling
 * Previous/Next/Shuffle/Repeat, future Now Playing surfaces) imports
 * from here instead of pattern-matching display fields ad-hoc.
 *
 * The contract uses BOTH invariants together so a single drift
 * elsewhere can't break the check:
 *
 * - `id < 0` is the backend sentinel for non-library tracks. It is
 *   assigned by `commands::player::next_radio_track_id` (a negative
 *   monotonic counter) for radio AND for remote-queue tracks, so
 *   `id < 0` alone can't tell those two apart.
 *
 * - `codec === "Web Radio"` is the discriminator. Hardcoded by
 *   `radioMetadataToTrack` in `PlayerContext.tsx`; the equivalent
 *   field for a remote track is `"Remote server"`, and for local
 *   tracks it's `null` or a real codec name (`"FLAC"`, `"MP3"`).
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
 * A file the user opened without importing it. It plays under the same
 * negative sentinel id, and is the one such source with a `file_path`
 * (`radioMetadataToTrack` leaves it empty for radio and remote tracks).
 * Like a remote track it is one entry of a queue that advances: Previous /
 * Next / Repeat stay on, Shuffle off.
 */
export function isFileTrack(track: Track | null): boolean {
  if (track === null) return false;
  return track.id < 0 && track.file_path !== "";
}

/**
 * Any track with no library row — radio, a remote-queue track, a file
 * played without importing it. Surfaces that gate a library affordance
 * (♥ like, artist slideshow, "go to album", start radio) on
 * `!isRadioTrack` should use this instead, or they light those up where
 * they would fail. The transport controls are the deliberate exception:
 * remote tracks and files advance, so they check the kinds apart.
 */
export function isStreamTrack(track: Track | null): boolean {
  return isRadioTrack(track) || isRemoteTrack(track) || isFileTrack(track);
}
