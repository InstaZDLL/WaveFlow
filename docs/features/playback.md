# Playback engine

The audio path lives in [`src-tauri/src/audio/`](../../src-tauri/src/audio). It is a 3-thread lock-free pipeline; see [audio architecture](../architecture/audio.md) for the wider topology and invariants.

## Decoding & output

- **Decoder** — [`symphonia 0.5`](https://crates.io/crates/symphonia) over MP3, FLAC, WAV, OGG Vorbis, AAC, ALAC (M4A). Source samples are converted to interleaved `f32`, channel-mapped (mono ↔ stereo, 5.1 → stereo Lo/Ro per ITU BS.775), then resampled to the device rate by [`rubato 2.0`](https://crates.io/crates/rubato) (`Fft<f32>` + `FixedSync::Input`, with a fast `Passthrough` variant when source rate already matches the device).
- **DSD pipeline** — symphonia 0.5 doesn't decode 1-bit DSD, so DSF (Sony) and DFF (Philips) containers route through [`audio/dsd/`](../../src-tauri/src/audio/dsd/): a custom container parser reads the layout (DSD64 → DSD1024, mono / stereo / multichannel), and a 256-tap windowed-sinc FIR with a Blackman-Harris envelope decimates the bitstream by 64 to land DSD64 at 44.1 kHz, DSD128 at 88.2 kHz, etc. The resulting PCM joins the same channel-convert + resample + ring-buffer pipeline as symphonia output. `ActiveStream` carries a `StreamBackend` enum (Symphonia / Dsd) so seeking and decoder reset stay uniform from the engine's perspective. **Limitation**: real audiophile players use multi-stage halfband cascades for lower CPU at the same SNR; ours prioritises code clarity. DoP (DSD-over-PCM) is not yet wired — the converter always produces PCM.
- **Output** — [`cpal 0.17`](https://crates.io/crates/cpal) on a dedicated thread because `cpal::Stream` is `!Send` on Windows. Samples cross the thread via an [`rtrb 0.3`](https://crates.io/crates/rtrb) SPSC ring (`RING_CAPACITY = 96 000` `f32`s ≈ 1 s @ 48 kHz stereo).
- **Hot-path rules** — the cpal callback never allocates, locks or logs. It only reads the `rtrb::Consumer` and `Atomic*` fields in `SharedPlayback`.

## Spectrum visualizer

Real-time FFT bars surfaced in the immersive Now Playing overlay. Implementation:

- Backend: [`audio/spectrum.rs`](../../src-tauri/src/audio/spectrum.rs) runs on the decoder thread (NOT in the cpal callback — too constrained). Post-EQ samples go through `SpectrumAnalyzer::feed`, which mono-mixes, applies a Hann window, runs a 2048-pt real FFT via `realfft`, then buckets the magnitudes into 48 log-spaced bands (30 Hz → 16 kHz). 50% overlap between successive frames so the visual feels continuous. Throttled to ~30 Hz via a manual `Instant` clock.
- Output is a `player:spectrum` Tauri event carrying a `Vec<f32>` of normalised band magnitudes (0..1, peaks may briefly overshoot).
- A `SharedPlayback::visualizer_enabled` atomic gates the entire path: when off, `feed` returns at the first atomic load — zero allocations, zero FFT cost. Persisted in `profile_setting['ui.visualizer']`, default OFF.
- Frontend: [`SpectrumVisualizer`](../../src/components/player/SpectrumVisualizer.tsx) subscribes to the event and drives a `<canvas>` with `requestAnimationFrame`. Asymmetric decay (jump up fast, fall slow) so transients pop without making the bars look glitchy. Auto-fades to zero on pause so the bars don't freeze mid-pose.

## Crossfade

Real dual-decoder mix in [`crossfade.rs`](../../src-tauri/src/audio/crossfade.rs). When the user enables crossfade, the decoder maintains two `ActiveStream`s during the fade window and feeds an equal-power gain pair (`cos(t·π/2)` / `sin(t·π/2)`) into each so the summed RMS stays flat — no mid-fade dip. The window is clamped to `min(user_ms, duration / 2)` so 30 s clips with a 12 s setting don't start mixing at the 18 s mark.

### Smart crossfade (album-aware skip)

A separate `SharedPlayback::smart_crossfade_enabled` toggle (default ON, persisted in `profile_setting['audio.smart_crossfade']`) suppresses the fade for two consecutive tracks belonging to the same album — concept records / live sets hand off naturally instead of getting smeared. Mechanism:

- The analytics worker's `PrefetchNext` handler looks up the current track's `album_id` and the upcoming track's `album_id` in a single SQLite round trip and writes the boolean result to `SharedPlayback::pending_next_same_album` right before sending `SetNextTrack`.
- The decoder, at mix-decision time, checks both atomics: if smart crossfade is on AND the prefetched track shares an album, it skips the mix branch and falls through to the existing gapless EOF swap (which already handles a sample-accurate hand-off when `pending_next.is_some()`).
- The hint is naturally one-shot: each new prefetch overwrites it, and `LoadAndPlay` paths (manual user clicks) don't go through the mix decision at all, so a stale value can't bleed into an unrelated transition.

ReplayGain is applied **per-stream before the mix** so the two tracks can have very different gains without the louder one swamping the fade.

## Seek

`format.seek()` + `decoder.reset()` + `resampler.flush()`. The cpal callback enters `drain_silent` mode, which (since 70c1968) drains the ring in **one bulk `while consumer.pop()` pass** instead of one sample per output slot — total perceived gap on seek dropped from ~270 ms (one full ring at 44.1 kHz × 8 ch) to ~10-15 ms (one cpal callback period).

After the drain, MP3 sources will emit a few `invalid main_data_begin, underflow` warnings from symphonia: the bit reservoir is invalidated by the seek and the codec recovers within 3-4 frames. Inherent to the format; not a bug.

## Output device picker

[`commands/player.rs::list_output_devices`](../../src-tauri/src/commands/player.rs) → cpal device enumeration. The display name uses `description().extended()[0]` (Windows `DEVPKEY_Device_FriendlyName` — `Speakers (Logitech PRO X Wireless Gaming Headset)`) instead of `description().name()` (`DEVPKEY_Device_DeviceDesc` — just `Speakers`) so multiple endpoints in the same device class stay distinguishable.

The chosen device's name is persisted in `profile_setting['audio.output_device']`. `lib.rs::setup` reads it during boot and forwards it to the audio engine, so playback resumes on the user's preferred sink without waiting for the frontend to settle.

On Linux, enumeration uses ALSA's hint database (`snd_device_name_hint("pcm")`) instead of cpal's `output_devices()` to avoid a 1-2 s freeze + `pcm_dmix` / `pcm_route` stderr spam from probing every PCM card.

## OS media controls

[`media_controls.rs`](../../src-tauri/src/media_controls.rs) bridges the engine to [`souvlaki 0.8`](https://crates.io/crates/souvlaki):

- **Windows** — SMTC. Now-Playing artwork is served to SMTC over a tiny localhost HTTP shim because Windows expects a URL, not a file path.
- **Linux** — MPRIS via D-Bus.
- **macOS** — MediaRemote (NowPlayingInfoCenter).

Initialised after the main window exists (needs an HWND on Windows). State transitions are driven through `transition_state()` so the OS overlay flips at the same instant as the in-app controls; the brief `Loading` state is skipped to avoid a 50 ms "controls flash off" between tracks.

The same `transition_state()` hook also feeds [`discord_presence.rs`](../../src-tauri/src/discord_presence.rs) so the user's Discord profile mirrors the playing/paused state. Documented separately under [Integrations → Discord Rich Presence](integrations.md#discord-rich-presence).

## A-B repeat

Musicolet-style intra-track loop. Two `AtomicU64` endpoints on `SharedPlayback` (`loop_a_ms`, `loop_b_ms`) — when both are set and `b > a`, the decoder loop in [`audio/decoder.rs::play_track`](../../src-tauri/src/audio/decoder.rs) checks the playhead once per packet and seeks back to A whenever it crosses B. Skipped during a crossfade because the loop is a single-track concern (looping mid-fade would fight the cross-track mix). Auto-cleared on every `LoadAndPlay` so the new track doesn't inherit stale endpoints from the previous one.

Three commands cover the lifecycle: `player_set_ab_loop` (set one or both endpoints), `player_clear_ab_loop`, `player_get_ab_loop`. Each one emits `player:ab-loop` so the UI button + ProgressBar markers stay in sync across views without polling.

UI is a tri-state click cycle in [`AbLoopButton`](../../src/components/player/AbLoopButton.tsx) — idle → A captured (amber) → A+B armed (emerald) → clear — with an "A" / "AB" badge over the icon. The PlayerBar's [`ProgressBar`](../../src/components/player/ProgressBar.tsx) renders the endpoints as coloured pin markers (amber A, rose B) with a tinted region between them so the loop is legible at a glance. Hidden by default — enable from Settings → Lecture → "Afficher la boucle A-B" (`profile_setting['ui.show_ab_loop']`).

## Queue

[`queue.rs`](../../src-tauri/src/queue.rs) — persistent SQLite-backed queue with shuffle (Fisher-Yates with seeded xorshift), repeat (off/all/one), auto-advance and drag-and-drop reorder. The frontend operates on a virtualised list so a 6000-track shuffle doesn't lock the UI.
