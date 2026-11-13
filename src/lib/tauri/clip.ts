import { invoke } from "@tauri-apps/api/core";

/** Where a track's music video was found. Mirrors
 *  `waveflow_core::scanner::clips::ClipOrigin`. */
export type ClipOrigin = "sibling" | "clips_dir" | "itself";

/** A track's music video (#1073), resolved to a local absolute path.
 *  Mirrors `commands::clip::TrackClip` (camelCase). */
export interface TrackClip {
  localPath: string;
  origin: ClipOrigin;
}

/**
 * The video named like the track — beside it, in a `Clips` folder beside
 * it, or the track's own file when it is a video. Looked up on the disk
 * at each call; `null` when there is none, and always for a track on a
 * network share or from the server.
 */
export function getTrackClip(trackId: number): Promise<TrackClip | null> {
  return invoke<TrackClip | null>("get_track_clip", { trackId });
}

/** The flag's writes in flight, one after the other. */
let clipOpenChain: Promise<void> = Promise.resolve();

/**
 * Tell the backend a clip is open (or closed), so a play / pause from the
 * media keys, the tray, the taskbar or a global shortcut drives the clip
 * (a `clip:control` event) rather than starting the track under it.
 */
export function playerSetClipOpen(open: boolean): Promise<void> {
  // One after the other: a clip closed and the next one opened at once
  // must not land as "open" then "closed", or the media keys would drive
  // the track under the new clip.
  const request = clipOpenChain
    .catch(() => undefined)
    .then(() => invoke<void>("player_set_clip_open", { open }));
  clipOpenChain = request;
  return request;
}

/** Payload of the `clip:control` event. */
export type ClipControl = "play" | "pause" | "toggle";
