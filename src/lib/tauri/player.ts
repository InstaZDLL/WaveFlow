import { invoke } from "@tauri-apps/api/core";

/**
 * Subset of a track sent back by `player_get_state`. Matches the
 * fields needed by the PlayerBar (not the full `Track` row).
 */
export interface QueueTrackPayload {
  id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  artist_ids: string | null;
  album_title: string | null;
  duration_ms: number;
  file_path: string;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Quality fields used by the PlayerBar footer + Hi-Res badge. */
  bitrate: number | null;
  sample_rate: number | null;
  channels: number | null;
  bit_depth: number | null;
  codec: string | null;
  file_size: number;
}

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
  shuffle: boolean;
  repeat_mode: "off" | "all" | "one";
  current_track: QueueTrackPayload | null;
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

/** Append `trackIds` to the end of the queue, no playback interruption. */
export function playerAddToQueue(trackIds: number[]): Promise<void> {
  return invoke<void>("player_add_to_queue", { trackIds });
}

/** Insert `trackIds` immediately after the currently-playing slot. */
export function playerPlayNext(trackIds: number[]): Promise<void> {
  return invoke<void>("player_play_next", { trackIds });
}

/**
 * Move the queue item at `fromPosition` to `toPosition`. The backend
 * shifts the surrounding items so positions stay dense and adjusts
 * `queue.current_index` so the playing track keeps playing.
 */
export function playerReorderQueue(
  fromPosition: number,
  toPosition: number,
): Promise<void> {
  return invoke<void>("player_reorder_queue", {
    fromPosition,
    toPosition,
  });
}

export function playerPrevious(): Promise<void> {
  return invoke<void>("player_previous");
}

/** Returns the new shuffle state (true = shuffled). */
export function playerToggleShuffle(): Promise<boolean> {
  return invoke<boolean>("player_toggle_shuffle");
}

/** Returns the new repeat mode. */
export function playerCycleRepeat(): Promise<"off" | "all" | "one"> {
  return invoke<"off" | "all" | "one">("player_cycle_repeat");
}

/** Resume from the persisted last-track + position. */
export function playerResumeLast(): Promise<void> {
  return invoke<void>("player_resume_last");
}

/** Live playback queue returned by `player_get_queue`. */
export interface PlayerQueueSnapshot {
  current_index: number;
  items: QueueTrackPayload[];
}

export function playerGetQueue(): Promise<PlayerQueueSnapshot> {
  return invoke<PlayerQueueSnapshot>("player_get_queue");
}

/** Jump the queue cursor to an arbitrary position and play from there. */
export function playerJumpToIndex(position: number): Promise<void> {
  return invoke<void>("player_jump_to_index", { position });
}

// ── Audio settings ──────────────────────────────────────────────────

export interface AudioSettingsSnapshot {
  normalize: boolean;
  mono: boolean;
  crossfade_ms: number;
}

export function playerGetAudioSettings(): Promise<AudioSettingsSnapshot> {
  return invoke<AudioSettingsSnapshot>("player_get_audio_settings");
}

export function playerSetNormalize(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_normalize", { enabled });
}

export function playerSetMono(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_mono", { enabled });
}

export function playerSetCrossfade(seconds: number): Promise<void> {
  return invoke<void>("player_set_crossfade", { seconds });
}
