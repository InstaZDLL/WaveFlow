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
| **`waveflow-{wasapi,alsa,coreaudio}-exclusive`** ¹ | `audio::{wasapi,alsa,coreaudio}_exclusive::spawn_*_output_thread` | The per-OS alternate output backend (opt-in). Owns the device outright — the WASAPI `IAudioClient` + event handle, the raw ALSA `hw:` PCM, or the hogged CoreAudio `AudioUnit`. Drives the same `rtrb::Consumer<f32>` as the cpal thread does in shared mode. |

¹ Mutually exclusive with the cpal output thread **and with each other** — exactly one output thread runs at a time, picked by `output::spawn_output_with_mode` from the persisted `audio.exclusive_output` setting (plus whether the track needs DoP).

## Shared state

[`SharedPlayback`](../../src-tauri/crates/app/src/audio/state.rs) — an `Arc<...>` of atomics plus the rtrb consumer half. Read on the hot path; mutated by the decoder and the command layer. No locks anywhere in the pipeline.

| Atomic                                        | Owner writes                                 | Hot-path reads             |
| --------------------------------------------- | -------------------------------------------- | -------------------------- |
| `samples_played`                              | cpal callback                                | UI for position display    |
| `base_offset_ms`                              | decoder (on seek / new track / speed change) | UI                         |
| `volume`, `normalize_enabled`, `mono_enabled` | command layer                                | cpal callback              |
| `paused_output`, `drain_silent`               | command layer / decoder                      | cpal callback              |
| `crossfade_ms`, `replaygain_enabled`          | command layer                                | decoder                    |
| ReplayGain pre-amp / fallback / clipping      | command layer                                | decoder (re-read per buffer) |
| `playback_speed_bits`, `speed_dirty`          | command layer / decoder                      | decoder + UI position math |
| `current_track_id`, `seek_generation`         | decoder                                      | UI                         |

`playback_speed_bits` is read on every position computation (UI 4 Hz + analytics) — see [`current_position_ms`](../../src-tauri/crates/app/src/audio/state.rs) and [playback / Playback speed](../features/playback.md#playback-speed-05--2). `speed_dirty` is a one-shot flag the decoder consumes once per `'pkt` loop iteration to trigger a resampler rebuild.

## Exclusive output (opt-in)

Each OS has a parallel output backend to the cpal shared-mode default, engaged by the **one** `audio.exclusive_output` profile setting (toggle in Settings → Audio):

| OS      | Backend                                                                                              | How the device is taken                                                     |
| ------- | ---------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| Windows | [`wasapi_exclusive.rs`](../../src-tauri/crates/app/src/audio/wasapi_exclusive.rs)                     | WASAPI event-driven exclusive mode via the [`wasapi` crate](https://crates.io/crates/wasapi) |
| Linux   | [`alsa_exclusive.rs`](../../src-tauri/crates/app/src/audio/alsa_exclusive.rs)                         | a raw `hw:` PCM, after asking the sound server for the card                  |
| macOS   | [`coreaudio_exclusive.rs`](../../src-tauri/crates/app/src/audio/coreaudio_exclusive.rs)               | CoreAudio **hog mode**                                                      |

The shape is the same everywhere: `output::spawn_output_with_mode` tries the exclusive backend first, and **every** failure (device busy, no format we can write, no driver support, COM apartment conflict) logs a warning and falls back transparently to the cpal shared backend, so the user keeps hearing audio. The one exception is DoP, which is a demand rather than a preference — see [playback / Decoding & output](../features/playback.md#decoding--output).

The setting is stored under `audio.exclusive_output`; the boot read in [`lib.rs`](../../src-tauri/crates/app/src/lib.rs) also accepts the legacy `audio.wasapi_exclusive` row (the name from when only Windows had a backend), with the current key winning when both exist.

**What it is and isn't.** Exclusive output means the system mixer is out of the path — nothing else is mixed in, resampled or DSP'd on top of us. It does **not** mean the source rate is honoured end to end: except on the DoP path, all three backends open at a rate the *device* offers and the decoder's rubato resampler meets it. Making the rate follow the source needs the device re-opened per track, which is a separate phase — which is why the UI copy no longer says "bit-perfect" here (the audio-pipeline pill still can, but only when it has also checked that source rate == output rate; see [ui / Bit-perfect conditions](../features/ui.md#bit-perfect-conditions)).

### Windows: format negotiation (two axes)

`open_exclusive_session` walks **layout × bit depth** and takes the first pair the driver accepts.

**Layout** — `(sample rate, channel count)`, from `build_layout_candidates`, in order:

| #   | Origin     | Source                                                                                           |
| --- | ---------- | ------------------------------------------------------------------------------------------------ |
| 1   | `endpoint` | `PKEY_AudioEngine_DeviceFormat` — the "Default Format" picked in the Windows Sound control panel |
| 2   | `mix`      | `IAudioClient::GetMixFormat` — the shared-mode mix format                                        |
| 3   | `stereo`   | 2 channels at each of the rates above                                                            |

The endpoint format leads on purpose (#409). The mix format describes the pipe into the Windows audio engine **after** its own processing, so with Audio Enhancements, Spatial Sound or a virtual-surround driver active it reports e.g. 8 channels for a plain stereo jack. Negotiating exclusive mode against that asks the hardware for a layout it has never supported: every format is rejected with `AUDCLNT_E_UNSUPPORTED_FORMAT` (`0x88890008`) and the user silently lands back in shared mode. `PKEY_AudioEngine_DeviceFormat` describes the endpoint itself, which is what exclusive mode wants. The two agree on most machines, so candidate 2 is deduped away.

**Sample format** — `FORMAT_FALLBACK_CHAIN`, tried in order: `F32` → `S24_3LE` → `S24_4LE` → `S16_LE`. Float32 leads because it costs no conversion at the boundary and most audiophile DACs take it natively; Realtek ALC and many integrated codecs reject it outright but accept PCM, so the two 24-bit PCM representations follow (packed 3-byte, then 24 valid bits in a 4-byte container — the same precision, different wire layout), with `S16_LE` as the universal last resort. Note this is a format sequence, not a monotonic walk down bit depth: `S24_4LE` uses a 32-bit container.

**Representation** — each `(layout, format)` pair is probed through `AudioClient::is_supported_exclusive_with_quirks`, not a bare `is_supported`. Two driver quirks hide behind an `AUDCLNT_E_UNSUPPORTED_FORMAT` that looks like a rate/depth problem but isn't:

- **Channel mask.** `WaveFormat::new(.., None)` guesses one, and a multi-channel endpoint typically accepts exactly one specific mask. The helper re-probes against each recommended `ksmedia.h` mask. _This is the one that bit in practice_ (#405): a 7.1-configured endpoint rejected every rate × depth combination purely on the mask.
- **Structure shape.** `WaveFormat::new` always builds a `WAVEFORMATEXTENSIBLE`; some drivers refuse it while accepting the same PCM layout as a plain `WAVEFORMATEX`. The helper retries that form for mono/stereo only.

Init then uses the shape the probe accepted, falling back to the requested shape when the two differ (wasapi's docs note some drivers validate the simplified form but want the original at init time).

Each rejection logs the full `(rate, channels, format, layout-origin)` at `debug`, the success logs the same at `info`, and a total failure logs **every** attempt in one `warn` line — release builds log at `info`, so without that summary a user report only ever surfaced the last attempt of eight.

Dependency footprint: the `wasapi` crate + a slim slice of `windows-rs` features (`Win32_Foundation`, `Win32_System_Com`, `Win32_System_Threading`) target-gated to `cfg(target_os = "windows")`. Adds ~5-10 MB to the NSIS / MSI Windows binary; the Linux + macOS bundles are untouched.

### Linux: a raw `hw:` device, asked for rather than grabbed

There is no plug layer under a `hw:` device — that is the whole point — so **every** conversion from the ring's `f32` is ours. `FORMAT_FALLBACK_CHAIN` walks the wire formats the hardware itself has to accept, best first:

`FLOAT_LE` → `S32_LE` → `S24_3LE` → `S24_LE` → `S16_LE`

with the channel count tried at the engine's current value first and stereo as the fallback (a zero count — nothing has opened an output yet — reads as stereo, or the first launch with exclusive already on would ask a card for mono and get it).

**`S24_LE` is placed late on purpose.** ALSA puts the 24 valid bits in the **low** three bytes of the 32-bit word; WASAPI's `Pcm24Padded` puts them in the **high** three. Copying the other backend's `<< 8` multiplies every sample by 256 into permanent clipping, and the mirror mistake costs 48 dB. A unit test pins the layout, and the enum variant's doc comment says why it exists.

Two things the DoP path had worked out first are now shared rather than duplicated:

- **The reservation protocol** ([`device_reservation.rs`](../../src-tauri/crates/app/src/audio/device_reservation.rs)) — PipeWire and PulseAudio hold every card from login, so a bare `hw:` open returns `EBUSY` on any desktop. Taking the `org.freedesktop.ReserveDevice1.Audio<N>` bus name is the protocol both servers watch in order to release a device; the open is then retried for up to a second while the server finishes letting go. The reservation is bound to the stream and released with it.
- **Partial writes** — re-offering only the frames the device declined.

**Period and buffer must both be asked for.** `HwParams::any` leaves them at whatever the driver offers, and `snd_pcm_hw_params` then takes its **maximum** for both. Measured on a `snd-dummy` card: a 16 384-frame period — ~370 ms at 44.1 kHz — and a buffer deep enough that starting a track, seeking and changing track each took about ten seconds. That wait was **the buffer draining**, not the period.

The period has its own, quieter effect: one period is drained from the ring in a single pass, and whatever the ring can't supply is written as silence. A period that size was two thirds of [`RING_CAPACITY`](#ring-buffer-sizing) on the card measured, which makes an underrun the normal case rather than the exception. `set_period_and_buffer` asks for 1024 frames (~23 ms at 44.1 kHz) and a buffer of 4 periods.

### macOS: hog mode, and nothing else

`open_and_run_pcm` takes hog mode and leaves the device's **physical format exactly as it found it**, reading the rate and channel count the device already runs at and publishing them for the decoder to meet. The DoP path does pin the format — a marker cadence that gets resampled is noise — but re-clocking a device the whole machine shares is a price only that cadence justifies.

**Hog mode is registered against a PID**, so it does not evict a stream from our own process. The engine's spawn-before-release order — which works on Windows (a shared client doesn't block the exclusive open at all) and on Linux (the reservation makes the server hand the card back) — produced an `AudioUnit` here that rendered nothing at all: no sound, position counter frozen. The release-first rule in [playback / Output-stream lifecycle](../features/playback.md#output-stream-lifecycle--recovery) now covers this case too.

### Shared by all three

- **One app at a time.** While exclusive is engaged, system sounds (notifications, Discord, browser audio) are silenced. By design.
- **The mode survives device hot-swaps.** `engine::set_output_device` reuses the same `spawn_output_with_mode` dispatch, so picking a new output keeps the chosen mode.
- **The negotiated layout is authoritative downstream.** Each backend stores what the device actually accepted into `SharedPlayback.{sample_rate,channels}` before init is reported to the caller, so the decoder thread — spawned only after that — resamples and downmixes to that, not to what any mix format claimed.
- **The period fill is one function.** [`output::fill_pcm_period`](../../src-tauri/crates/app/src/audio/output.rs) pops the ring and applies volume / normalization / mono downmix, and it lives in `output.rs` rather than in each backend — the three have no business disagreeing about whether an underrun counts toward the play clock. Its tests run on every platform instead of only where one backend compiles.
- **Failure is fail-soft but not silent.** A backend that can't open logs a `warn`; the engine reports what actually engaged through `PlayerStateSnapshot.exclusive_active`, which is what the Settings card and the pipeline pill read (the toggle can be on while the mode is off).

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

Per-stream ReplayGain is applied **before** the mix so the loudness of the two tracks doesn't drift mid-fade. Each `ActiveStream` carries the track's gain and peak as metadata rather than a baked-in scalar, and [`replay_gain::effective_linear`](../../src-tauri/crates/app/src/audio/replay_gain.rs) turns them into a multiplier once per decoded buffer — that is what lets the pre-amp and clipping settings take effect mid-track. Full behaviour in [playback](../features/playback.md#replaygain).

## Why not async for the decoder?

The decoder is a tight CPU + I/O loop with no benefit from `Future` polling. Spawning it as a `std::thread` keeps it off the tokio runtime (so a stuck packet read can't starve other tasks) and lets it own its `Producer<f32>` and `ActiveStream` without `Send + Sync` gymnastics. The interface to the rest of the app is a single `crossbeam::channel`.
