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
 * Arm or disarm the backend's "pause when the current track ends"
 * flag. Used by the sleep timer's "end of current track" mode to
 * suppress the auto-advance step racing the frontend's pause call.
 * The flag is one-shot — consumed the next time a track ends
 * naturally — but disarming explicitly is supported for the cancel
 * path.
 */
export function playerSetPauseAfterTrack(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_pause_after_track", { enabled });
}

/** Snapshot of the A-B loop endpoints. `null` = unset. */
export interface AbLoopSnapshot {
  a_ms: number | null;
  b_ms: number | null;
}

/**
 * Configure the A-B loop. Pass `null` for either endpoint to leave
 * that side untouched; pass both as `null` to disarm. The backend
 * only loops when both are set AND `a_ms < b_ms`.
 */
export function playerSetAbLoop(
  aMs: number | null,
  bMs: number | null,
): Promise<AbLoopSnapshot> {
  return invoke<AbLoopSnapshot>("player_set_ab_loop", {
    aMs,
    bMs,
  });
}

/** Drop both A-B loop endpoints. */
export function playerClearAbLoop(): Promise<AbLoopSnapshot> {
  return invoke<AbLoopSnapshot>("player_clear_ab_loop");
}

/** Read the current A-B loop. Used to hydrate UI state on mount. */
export function playerGetAbLoop(): Promise<AbLoopSnapshot> {
  return invoke<AbLoopSnapshot>("player_get_ab_loop");
}

/**
 * Replace the queue with `trackIds` and start playing at
 * `startIndex`. The backend validates that `startIndex` is in range.
 */
export function playerPlayTracks(
  sourceType: QueueSource,
  sourceId: number | null,
  trackIds: number[],
  startIndex: number,
): Promise<void> {
  return invoke<void>("player_play_tracks", {
    sourceType,
    sourceId,
    trackIds,
    startIndex,
  });
}

export interface PlayUrlArgs {
  url: string;
  title?: string;
  artist?: string;
  /** Cover URL (Deezer / radio-browser) — passed through to the
   *  `player:radio-metadata` event for the PlayerBar to render. */
  artworkUrl?: string;
  /** Optional codec hint forwarded to the symphonia probe. */
  extHint?: string;
}

/**
 * Play a live HTTP(S) audio stream through the cpal engine.
 * Returns the negative sentinel track id assigned to this session —
 * useful for distinguishing back-to-back radio loads.
 *
 * Distinct from `playerPlayTracks`: there's no queue insertion, no
 * library row, no `play_event` credit. Metadata supplied here drives
 * the PlayerBar / OS overlay via the `player:radio-metadata` event.
 */
export function playerPlayUrl(args: PlayUrlArgs): Promise<number> {
  return invoke<number>("player_play_url", {
    url: args.url,
    title: args.title,
    artist: args.artist,
    artworkUrl: args.artworkUrl,
    extHint: args.extHint,
  });
}

/**
 * Wire shape of the `player:radio-metadata` event AND the
 * `get_current_radio_metadata` snapshot. Two layers travel together:
 * the live **now playing** song (`title`/`artist`/`artwork_url`, from
 * ICY) and the stable **station identity** (`station_*`) the PlayerBar /
 * mini-player keep so the favorite star can save the station even after
 * a song title has overwritten the now-playing line. The favorite id is
 * `url:<station_url>`.
 */
export interface RadioMetadata {
  track_id: number;
  title: string | null;
  artist: string | null;
  artwork_url: string | null;
  station_url: string | null;
  station_name: string | null;
  station_artist: string | null;
  station_artwork: string | null;
}

/**
 * Snapshot the current radio session, or `null` when none is playing.
 * `player_get_state` can't carry radio (no library row), so a webview
 * that mounts mid-stream — the mini-player opened after a station
 * started — calls this to hydrate instead of waiting for the next ICY
 * `StreamTitle` change.
 */
export function getCurrentRadioMetadata(): Promise<RadioMetadata | null> {
  return invoke<RadioMetadata | null>("get_current_radio_metadata");
}

/**
 * Resolve album cover art for the currently-playing Web Radio song via
 * Deezer. `artist` + `title` come from the ICY `StreamTitle` split. The
 * backend returns a remote CDN URL (not cached to disk — the now-playing
 * line is ephemeral), or `null` when offline / no match / network error,
 * in which case the caller keeps the station favicon.
 */
export function fetchRadioArtwork(
  artist: string,
  title: string,
): Promise<string | null> {
  return invoke<string | null>("fetch_radio_artwork", { artist, title });
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
  /** Source label of the queue's first row, or `null` when empty.
   *  Drives the queue-wide "Radio based on X" banner. */
  source_type: QueueSource | null;
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
  replaygain: boolean;
  gapless: boolean;
  /** Active DSD → PCM FIR tap count (256 / 1024 / 2048). */
  dsd_taps: number;
}

/** Allowed DSD → PCM precision tiers (FIR tap counts). */
export const DSD_PRECISION_TAPS = [256, 1024, 2048] as const;
export type DsdPrecisionTaps = (typeof DSD_PRECISION_TAPS)[number];

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

export function playerSetReplayGain(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_replaygain", { enabled });
}

export function playerSetGapless(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_gapless", { enabled });
}

/**
 * Set the DSD → PCM converter precision (FIR tap count). Only affects
 * `.dsf` / `.dff` playback; symphonia formats ignore it. Takes effect on
 * the next track open. An out-of-set value is coerced to 256 by the
 * backend. Persisted in `profile_setting['audio.dsd_precision']`.
 */
export function playerSetDsdPrecision(taps: DsdPrecisionTaps): Promise<void> {
  return invoke<void>("player_set_dsd_precision", { taps });
}

/**
 * Update playback speed. Clamped to `[0.5, 2.0]` on the engine side;
 * out-of-range values are saturated. Pitch is NOT preserved — 1.5×
 * lifts the pitch by ~7 semitones (resampler-shift, same as VLC's
 * default playback rate).
 */
export function playerSetSpeed(value: number): Promise<void> {
  return invoke<void>("player_set_speed", { value });
}

export function playerGetSpeed(): Promise<number> {
  return invoke<number>("player_get_speed");
}

// ── Output device picker ───────────────────────────────────────────

/**
 * Mirrors `commands::player::OutputDeviceRow`. `id` is the cpal
 * device name (cpal does not surface stable IDs across hosts) and
 * doubles as the value passed back to `playerSetOutputDevice`.
 * `is_active` flags the device the engine is currently driving;
 * `is_default` flags the OS default device so the UI can show
 * something like "(System default)" next to it.
 */
export interface OutputDevice {
  id: string;
  name: string;
  is_default: boolean;
  is_active: boolean;
}

export function playerListOutputDevices(): Promise<OutputDevice[]> {
  return invoke<OutputDevice[]>("player_list_output_devices");
}

/**
 * `deviceId = null` (or `undefined`) means "follow the OS default".
 * The backend pauses, releases the old device, opens the new one,
 * and resumes the same track at the same position.
 */
export function playerSetOutputDevice(deviceId: string | null): Promise<void> {
  return invoke<void>("player_set_output_device", { deviceId });
}

/**
 * Toggle WASAPI Exclusive Mode (Windows only). The backend persists
 * the value across platforms but only re-opens the output stream on
 * Windows. Falls back to cpal shared if exclusive init fails (device
 * busy, no exclusive format support).
 */
export function playerSetWasapiExclusive(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_wasapi_exclusive", { enabled });
}

/**
 * Read whether WASAPI Exclusive Mode is currently engaged. Always
 * `false` on Linux / macOS. Useful for the Settings card to show
 * what's actually active (a failed exclusive init silently falls
 * back to shared, so the toggle could be on but the mode off).
 */
export function playerGetWasapiExclusive(): Promise<boolean> {
  return invoke<boolean>("player_get_wasapi_exclusive");
}
