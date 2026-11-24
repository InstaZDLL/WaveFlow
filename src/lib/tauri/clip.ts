import { invoke } from "./invoke";

/** Where a track's music video was found. Mirrors
 *  `waveflow_core::scanner::clips::ClipOrigin`. */
export type ClipOrigin = "sibling" | "clips_dir" | "itself";

/** A track's music video (#1073), resolved to a local absolute path.
 *  Mirrors `commands::clip::TrackClip` (camelCase). */
export interface TrackClip {
  localPath: string;
  origin: ClipOrigin;
}

/** A library track's clip. Mirrors `commands::clip::LibraryTrackClip`. */
export type LibraryTrackClip =
  | ({ kind: "local" } & TrackClip)
  | { kind: "share"; url: string; origin: ClipOrigin };

/**
 * The video named like the track — beside it, in a `Clips` folder beside
 * it, or the track's own file when it is a video. Looked up at each call,
 * on the disk or on the share the track lies on (#1133); `null` when there
 * is none. A clip on a share comes with the loopback address the webview
 * plays it from. A server track asks {@link getRemoteTrackClip}.
 */
export function getTrackClip(
  trackId: number,
): Promise<LibraryTrackClip | null> {
  return invoke<LibraryTrackClip | null>("get_track_clip", { trackId });
}

/** A server track's clip. Mirrors `commands::clip::RemoteTrackClip`. */
export type RemoteTrackClip =
  ({ kind: "local" } & TrackClip) | { kind: "server" };

/**
 * A server track's music video (RFC-029 decision 9): the clip beside the
 * local file it was reconciled with when there is one, otherwise the
 * server's when the mirror says it has one. With `probe`, a track the mirror
 * knows nothing about is asked of the server; without, it has none.
 */
export function getRemoteTrackClip(
  remoteTrackId: string,
  probe: boolean,
): Promise<RemoteTrackClip | null> {
  return invoke<RemoteTrackClip | null>("get_remote_track_clip", {
    remoteTrackId,
    probe,
  });
}

/** Where an open clip plays from: a file on this machine, a file on a share
 *  through the loopback server, or the server through a ticket minted as it
 *  opens. */
export type ClipSource =
  | { kind: "local"; localPath: string }
  | { kind: "share"; url: string }
  | { kind: "server"; remoteTrackId: string };

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
export type ClipControl = "play" | "pause" | "toggle" | "stop";

/**
 * The factor the clip of `trackId` multiplies the volume slider by: what
 * the engine puts on that track's samples besides the slider (its
 * ReplayGain, by the decoder's own formula, times the normalization), so
 * that switching to the clip keeps the level. Linear, above 1 for a boost.
 * `remote` is set for a server row. Asked again at each
 * `player:loudness-changed`.
 */
export function getClipGain(trackId: number, remote: boolean): Promise<number> {
  return invoke<number>("get_clip_gain", { trackId, remote });
}
