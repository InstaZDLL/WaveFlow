# Playback engine

The audio path lives in [`src-tauri/crates/app/src/audio/`](../../src-tauri/crates/app/src/audio). It is a 3-thread lock-free pipeline; see [audio architecture](../architecture/audio.md) for the wider topology and invariants.

## Decoding & output

- **Decoder** — [`symphonia 0.6`](https://crates.io/crates/symphonia) over MP3, FLAC, WAV, OGG Vorbis, AAC, ALAC (M4A). Source samples are converted to interleaved `f32`, channel-mapped (mono ↔ stereo, and any multichannel source — 3.0 / quad / 5.0 / 5.1 / 6.1 / 7.1 — folded to stereo Lo/Ro per ITU-R BS.775, centre + surrounds at −3 dB, LFE dropped), then resampled to the device rate by [`rubato 2.0`](https://crates.io/crates/rubato) (`Fft<f32>` + `FixedSync::Input`, with a fast `Passthrough` variant when source rate already matches the device). **Network pre-load**: when the source lives on a network share (Windows UNC / mapped `DRIVE_REMOTE` drive, or a Linux gvfs / SMB mount), [`ActiveStream::open`](../../src-tauri/crates/app/src/audio/crossfade.rs) reads the whole file into RAM (under a 512 MiB cap) and decodes from an in-memory `Cursor` instead of streaming — high-latency per-packet reads over the link would otherwise stutter mid-playback. Best-effort: oversize / unreadable files fall back to ordinary streaming. DSD keeps streaming (multi-GB files would blow the cap).
- **DSD pipeline** — symphonia doesn't decode 1-bit DSD, so DSF (Sony) and DFF (Philips) containers route through [`audio/dsd/`](../../src-tauri/crates/core/src/audio_format/dsd/): a custom container parser reads the layout (DSD64 → DSD1024, mono / stereo / multichannel), and a windowed-sinc FIR with a Blackman-Harris envelope (256 taps by default, user-selectable up to 1024 / 2048 via Settings → Playback — persisted in `profile_setting['audio.dsd_precision']`, mirrored into the `SharedPlayback.dsd_taps` atomic and read at stream-open by [`DsdToPcm::new_with_taps`](../../src-tauri/crates/core/src/audio_format/dsd/pcm.rs); DSD-only, symphonia formats ignore it, and more taps buy a sharper transition band at linear CPU cost) decimates the bitstream by 64 to land DSD64 at 44.1 kHz, DSD128 at 88.2 kHz, etc. The resulting PCM joins the same channel-convert + resample + ring-buffer pipeline as symphonia output. `ActiveStream` carries a `StreamBackend` enum (Symphonia / Dsd / Dop) so seeking and decoder reset stay uniform from the engine's perspective. **Limitation**: real audiophile players use multi-stage halfband cascades for lower CPU at the same SNR; ours prioritises code clarity.
- **Native DSD via DoP** (DSD over PCM, #495, opt-in `profile_setting['audio.dsd_dop']` default OFF) — when the toggle is on AND the active output has exclusive device access AND the DAC accepts the format, a DSD track skips the FIR entirely: [`DsdToDop`](../../src-tauri/crates/core/src/audio_format/dsd/dop.rs) repackages the raw 1-bit stream into 24-bit DoP frames (marker `0x05`/`0xFA` alternating per frame, payload MSB-first — DFF verbatim, DSF bit-reversed) at `dsd_rate / 16` (DSD64 → 176.4 kHz, DSD128 → 352.8, DSD256 → 705.6), and the DAC reconstructs the 1-bit stream in hardware (truly bit-perfect, nothing on our side filters / resamples / gains it). **Per-platform exclusive backend** — DoP needs a mixer-free path to the DAC, so each OS has its own: **Windows** = WASAPI Exclusive ([`run_dop_event_loop`](../../src-tauri/crates/app/src/audio/wasapi_exclusive.rs)); **Linux** = a raw `hw:` ALSA device at `S32_LE`, marker MSB-justified ([`alsa_exclusive`](../../src-tauri/crates/app/src/audio/alsa_exclusive.rs)); **macOS** = CoreAudio **hog mode** + a forced physical stream format at the DoP rate in 32-bit int, fed through an `AudioUnit` render callback ([`coreaudio_exclusive`](../../src-tauri/crates/app/src/audio/coreaudio_exclusive.rs)); that format is a property of the **device**, so the previous one is saved and restored on teardown — otherwise the rest of the machine keeps talking to a DAC clocked for DoP — and device loss normally arrives through an `IsAlive` property listener, with a periodic `get_hogging_pid` query as the fallback when the listener can't be registered. All three ship the DoP words MSB-justified (marker in the top byte) and share the byte/idle packing in [`audio/dop_pack`](../../src-tauri/crates/app/src/audio/dop_pack.rs), which is also where the `0x05`/`0xFA` marker is **re-stamped** onto every outgoing frame from a single running counter: the encoder's own phase restarts on each seek and never sees the idle frames generated on pause / underrun, so letting both sides number frames independently would repeat or skip a marker exactly at those seams and drop the DAC out of DSD lock. On a cold `LoadAndPlay`, [`maybe_switch_dop_output`](../../src-tauri/crates/app/src/audio/decoder.rs) parses the DSD header for the DoP rate and asks the engine to re-open the exclusive output at that exact format ([`AudioEngine::switch_output_for_track`](../../src-tauri/crates/app/src/audio/engine.rs), which hands the fresh ring producer straight back rather than via the `SwapProducer` channel); the backend ships the words bit-exact and emits marker-carrying DoP idle frames (`0x69` payload) on pause/underrun so the DAC keeps DoP lock. **Linux: the card is asked for, not given up on.** PipeWire / PulseAudio hold every card from login, so the raw `hw:` open returned `EBUSY` on any desktop and DoP fell back silently — the toggle did nothing and said nothing. [`device_reservation`](../../src-tauri/crates/app/src/audio/device_reservation.rs) now takes the `org.freedesktop.ReserveDevice1.Audio<N>` name on the session bus, which is the protocol both servers watch in order to release a device, then the open is retried for up to a second while the server finishes letting go (releasing is asynchronous on its side). The reservation is bound to the stream and released with it. No session bus, or an owner that refuses to be replaced, lands exactly where the code was before. **Fully fail-soft**: a DAC that refuses the DoP rate, a non-exclusive output, or a platform without a DoP backend all fall back transparently to the DSD → PCM path above. The two load paths that never negotiate a format — remote files and HTTP streams — force the output back to PCM before opening, since a leftover DoP output would read their PCM samples as 24-bit words and ship them to the DAC as noise. On Windows DoP rides the separate WASAPI Exclusive opt-in; on Linux and macOS the DoP toggle itself engages the exclusive path (raw `hw:` / hog mode). DoP tracks never crossfade / gaplessly prefetch (the words can't be mixed), so they always transition through a cold load + output re-open; EQ / ReplayGain / normalize / mono / speed are bypassed (bit-perfect, volume is the DAC's job — and playback speed is pinned to 1× for the track, since the resampler that implements it isn't in the chain and only the reported position would move). Seeking is the one transport action that survives untouched, so **A-B repeat works on DoP tracks** just like on PCM ones. The pipeline popover shows a "Native DSD" pill sourced from `player_get_state.dop_active` — what actually engaged, not just the opt-in. **Playing DoP to a non-DoP DAC produces white noise**, so the toggle is opt-in and default OFF for users who know their DAC supports it.
- **Output** — [`cpal 0.17`](https://crates.io/crates/cpal) on a dedicated thread because `cpal::Stream` is `!Send` on Windows. Samples cross the thread via an [`rtrb 0.3`](https://crates.io/crates/rtrb) SPSC ring (`RING_CAPACITY = 96 000` `f32`s ≈ 1 s @ 48 kHz stereo).
- **Hot-path rules** — the cpal callback never allocates, locks or logs. It only reads the `rtrb::Consumer` and `Atomic*` fields in `SharedPlayback`.

## Spectrum visualizer

Real-time FFT bars surfaced in the immersive Now Playing overlay. Implementation:

- Backend: [`audio/spectrum.rs`](../../src-tauri/crates/app/src/audio/spectrum.rs) runs on the decoder thread (NOT in the cpal callback — too constrained). Post-EQ samples go through `SpectrumAnalyzer::feed`, which mono-mixes, applies a Hann window, runs a 2048-pt real FFT via `realfft`, then buckets the magnitudes into 48 log-spaced bands (30 Hz → 16 kHz). 50% overlap between successive frames so the visual feels continuous. Throttled to ~30 Hz via a manual `Instant` clock.
- Output is a `player:spectrum` Tauri event carrying a `Vec<f32>` of normalised band magnitudes (0..1, peaks may briefly overshoot).
- A `SharedPlayback::visualizer_enabled` atomic gates the entire path: when off, `feed` returns at the first atomic load — zero allocations, zero FFT cost. Persisted in `profile_setting['ui.visualizer']`, default OFF.
- Frontend: [`SpectrumVisualizer`](../../src/components/player/SpectrumVisualizer.tsx) subscribes to the event and drives a `<canvas>` with `requestAnimationFrame`. Asymmetric decay (jump up fast, fall slow) so transients pop without making the bars look glitchy. Auto-fades to zero on pause so the bars don't freeze mid-pose.
- Bar colour (issue #468): user-selectable per profile via [`useVisualizerColor`](../../src/hooks/useVisualizerColor.ts) — `White` (default, the historical `rgba(255,255,255,0.85)` so existing installs are unchanged) → `Emerald` → `Orange` → `Aqua` → `Magenta` → `Rainbow` (per-bar 0–300° hue sweep), stored in `profile_setting['ui.visualizer_color']`. A [`VisualizerColorButton`](../../src/components/player/VisualizerColorButton.tsx) next to the like/★ in [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) cycles through them (loops back to `White`); it only appears when the visualizer toggle is on. Rationale: the immersive backdrop is derived from album art, so no single fixed colour reads well over every cover — the user picks one that contrasts.

## Crossfade

Real dual-decoder mix in [`crossfade.rs`](../../src-tauri/crates/app/src/audio/crossfade.rs). When the user enables crossfade, the decoder maintains two `ActiveStream`s during the fade window and feeds an equal-power gain pair (`cos(t·π/2)` / `sin(t·π/2)`) into each so the summed RMS stays flat — no mid-fade dip. The window is clamped to `min(user_ms, duration / 2)` so 30 s clips with a 12 s setting don't start mixing at the 18 s mark.

### Smart crossfade (album-aware skip)

A separate `SharedPlayback::smart_crossfade_enabled` toggle (default OFF — opt-in because it's an opinionated behaviour change, persisted in `profile_setting['audio.smart_crossfade']`) suppresses the fade for two consecutive tracks belonging to the same album — concept records / live sets hand off naturally instead of getting smeared. Mechanism:

- The analytics worker's `PrefetchNext` handler looks up the current track's `album_id` and the upcoming track's `album_id` in a single SQLite round trip and writes the boolean result to `SharedPlayback::pending_next_same_album` right before sending `SetNextTrack`.
- The decoder, at mix-decision time, checks both atomics: if smart crossfade is on AND the prefetched track shares an album, it skips the mix branch and falls through to the existing gapless EOF swap (which already handles a sample-accurate hand-off when `pending_next.is_some()`).
- The hint is naturally one-shot: each new prefetch overwrites it, and `LoadAndPlay` paths (manual user clicks) don't go through the mix decision at all, so a stale value can't bleed into an unrelated transition.

### Dynamic crossfade (tempo-aware)

A separate `SharedPlayback::dynamic_crossfade_enabled` toggle (default OFF, persisted in `profile_setting['audio.dynamic_crossfade']`) scales each upcoming fade by the BPM gap between the current and next tracks. Same one-shot hint pattern as smart crossfade:

- The analytics `PrefetchNext` handler reads `track_analysis.bpm` for both tracks. If either is missing or zero, no override is written and the decoder falls back to the user's static `crossfade_ms`.
- When both BPMs are known, the worker scales `crossfade_ms` by a tier factor (≤8 BPM gap → 100%, ≤20 → 75%, ≤40 → 50%, otherwise 30%) with a 1500 ms floor (clamped against the base when the user picked a shorter window). The result lands in `SharedPlayback::pending_next_crossfade_ms` right before `SetNextTrack`.
- The decoder reads the override as the effective `cf_ms` when non-zero and clears it the instant the mix actually starts so the next prefetch starts from a clean slate. Toggling dynamic OFF also clears any in-flight override so the next transition snaps back to the static window immediately.

Smart and dynamic crossfade compose: the album skip wins (it's a hard "no fade" decision); when the album differs, the dynamic scaling applies.

ReplayGain is applied **per-stream before the mix** so the two tracks can have very different gains without the louder one swamping the fade.

## Seek

`format.seek()` + `decoder.reset()` + `resampler.flush()`. The cpal callback enters `drain_silent` mode, which (since 70c1968) drains the ring in **one bulk `while consumer.pop()` pass** instead of one sample per output slot — total perceived gap on seek dropped from ~270 ms (one full ring at 44.1 kHz × 8 ch) to ~10-15 ms (one cpal callback period).

After the drain, MP3 sources will emit a few `invalid main_data_begin, underflow` warnings from symphonia: the bit reservoir is invalidated by the seek and the codec recovers within 3-4 frames. Inherent to the format; not a bug.

## Output device picker

[`commands/player.rs::list_output_devices`](../../src-tauri/crates/app/src/commands/player.rs) → cpal device enumeration. The display name uses `description().extended()[0]` (Windows `DEVPKEY_Device_FriendlyName` — `Speakers (Logitech PRO X Wireless Gaming Headset)`) instead of `description().name()` (`DEVPKEY_Device_DeviceDesc` — just `Speakers`) so multiple endpoints in the same device class stay distinguishable.

The chosen device's name is persisted in `profile_setting['audio.output_device']`. `lib.rs::setup` reads it during boot and forwards it to the audio engine, so playback resumes on the user's preferred sink without waiting for the frontend to settle.

On Linux, enumeration uses ALSA's hint database (`snd_device_name_hint("pcm")`) instead of cpal's `output_devices()` to avoid a 1-2 s freeze + `pcm_dmix` / `pcm_route` stderr spam from probing every PCM card.

## Output-stream lifecycle & recovery

Three paths replace the output stream, and they must all end in the same place: `wasapi_exclusive_active` updated and a `player:audio-mode-changed` event emitted, because that event is the only thing that keeps Settings' Exclusive-mode toggle honest ([`ExclusiveModeCard`](../../src/components/views/settings/ExclusiveModeCard.tsx) re-reads on it).

| Path                   | Trigger                                 | Order                                                                                                                    |
| ---------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `set_output_device`    | user picks another endpoint             | spawn first, then release — the two streams target different devices, so a failed spawn can roll back to the working one |
| `set_wasapi_exclusive` | user toggles the mode                   | **release first when the old stream is exclusive**, then spawn                                                           |
| `force_rebuild_output` | automatic recovery after a device error | **release first when the old stream is exclusive**, then spawn                                                           |

The release-first rule is the #322 / #405 lesson: a WASAPI exclusive client owns its endpoint outright, so no other client — shared _or_ exclusive — can open it until that client is released. Re-opening the **same** endpoint while an exclusive stream still holds it always fails, and when it failed inside `set_wasapi_exclusive` the command returned `Err` before persisting anything, leaving the toggle latched on the mode the user was trying to leave (#405).

Device loss reaches the recovery path from two independent places, since the two backends have separate failure surfaces:

- **cpal shared** — the stream's `err_fn` callback fires on an arbitrary thread.
- **WASAPI exclusive** — [`wasapi_exclusive::run_event_loop`](../../src-tauri/crates/app/src/audio/wasapi_exclusive.rs) returns an `ExitReason`; `DeviceLost` covers a failed `wait_for_event` / `write_to_device`. Each is re-checked against the shutdown channel first so a deliberate teardown isn't mistaken for a failure.

Both then call the shared [`output::notify_device_lost`](../../src-tauri/crates/app/src/audio/output.rs) (park the player, emit `player:state` + `player:error`, sync the OS media controls) and [`output::schedule_device_rebuild`](../../src-tauri/crates/app/src/audio/output.rs) (300 ms backoff, then a same-device rebuild).

Two gates keep the recovery from thrashing:

- **`RebuildGate`** (`REBUILD_SETTLE_WINDOW`, 2 s) — one rebuild per burst of device errors. `begin_deliberate_output_change()` opens the same window around a mode toggle, because seizing the endpoint exclusively kicks the outgoing shared client off it and that self-inflicted `DeviceNotAvailable` would otherwise schedule a rebuild that undoes the switch.
- **`FlapWindow`** (`EXCLUSIVE_FLAP_THRESHOLD` / `EXCLUSIVE_FLAP_WINDOW`) — a device that resets on every exclusive grab gives up on exclusive for the rest of the session. Cleared by an explicit toggle or device switch.

Every failure path that ends with no output thread at all publishes `wasapi_exclusive_active = false` + the event before returning the error — a toggle describing a stream that no longer exists is the exact shape of #405.

## OS media controls

[`media_controls.rs`](../../src-tauri/crates/app/src/media_controls.rs) bridges the engine to [`souvlaki 0.8`](https://crates.io/crates/souvlaki):

- **Windows** — SMTC. Now-Playing artwork is served to SMTC over a tiny localhost HTTP shim because Windows expects a URL, not a file path.
- **Linux** — MPRIS via D-Bus.
- **macOS** — MediaRemote (NowPlayingInfoCenter).

Initialised after the main window exists (needs an HWND on Windows). State transitions are driven through `transition_state()` so the OS overlay flips at the same instant as the in-app controls; the brief `Loading` state is skipped to avoid a 50 ms "controls flash off" between tracks.

The same `transition_state()` hook also feeds [`discord_presence.rs`](../../src-tauri/crates/app/src/discord_presence.rs) so the user's Discord profile mirrors the playing/paused state. Documented separately under [Integrations → Discord Rich Presence](integrations.md#discord-rich-presence).

## Playback speed (0.5× – 2×)

Resampler-shift approach — same trick VLC uses for its default playback rate, costs ~zero CPU and works uniformly across every codec (symphonia + DSD). **Pitch is NOT preserved**: 1.5× speed lifts the pitch by ~7 semitones. Proper pitch-locked time-stretching needs a phase vocoder; this is out of scope for the MVP.

### Mechanism

The decoder feeds [`rubato`](https://crates.io/crates/rubato) a fake source rate of `actual_rate × speed`. Each cpal output sample then represents `speed` source samples of audio, so the device clock plays the track faster (speed > 1) or slower (speed < 1) without changing the device's real sample rate. Concretely:

- `SharedPlayback::playback_speed_bits` (`AtomicU32` holding `f32::to_bits`, clamped to `[0.5, 2.0]`).
- `SharedPlayback::speed_dirty` — flipped by `set_playback_speed`; the decoder polls it once per `'pkt` loop iteration and rebuilds every active stream's resampler (primary + crossfade prefetched secondary). Rebuild cost is a single `Resampler::new` call; rubato's `Fft<f32>` is fixed-rate and can't be reconfigured in place.
- Local already-resampled buffers (`primary_resampled`, `secondary_resampled`) are cleared on rebuild so old-speed samples don't get pushed alongside new-speed ones, and `drain_silent` flushes the rtrb ring so the audible transition is < 20 ms.
- `ActiveStream` caches its true `src_sample_rate` the first time `decode_next` builds a resampler so subsequent rebuilds (mid-track speed change) know what to multiply by. New tracks (`LoadAndPlay`, `SetNextTrack`) inherit the active speed before their first decode, so the lazy resampler init picks the right effective rate from packet #1.

### Position continuity

`set_playback_speed` snapshots the current position **at the old speed**, rebases `samples_played` to 0 and stores the snapshot in `base_offset_ms` before flipping the speed atomic. Without this, the next call to `current_position_ms()` would re-scale the existing samples_played counter by the new factor — the progress bar would jump backwards (slowing down) or forwards (speeding up) at the exact moment the user changed speed. Tested in [`audio/state.rs::speed_change_preserves_position_continuity`](../../src-tauri/crates/app/src/audio/state.rs).

### Analytics accounting

Both `current_position_ms()` and `session_listened_ms()` multiply the wall-clock delta by the active speed, so analytics credit and the 15 s "Recently played" threshold fire on **track-time covered**, not wall-clock listened. Listening to a 6 min track at 2× for 3 min wall-clock counts as 6 min of that track for the heatmap / Top Tracks aggregates.

### Persistence & commands

`profile_setting['audio.playback_speed']` (float). Restored at boot in `player_get_state` via a raw atomic write — NOT through `set_playback_speed`, because the rebase would otherwise move the persisted resume point off the persisted value. Tauri surface: `player_set_speed(value)` + `player_get_speed`. Frontend hydrates via `playerGetSpeed` on mount.

### UI

Speed lives inside the player-bar overflow ("⋯") menu — range slider (step 0.05) + five preset buttons (0.75 / 1 / 1.25 / 1.5 / 2) — rather than a dedicated pill, since most users never touch it. When speed ≠ 1×, the "⋯" trigger surfaces a compact `1.25×` badge in emerald so the user keeps a live indicator without opening the menu. Hidden entirely in Spotify mode (the Web Playback SDK has no speed control).

## A-B repeat

Musicolet-style intra-track loop. Two `AtomicU64` endpoints on `SharedPlayback` (`loop_a_ms`, `loop_b_ms`) — when both are set and `b > a`, the decoder loop in [`audio/decoder.rs::play_track`](../../src-tauri/crates/app/src/audio/decoder.rs) checks the playhead once per packet and seeks back to A whenever it crosses B. Skipped during a crossfade because the loop is a single-track concern (looping mid-fade would fight the cross-track mix). Auto-cleared on every `LoadAndPlay` so the new track doesn't inherit stale endpoints from the previous one.

Three commands cover the lifecycle: `player_set_ab_loop` (set one or both endpoints), `player_clear_ab_loop`, `player_get_ab_loop`. Each one emits `player:ab-loop` so the UI button + ProgressBar markers stay in sync across views without polling.

UI is a tri-state click cycle in [`AbLoopButton`](../../src/components/player/AbLoopButton.tsx) — idle → A captured (amber) → A+B armed (emerald) → clear — with an "A" / "AB" badge over the icon. The PlayerBar's [`ProgressBar`](../../src/components/player/ProgressBar.tsx) renders the endpoints as coloured pin markers (amber A, rose B) with a tinted region between them so the loop is legible at a glance. By default the button lives in the player-bar overflow ("⋯") menu wrapped as a labelled row; pinning it to a primary slot is a one-click toggle in Settings → Lecture (`profile_setting['ui.show_ab_loop']`).

## Queue

[`queue.rs`](../../src-tauri/crates/app/src/queue.rs) — persistent SQLite-backed queue with shuffle (Fisher-Yates with seeded xorshift), repeat (off/all/one), auto-advance and drag-and-drop reorder. The frontend operates on a virtualised list so a 6000-track shuffle doesn't lock the UI.

**User queue vs context tail.** Every `queue_item` carries a `source_type` (`'album'`, `'playlist'`, `'smart'`, `'manual'`, …). The Spotify-style split flows out of that flag:

- `fill_queue` (Play album / Play playlist / Play smart) populates the queue with `source_type = 'album' | 'playlist' | …` from the current view.
- `insert_after_current` (Play next, context-menu action) drops the picks at `current_index + 1` with `source_type = 'manual'` — pushes the rest of the queue down by N.
- `append_to_user_queue` (Add to queue, context-menu action) finds the boundary `MIN(position) WHERE position > current AND source_type != 'manual'` — i.e. the first context-tail item — and inserts the new picks right before it with `source_type = 'manual'`. Falls back to `append` when the entire post-cursor tail is already manual (or there's nothing past the cursor), and to `fill_queue` when the queue is empty.

Net effect matches Spotify's behaviour: the manual block stacks between Now Playing and the album / playlist tail. "Play next" pushes to the top of that block, "Add to queue" stacks at the bottom, and the album resumes once the user queue drains. No tracks get banished to the very end past the rest of the album any more.
