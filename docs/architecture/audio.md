# Audio architecture

3-thread lock-free pipeline. The contract is "the cpal callback never blocks, allocates or logs"; everything else flows from that.

```bash
┌─ Tauri commands (tokio)        ┌─ Decoder thread (std)               ┌─ cpal callback (real-time)
│  player_play, pause, seek      │  symphonia FormatReader +           │  pop f32 from SPSC ring
│  → crossbeam::Sender ─────────►│  Decoder + rubato Resampler         │  × volume × normalization
│                                │  push f32 → rtrb::Producer ────────►│  mono downmix (if enabled)
│                                │  emit position/state events         │  → device native format
└────────────────────────────────┴─────────────────────────────────────┴──────────────────────────
```

## Threads

| Thread                            | Owner                                                    | Responsibilities                                                                                                                                                                                                                                 |
| --------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Tokio runtime**                 | Tauri                                                    | Command dispatch. `player_*` commands send `AudioCmd` enum variants over a `crossbeam::Sender` to the decoder.                                                                                                                                   |
| **`waveflow-audio-decoder`**      | `audio::decoder::spawn_decoder_thread`                   | Owns the `rtrb::Producer<f32>` and the active `ActiveStream` (symphonia + rubato). Polls commands between packets so pause / stop / seek feel responsive.                                                                                        |
| **`waveflow-audio-output`**       | `audio::output::spawn_output_thread`                     | Owns the `cpal::Stream` (which is `!Send` on Windows because WASAPI / COM handles can't cross threads). Parks on a shutdown channel for the engine's lifetime.                                                                                   |
| **cpal callback**                 | cpal-managed (WASAPI / ALSA / CoreAudio worker)          | Pops samples from `rtrb::Consumer`, applies volume / normalization / mono downmix, writes to the device buffer.                                                                                                                                  |
| **`waveflow-wasapi-exclusive`** ¹ | `audio::wasapi_exclusive::spawn_exclusive_output_thread` | Windows-only alternate output backend (opt-in). Owns the WASAPI `IAudioClient` + event handle. Blocks on the OS event between buffer periods — zero CPU when idle. Drives the same `rtrb::Consumer<f32>` as the cpal thread does in shared mode. |

¹ Mutually exclusive with the cpal output thread — only one of the two is running at a time, picked by `output::spawn_output_with_mode` based on the persisted `audio.wasapi_exclusive` setting.

## Shared state

[`SharedPlayback`](../../src-tauri/crates/app/src/audio/state.rs) — an `Arc<...>` of atomics plus the rtrb consumer half. Read on the hot path; mutated by the decoder and the command layer. No locks anywhere in the pipeline.

| Atomic                                        | Owner writes                                 | Hot-path reads             |
| --------------------------------------------- | -------------------------------------------- | -------------------------- |
| `samples_played`                              | cpal callback                                | UI for position display    |
| `base_offset_ms`                              | decoder (on seek / new track / speed change) | UI                         |
| `volume`, `normalize_enabled`, `mono_enabled` | command layer                                | cpal callback              |
| `paused_output`, `drain_silent`               | command layer / decoder                      | cpal callback              |
| `crossfade_ms`, `replaygain_enabled`          | command layer                                | decoder                    |
| `playback_speed_bits`, `speed_dirty`          | command layer / decoder                      | decoder + UI position math |
| `current_track_id`, `seek_generation`         | decoder                                      | UI                         |

`playback_speed_bits` is read on every position computation (UI 4 Hz + analytics) — see [`current_position_ms`](../../src-tauri/crates/app/src/audio/state.rs) and [playback / Playback speed](../features/playback.md#playback-speed-05--2). `speed_dirty` is a one-shot flag the decoder consumes once per `'pkt` loop iteration to trigger a resampler rebuild.

## WASAPI Exclusive Mode (Windows opt-in)

[`audio/wasapi_exclusive.rs`](../../src-tauri/crates/app/src/audio/wasapi_exclusive.rs) is a parallel output backend to the cpal shared-mode default. Engaged via the `audio.wasapi_exclusive` profile setting (toggle in Settings → Audio). When on:

1. `output::spawn_output_with_mode` tries the exclusive backend first via the [`wasapi` crate](https://crates.io/crates/wasapi).
2. The backend opens the device in **event-driven exclusive mode**, negotiating over two axes (see below).
3. If every candidate fails (device busy with another exclusive app, no supported format, COM apartment conflict), the engine logs a warning and falls back transparently to the cpal shared backend so the user keeps hearing audio.

### Format negotiation (two axes)

`open_exclusive_session` walks **layout × bit depth** and takes the first pair the driver accepts.

**Layout** — `(sample rate, channel count)`, from `build_layout_candidates`, in order:

| # | Origin     | Source                                                                                           |
|---|------------|--------------------------------------------------------------------------------------------------|
| 1 | `endpoint` | `PKEY_AudioEngine_DeviceFormat` — the "Default Format" picked in the Windows Sound control panel |
| 2 | `mix`      | `IAudioClient::GetMixFormat` — the shared-mode mix format                                        |
| 3 | `stereo`   | 2 channels at each of the rates above                                                            |

The endpoint format leads on purpose (#409). The mix format describes the pipe into the Windows audio engine **after** its own processing, so with Audio Enhancements, Spatial Sound or a virtual-surround driver active it reports e.g. 8 channels for a plain stereo jack. Negotiating exclusive mode against that asks the hardware for a layout it has never supported: every format is rejected with `AUDCLNT_E_UNSUPPORTED_FORMAT` (`0x88890008`) and the user silently lands back in shared mode. `PKEY_AudioEngine_DeviceFormat` describes the endpoint itself, which is what exclusive mode wants. The two agree on most machines, so candidate 2 is deduped away.

**Bit depth** — `FORMAT_FALLBACK_CHAIN`, high to low: `F32` → `S24_3LE` → `S24_4LE` → `S16_LE`. Most audiophile DACs take Float32 natively; Realtek ALC and many integrated codecs reject it outright but accept PCM.

Each rejection logs the full `(rate, channels, format, layout-origin)` at `debug`, and the success logs the same at `info` — a rejection blamed on the bit depth is very often the channel count instead.

Trade-offs:

- **Bit-perfect to the DAC at the chosen rate.** No Windows mixer between us and the hardware — no automatic resampling, no system-sound mixing, no per-app volume DSP.
- **One app at a time.** While exclusive is engaged, system sounds (notifications, Discord, browser audio) are silenced. By design.
- **Mode survives device hot-swaps.** `engine::set_output_device` reuses the same `spawn_output_with_mode` dispatch so picking a new output keeps the chosen mode.
- **No per-track rate switching yet.** The decoder's rubato resampler still converts every source to the negotiated rate. True bit-perfect at the source rate is a future phase (would require reinitialising the WASAPI client on every rate change).
- **The negotiated layout is authoritative downstream.** `open_exclusive_session` stores it into `SharedPlayback.{sample_rate,channels}` before init is reported to the caller, so the decoder thread — spawned only after that — resamples and downmixes to what the device actually accepted, not to what the mix format claimed.

Dependency footprint: the `wasapi` crate + a slim slice of `windows-rs` features (`Win32_Foundation`, `Win32_System_Com`, `Win32_System_Threading`) target-gated to `cfg(target_os = "windows")`. Adds ~5-10 MB to the NSIS / MSI Windows binary; the Linux + macOS bundles are untouched.

## Ring buffer sizing

`RING_CAPACITY = 96_000` `f32` samples. At 48 kHz stereo this is ~1 s of audio — plenty of headroom for the decoder while keeping latency low. With more channels the headroom shrinks proportionally (8-channel surround → ~272 ms), which is mostly relevant for the seek drain time (see [playback](../features/playback.md#seek)).

## Drain modes

Two reasons to suppress audio output without tearing the stream down:

| Flag            | Behaviour                                                 | Use                                                                                             |
| --------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `paused_output` | callback writes silence, **doesn't pop** the ring         | Pause — resume picks back up exactly where we stopped.                                          |
| `drain_silent`  | callback **bulk-pops** the entire ring AND writes silence | Track switch / seek — flushes the tail of the previous position so it never reaches the device. |

The bulk-pop in `drain_silent` (vs the previous one-pop-per-output-slot) is what makes seeks feel instant on multi-channel output devices.

## Crossfade dual-decoder

When the user enables crossfade, the decoder maintains a `pending_next: Option<ActiveStream>` set by an `AudioCmd::SetNextTrack` from the command layer. On each iteration it tops up persistent `primary_resampled` and `secondary_resampled` buffers (one packet each), then mixes the minimum of both with `equal_power_gains(t)`. The window is clamped to `min(user_ms, primary.duration / 2)` so 30 s clips don't start mixing at 18 s.

Per-stream ReplayGain is applied **before** the mix so the loudness of the two tracks doesn't drift mid-fade.

## Why not async for the decoder?

The decoder is a tight CPU + I/O loop with no benefit from `Future` polling. Spawning it as a `std::thread` keeps it off the tokio runtime (so a stuck packet read can't starve other tasks) and lets it own its `Producer<f32>` and `ActiveStream` without `Send + Sync` gymnastics. The interface to the rest of the app is a single `crossbeam::channel`.
