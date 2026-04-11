import { invoke } from "@tauri-apps/api/core";

/**
 * Mirror of `commands::player::PlayerStateSnapshot` — lock-free read
 * of the current engine state, returned by `player_get_state`.
 */
export interface PlayerStateSnapshot {
  state: "idle" | "loading" | "playing" | "paused" | "ended";
  position_ms: number;
  volume: number;
  sample_rate: number;
  channels: number;
}

/** Event payloads emitted by the Rust decoder thread. */
export interface PlayerPositionPayload {
  ms: number;
}
export interface PlayerStatePayload {
  state: "idle" | "loading" | "playing" | "paused" | "ended";
  track_id: number | null;
}
export interface PlayerTrackEndedPayload {
  track_id: number;
  completed: boolean;
  listened_ms: number;
}
export interface PlayerErrorPayload {
  message: string;
}

/** `queue_item.source_type` values the backend accepts. */
export type QueueSource =
  | "album"
  | "playlist"
  | "artist"
  | "library"
  | "liked"
  | "manual"
  | "radio";

export function playerGetState(): Promise<PlayerStateSnapshot> {
  return invoke<PlayerStateSnapshot>("player_get_state");
}

export function playerPause(): Promise<void> {
  return invoke<void>("player_pause");
}

export function playerResume(): Promise<void> {
  return invoke<void>("player_resume");
}

export function playerStop(): Promise<void> {
  return invoke<void>("player_stop");
}

export function playerSeek(ms: number): Promise<void> {
  return invoke<void>("player_seek", { ms: Math.max(0, Math.round(ms)) });
}

/**
 * `value` must be in `[0, 1]`. The backend clamps but we round-trip
 * the UI's `[0, 100]` here for convenience so callers can pass raw
 * slider state.
 */
export function playerSetVolume(value01: number): Promise<void> {
  const clamped = Math.max(0, Math.min(1, value01));
  return invoke<void>("player_set_volume", { value: clamped });
}

/**
 * Replace the queue with `trackIds` and start playing at
 * `startIndex`. The backend validates that `startIndex` is in range.
 */
export function playerPlayTracks(
  sourceType: QueueSource,
  sourceId: number | null,
  trackIds: number[],
  startIndex: number
): Promise<void> {
  return invoke<void>("player_play_tracks", {
    sourceType,
    sourceId,
    trackIds,
    startIndex,
  });
}

export function playerNext(): Promise<void> {
  return invoke<void>("player_next");
}

export function playerPrevious(): Promise<void> {
  return invoke<void>("player_previous");
}
