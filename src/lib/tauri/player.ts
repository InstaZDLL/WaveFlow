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
  /** How shuffle groups the queue — see {@link ShuffleMode}. */
  shuffle_mode: ShuffleMode;
  repeat_mode: "off" | "all" | "one";
  current_track: QueueTrackPayload | null;
  /** True when the output is shipping native DSD via DoP (#495). */
  dop_active: boolean;
  /** What the output really is right now (#597). */
  output_mode: OutputMode;
  /**
   * True when the stream really owns the device — WASAPI Exclusive on
   * Windows, a raw ALSA `hw:` device on Linux, CoreAudio hog mode on
   * macOS. This is what actually engaged, not the opt-in, so it is
   * false after a silent fallback to shared mode. What separates a
   * bit-perfect stream from one the system mixer re-clocks on its way
   * to the DAC.
   */
  exclusive_active: boolean;
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
  /** The technical message, for the console and for bug reports. */
  message: string;
  /**
   * What kind of failure this is, so the UI can say it in the user's
   * language instead of showing the message above (#597). Optional
   * because the UI must keep working against an older backend, and
   * unknown values fall back to the generic sentence.
   */
  kind?: string;
}

/**
 * Playback is not what the user asked for, but nothing failed (#597).
 *
 * A separate register from {@link PlayerErrorPayload}: losing the device
 * is a fault, falling back to shared mode is normal operation that
 * happens to contradict a choice. The engine emits each one once per
 * transition, not at every track.
 */
export interface PlayerNoticePayload {
  kind: string;
}

/**
 * What the output really is right now (#597) — computed by the engine so
 * the badge, the notice and the Settings card cannot disagree.
 */
export type OutputMode =
  | "shared"
  | "exclusive"
  | "dop"
  | "exclusive-refused";

/** `queue_item.source_type` values the backend accepts. */
export type QueueSource =
  "album" | "playlist" | "artist" | "library" | "liked" | "manual" | "radio";

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
  /**
   * `true` when this URL stream is a track from the remote play queue
   * (RFC-005), not a live radio station. Both ride the same event and a
   * negative id; this is what tells them apart, so the PlayerBar can keep
   * next / previous enabled and label the source "Remote server" rather
   * than "Web Radio".
   */
  is_remote: boolean;
  /** Track length in ms for a remote-queue entry, when known; `null` for
   *  live radio. Drives a bounded (but non-seekable) progress bar. */
  duration_ms: number | null;
  /** Artwork hash for a remote-queue track; `null` for radio. The frontend
   *  fetches it (Bearer-only) as a data URL for the PlayerBar cover. */
  artwork_hash: string | null;
  /** The track's identifier **on the server**, for the surfaces that have to
   *  name it there — the Canvas lookup above all. `track_id` cannot stand in:
   *  it is a negative sentinel minted per playback, meaningful only to this
   *  process. `null` for live radio, which has no server track. */
  remote_track_id: string | null;
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

/**
 * How shuffle reorders the queue (#618).
 *
 * `tracks` is the shuffle that has always existed. `albums` randomises
 * only the order of the records — inside each one the tracks keep disc
 * and track order, so an album still plays the way it was pressed.
 */
export type ShuffleMode = "off" | "tracks" | "albums";

/** The order the player control cycles through. */
export const SHUFFLE_MODES: readonly ShuffleMode[] = [
  "off",
  "tracks",
  "albums",
];

/** Returns the mode in force. */
export function playerSetShuffleMode(
  mode: ShuffleMode,
): Promise<ShuffleMode> {
  return invoke<ShuffleMode>("player_set_shuffle_mode", { mode });
}

/**
 * Turn shuffle on or off without choosing a grouping — what every
 * "Shuffle" button on an album, an artist or a playlist means.
 *
 * Returns the mode that ended up in force rather than a boolean:
 * turning shuffle on restores whichever grouping was last picked, and
 * only the backend knows which that was.
 */
export function playerToggleShuffle(): Promise<ShuffleMode> {
  return invoke<ShuffleMode>("player_toggle_shuffle");
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
  /** Pre-amp added to every track's ReplayGain, in dB. */
  replaygain_preamp_db: number;
  /** Gain used for tracks that carry none and were never analysed. */
  replaygain_fallback_db: number;
  /** Hold gains back to the headroom each track's peak leaves. */
  replaygain_prevent_clipping: boolean;
  /** Which of a file's two gains to apply — see {@link ReplayGainMode}. */
  replaygain_mode: ReplayGainMode;
  gapless: boolean;
  /** Active DSD → PCM FIR tap count (256 / 1024 / 2048). */
  dsd_taps: number;
  /** Native DSD via DoP opt-in (#495), default false. */
  dsd_dop: boolean;
  /** Open the output at each track's own rate (#600), default false. */
  match_source_rate: boolean;
  /** Park playback when the output device goes away (#617), default true. */
  pause_on_device_loss: boolean;
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

/**
 * Open the output at each track's own rate instead of taking whatever
 * the device offers (#600).
 *
 * Only while the output really owns its device: in shared mode the
 * system mixer is in the path whatever rate we open at. A device that
 * refuses the track's rate falls back to one it does offer and the
 * decoder resamples, as it always did.
 *
 * Off by default, and not because it is worse — reopening the device
 * costs an audible gap, so every rate change becomes a break in the
 * music. Takes effect on the next track.
 */
export function playerSetMatchSourceRate(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_match_source_rate", { enabled });
}

/**
 * Park playback instead of letting it follow the system onto another
 * device when the one it is playing on disconnects (#617).
 *
 * Applies to the next device loss — nothing is rebuilt now. A device
 * that flaps and comes back is still picked up where it was; only a
 * fallback onto a different endpoint parks the session.
 */
export function playerSetPauseOnDeviceLoss(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_pause_on_device_loss", { enabled });
}

export function playerSetCrossfade(seconds: number): Promise<void> {
  return invoke<void>("player_set_crossfade", { seconds });
}

export function playerSetReplayGain(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_replaygain", { enabled });
}

/**
 * Bound the backend enforces on the pre-amp and the fallback gain.
 * Mirrored here so the sliders can't ask for a value that would come
 * back clamped and out of sync with what the user sees.
 */
export const REPLAYGAIN_ADJUST_LIMIT_DB = 15;

export interface ReplayGainOptions {
  preampDb: number;
  fallbackDb: number;
  preventClipping: boolean;
}

export function playerSetReplayGainOptions(
  options: ReplayGainOptions,
): Promise<void> {
  return invoke<void>("player_set_replaygain_options", {
    preampDb: options.preampDb,
    fallbackDb: options.fallbackDb,
    preventClipping: options.preventClipping,
  });
}

/**
 * Which of the two gains a file can carry gets applied.
 *
 * - `track` levels every track against every other one.
 * - `album` applies one gain across a whole record, keeping the level
 *   relationships the mastering engineer put inside it.
 * - `auto` picks per track from how it is being played: album gain
 *   while a record plays through, track gain for the same song heard
 *   between two unrelated ones.
 */
export type ReplayGainMode = "track" | "album" | "auto";

export const REPLAYGAIN_MODES: readonly ReplayGainMode[] = [
  "auto",
  "track",
  "album",
];

export function playerSetReplayGainMode(
  mode: ReplayGainMode,
): Promise<void> {
  return invoke<void>("player_set_replaygain_mode", { mode });
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
 * Toggle native DSD output via DoP (DSD over PCM), #495. When on AND an
 * exclusive output can be opened — WASAPI Exclusive (Windows), a raw ALSA
 * `hw:` device (Linux), CoreAudio hog mode (macOS) — AND the DAC accepts
 * the DoP format, `.dsf` / `.dff` tracks are shipped as raw 1-bit DoP
 * frames the DAC decodes natively (bit-perfect) instead of being
 * converted to PCM. Any condition failing falls back silently to
 * DSD → PCM, so it's safe to leave on. Persisted in
 * `profile_setting['audio.dsd_dop']`.
 */
export function playerSetDsdDop(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_dsd_dop", { enabled });
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
  /** The device audio really comes out of, read from what the backend
   *  opened. A pinned device that has vanished is replaced by the
   *  default endpoint, so this can differ from `is_pinned` (#612). */
  is_active: boolean;
  /** The device the user picked, opened or not. Differs from
   *  `is_active` exactly during such a fallback. */
  is_pinned: boolean;
}

/**
 * Which question a capability answer came from (#593).
 *
 * Stated rather than implied, because what a driver *declares* and what
 * it *accepts in exclusive mode* are different questions — a shared-mode
 * path can advertise rates it reaches by resampling.
 */
export type CapabilitySource =
  | "wasapi-exclusive"
  | "alsa-hardware"
  | "coreaudio"
  | "unavailable";

export interface DeviceFormat {
  /** The backend's own spelling: `S24_3LE`, `F32`, … */
  label: string;
  /** Bits of real audio per sample — 24 for both 24-bit layouts. */
  bits: number;
  float: boolean;
  /**
   * The rates accepted **in this format**, ascending.
   *
   * Per format because the two are not independent: a DAC that takes
   * 32-bit to 96 kHz and 24-bit to 192 kHz accepts neither pair the two
   * maxima would suggest.
   */
  sample_rates: number[];
}

/** What one output device accepts (#593). */
export interface DeviceCapabilities {
  device_id: string | null;
  source: CapabilitySource;
  formats: DeviceFormat[];
  /** Every rate accepted in *some* format — the union, for the tiers. */
  sample_rates: number[];
  max_channels: number;
  /** The smallest period the device will take, in frames — one quantity
   *  asked the same way of every backend. */
  min_period_frames: number | null;
  /** Technical, for the tooltip — the UI says it in its own words. */
  unavailable_reason: string | null;
}

/**
 * Ask one device what it accepts (#593).
 *
 * On demand and one device at a time: the listing never does this, since
 * probing during enumeration is what the ALSA-hint shortcut exists to
 * avoid. The backend memoises the answer for the session.
 */
export function playerProbeOutputDevice(
  deviceId: string | null,
): Promise<DeviceCapabilities> {
  return invoke<DeviceCapabilities>("player_probe_output_device", { deviceId });
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
 * Re-open the stream on the device that is already selected. Picking
 * the same device is a no-op in the engine, which left no way back when
 * the audio had drifted onto another endpoint (#612). Changes no
 * preference, so nothing is persisted.
 */
export function playerReopenOutputDevice(): Promise<void> {
  return invoke<void>("player_reopen_output_device");
}

/**
 * Toggle exclusive output: own the device rather than share it with
 * the system mixer — WASAPI Exclusive on Windows, a raw ALSA `hw:`
 * device on Linux, CoreAudio hog mode on macOS. The backend persists
 * the value on every platform, including any without an exclusive
 * backend. Falls back to cpal shared if exclusive init fails (device
 * busy, no supported format).
 */
export function playerSetExclusiveOutput(enabled: boolean): Promise<void> {
  return invoke<void>("player_set_exclusive_output", { enabled });
}

/**
 * Read whether the output really owns its device right now. Useful for
 * the Settings card to show what's actually active: a failed exclusive
 * init silently falls back to shared, so the toggle can be on while
 * the mode is off.
 */
export function playerGetExclusiveOutput(): Promise<boolean> {
  return invoke<boolean>("player_get_exclusive_output");
}
