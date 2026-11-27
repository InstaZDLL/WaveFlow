import { invoke } from "./invoke";

/** Why the last WaveSync session ended. */
export type WaveSyncEndReason =
  | "ended"
  | "left"
  | "hostEnded"
  | "hostLeft"
  | "removed"
  | "refused"
  | "offline"
  | "profileChanged"
  | "diverged";

export interface WaveSyncParticipant {
  id: number;
  name: string;
  host: boolean;
  me: boolean;
  state: "listening" | "buffering" | "notFollowing" | "away";
}

export interface WaveSyncEntry {
  id: string;
  title: string;
  artists: string[];
  album: string | null;
  durationMs: number;
  ownerId: number;
  ownerName: string | null;
  mine: boolean;
  /** The library track, for this machine's own entries only. */
  trackId: number | null;
  state: "played" | "current" | "upcoming" | "ownerAway";
}

/** What the backend says about the session (`wavesync:changed`). */
export interface WaveSyncView {
  status: "idle" | "waiting" | "active";
  role: "host" | "guest" | null;
  endedReason: WaveSyncEndReason | null;
  sessionName: string | null;
  me: number | null;
  code: string | null;
  fingerprint: string | null;
  invitation: string | null;
  participants: WaveSyncParticipant[];
  queue: WaveSyncEntry[];
  pending: { request: number; name: string }[];
  approval: boolean;
  wrongCodes: number;
  startedAtMs: number | null;
  everJoined: boolean;
}

export interface WaveSyncDiscovered {
  name: string;
  host: string;
  address: string;
  fingerprint: string | null;
}

export const WAVESYNC_CHANGED_EVENT = "wavesync:changed";

export function wavesyncGetView(): Promise<WaveSyncView> {
  return invoke<WaveSyncView>("wavesync_get_view");
}

export function wavesyncStart(
  name: string,
  displayName: string,
  approval: boolean,
): Promise<WaveSyncView> {
  return invoke<WaveSyncView>("wavesync_start", {
    name,
    displayName,
    approval,
  });
}

export function wavesyncDiscover(): Promise<WaveSyncDiscovered[]> {
  return invoke<WaveSyncDiscovered[]>("wavesync_discover");
}

export function wavesyncJoin(
  target: string,
  fingerprint: string | null,
  code: string | null,
  displayName: string,
): Promise<WaveSyncView> {
  return invoke<WaveSyncView>("wavesync_join", {
    target,
    fingerprint,
    code,
    displayName,
  });
}

export function wavesyncLeave(): Promise<void> {
  return invoke<void>("wavesync_leave");
}

export function wavesyncDismiss(): Promise<void> {
  return invoke<void>("wavesync_dismiss");
}

export function wavesyncApprove(
  request: number,
  accept: boolean,
): Promise<void> {
  return invoke<void>("wavesync_approve", { request, accept });
}

export function wavesyncNewCode(): Promise<void> {
  return invoke<void>("wavesync_new_code");
}

export function wavesyncRemoveParticipant(participant: number): Promise<void> {
  return invoke<void>("wavesync_remove_participant", { participant });
}

export function wavesyncAdd(
  trackIds: number[],
): Promise<{ added: number; skipped: number }> {
  return invoke<{ added: number; skipped: number }>("wavesync_add", {
    trackIds,
  });
}

export function wavesyncRemove(entry: string): Promise<void> {
  return invoke<void>("wavesync_remove", { entry });
}
