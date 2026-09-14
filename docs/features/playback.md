# Playback engine

The audio path lives in [`src-tauri/crates/app/src/audio/`](../../src-tauri/crates/app/src/audio). It is a 3-thread lock-free pipeline; see [audio architecture](../architecture/audio.md) for the wider topology and invariants.

## Decoding & output

- **Decoder** — [`symphonia 0.6`](https://crates.io/crates/symphonia) over MP3, FLAC, WAV, OGG Vorbis, AAC, ALAC (M4A), plus **Opus** through an out-of-tree decoder (see below). Source samples are converted to interleaved `f32`, channel-mapped (mono ↔ stereo, and any multichannel source — 3.0 / quad / 5.0 / 5.1 / 6.1 / 7.1 — folded to stereo Lo/Ro per ITU-R BS.775, centre + surrounds at −3 dB, LFE dropped), then resampled to the device rate by [`rubato 2.0`](https://crates.io/crates/rubato) (`Fft<f32>` + `FixedSync::Input`, with a fast `Passthrough` variant when source rate already matches the device). **Network pre-load**: when the source lives on a network share (Windows UNC / mapped `DRIVE_REMOTE` drive, or a Linux gvfs / SMB mount), [`ActiveStream::open`](../../src-tauri/crates/app/src/audio/crossfade.rs) reads the whole file into RAM (under a 512 MiB cap) and decodes from an in-memory `Cursor` instead of streaming — high-latency per-packet reads over the link would otherwise stutter mid-playback. Best-effort: oversize / unreadable files fall back to ordinary streaming. DSD keeps streaming (multi-GB files would blow the cap).
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

## Opus (#581)

symphonia reads Opus containers perfectly well — `symphonia-format-ogg`
ships a complete Opus mapper, so tags and durations were already right
— but it ships no Opus decoder, and `symphonia-codec-opus` does not
exist. So [`audio_format::opus`](../../src-tauri/crates/core/src/audio_format/opus.rs)
implements one over **libopus 1.6.1**, vendored and built statically by
[`opusic-sys`](https://crates.io/crates/opusic-sys).

**Why a C binding.** Two pure-Rust decoders were measured against a
libopus reference rather than assessed from their READMEs. `libopus-rs`
covers CELT only and refuses honestly outside that range — too narrow
to ship, but it fails loudly. `opus-rs` decodes everything and **never
returns an error**, including on the files it gets wrong: a 48 kb/s
stereo file that is 94 % CELT and 6 % hybrid scores 30 dB. Silent audio
corruption with nothing for the player to catch.

**Static on every platform**, which is a packaging decision rather than
a technical one: the Linux packages repackage the release binary rather
than building from source, so a system libopus would mean a runtime
dependency added to three packaging manifests and a library bundled
into the AppImage, to gain nothing. The cost is `cmake` at build time,
which every build environment already has.

**The pre-skip is ours to drop, and that is not obvious.** Every Opus
stream opens with encoder priming that must never be played. The Ogg
reader turns `OpusHead`'s pre-skip into `Track::delay` and hands each
packet a `trim_start` — which is exactly how the Vorbis decoder next
door disposes of its own priming — but the Opus packet parser in
`symphonia-format-ogg` 0.6.1 reports a discard of **zero** for every
packet, so that trim is always empty. Measured: a 3-second file decoded
to 144 312 frames instead of 144 000, the difference being precisely
the 312-frame priming. The decoder counts it itself, credits whatever
the reader did report against what is owed so a future upstream fix
cannot make it drop twice, and does **not** re-apply it after a seek,
where there is nothing to drop and doing so would eat real audio.

RFC 7845 also suggests decoding ~80 ms before a seek point and
discarding it so the decoder has converged. We deliberately do not:
that is audio the listener asked for, and the convergence artefact is
brief and bounded where the loss would not be.

`OpusHead`'s output gain is applied, by libopus itself — RFC 7845 says
a player SHOULD always apply it, because it is part of the file's
intended level rather than a ReplayGain-style suggestion.

**One registry for the whole app.** Playback, the analysis pass and the
scanner's probe each used to reach for `symphonia::default::get_codecs`.
They now share [`audio_format::opus::codecs`](../../src-tauri/crates/core/src/audio_format/opus.rs),
because those three have to agree on what this build can play — a
disagreement between them is what puts an unplayable track in a library.

## ReplayGain

Off by default; the switch lives in Settings → Playback and persists in `profile_setting['audio.replaygain']`.

**Two sources, one scale.** The gain comes from the file's own tags when it has them and from our analysis pass otherwise — [`TrackGain::prefer_tag`](../../src-tauri/crates/app/src/audio/replay_gain.rs). The scanner reads `REPLAYGAIN_TRACK_GAIN` / `_TRACK_PEAK` / `_ALBUM_GAIN` / `_ALBUM_PEAK` and the Opus/Vorbis `R128_*` pair through [`scanner::replay_gain`](../../src-tauri/crates/core/src/scanner/replay_gain.rs) into four columns on `track`, refreshed on every (re)scan. An `R128_*` value is a Q7.8 integer of 1/256 LU referenced to −23 LUFS; it is converted to dB against −18 LUFS on the way in, so everything downstream — tags, `R128`, our own measurement — is on the ReplayGain 2.0 scale. Where both exist the textual tag wins, since it is already on that scale and is what other players will use on the same file. Each field falls back independently: a tagger that wrote a gain but no peak still gets clipping prevention from our analysis.

**An Opus caveat**: the stream header also carries an `output gain` a decoder is required to apply, and adding `R128_TRACK_GAIN` on top of a non-zero one would adjust the same stream twice. Only `R128_TRACK_GAIN` is processed, so **an Opus file with a non-zero header output gain is not supported**: the header value is neither read nor compensated for, and such a file gets no gain from us rather than a doubly-applied one. Taggers overwhelmingly leave the header at 0 and write the tag, which is the case this is built for.

**Track gain or album gain** (#587, `profile_setting['audio.replaygain_mode']`).
Track gain levels every track against every other one, which is what
you want when a song comes up shuffled between two unrelated things. On
a record mastered as a whole it is the wrong answer: the hushed
interlude gets pushed up to meet the single, flattening exactly what
the mastering engineer put there. Album gain applies one gain across
the whole record instead.

Which is right depends on **why** the track is playing, and that was
already known where the gain is applied — `queue_item.source_type`
travels all the way to the decoder. So the default is `auto`: album
gain while a record plays through, track gain otherwise, with no
setting for the listener to manage. Shuffle-by-album counts as a record
playing through, since that is precisely what it produces. `track` and
`album` force the choice.

**The clipping cap switches with the gain**, and this is the part that
is easy to get wrong. In album mode it caps by the **album** peak,
which is the same number for every track on the record, so the cap is
uniform. Capping each track by its own peak would pull tracks down
according to their own loudest sample — re-introducing exactly the
per-track variation album mode exists to remove.

Album numbers are tag-only: our analysis pass measures one track at a
time and has no notion of a record, so a file gets album gain when a
tagger wrote `REPLAYGAIN_ALBUM_GAIN` into it and otherwise falls back
to its track gain — album mode would otherwise do nothing at all on a
half-tagged library. The peak falls back with it, which is a deliberate
trade the other way: an uneven cap is cosmetic, and the cap only ever
binds where the alternative is audible clipping.

**Three knobs on top of the switch** (`player_set_replaygain_options`, persisted per profile, clamped to ±15 dB on the way in and again on the way out of the database):

| Knob | Why |
| --- | --- |
| **Pre-amp** | −18 LUFS is quieter than most systems are set for, so a correctly-normalised library otherwise sounds like it lost volume the moment the switch goes on. |
| **Fallback gain** | Applied to tracks that have no gain from either source, so a half-tagged library doesn't jump every time playback crosses the line. |
| **Clipping prevention** (default ON) | A boost on a track that already peaks near full scale pushes samples past 1.0, and the decoder's final clamp flattens every one of them into distortion. Knowing the peak, the gain is capped at `-20·log10(peak)` instead — the loudest sample lands exactly at full scale. It only ever lowers a gain, so a quiet-peaking track that measured loud is still turned down. |

**A peak we cannot vouch for** (#586). Clipping prevention is only as good as the peak it reads, and rows measured before the BS.1770 switch took theirs over a mono downmix — which under-reports an out-of-phase mix, so the cap derived from it is too generous and the boost it allows clips. Those rows are recognisable because they carry no `analysis_version`; `TrackGain::peak_unverified` marks them — and marks, more generally, any version that isn't the one this build knows, since a row left by a different generation is equally unaccounted for. The limiter then reads the value as a **lower bound** instead of a measurement: the headroom is capped at 0 dB, so no boost survives, while any attenuation the value asks for is still applied in full — a downmix already reading above full scale describes a master that clips harder still. The flag travels with the peak it belongs to, so a file carrying its own `REPLAYGAIN_TRACK_PEAK` clears it. Re-measuring the track is what lifts the restriction, and the library sweep now picks those rows up on its own — the ones *older* than the current version, at least: a profile carried back from a newer build holds measurements this one has no better replacement for, so the sweep leaves them alone while the limiter still declines to boost them.

The total is bounded to [−30, +12] dB regardless, which is what stands between a corrupt tag that got past the parser and a pair of speakers.

The scalar is recomputed **per decoded buffer** rather than baked into the stream at load time, so moving the pre-amp is audible immediately instead of at the next track. One `powf` per buffer, on the decoder thread — never in the cpal callback.

## Seek

`format.seek()` + `decoder.reset()` + `resampler.flush()`. The cpal callback enters `drain_silent` mode, which (since 70c1968) drains the ring in **one bulk `while consumer.pop()` pass** instead of one sample per output slot — total perceived gap on seek dropped from ~270 ms (one full ring at 44.1 kHz × 8 ch) to ~10-15 ms (one cpal callback period).

After the drain, MP3 sources will emit a few `invalid main_data_begin, underflow` warnings from symphonia: the bit reservoir is invalidated by the seek and the codec recovers within 3-4 frames. Inherent to the format; not a bug.

## Output device picker

[`commands/player.rs::list_output_devices`](../../src-tauri/crates/app/src/commands/player.rs) → cpal device enumeration. The display name uses `description().extended()[0]` (Windows `DEVPKEY_Device_FriendlyName` — `Speakers (Logitech PRO X Wireless Gaming Headset)`) instead of `description().name()` (`DEVPKEY_Device_DeviceDesc` — just `Speakers`) so multiple endpoints in the same device class stay distinguishable.

The chosen device's name is persisted in `profile_setting['audio.output_device']`. `lib.rs::setup` reads it during boot and forwards it to the audio engine, so playback resumes on the user's preferred sink without waiting for the frontend to settle.

On Linux, enumeration uses ALSA's hint database (`snd_device_name_hint("pcm")`) instead of cpal's `output_devices()` to avoid a 1-2 s freeze + `pcm_dmix` / `pcm_route` stderr spam from probing every PCM card.

### What the Linux list shows, and what it hides

The hint database answers with ALSA's whole namespace, and most of it is not a device. One sound card came back as six lines — `hw:`, `plughw:`, `dmix:`, `dsnoop:`, `surround40:`, `front:` — under names that mean nothing to the person choosing, and three of those route through the software mixer, so picking one **silently defeats the exclusive output the user has just turned on** (#594).

`present_alsa_hints` is that filter, pure and unit-tested, applied to the rows after the walk rather than during it — the enumeration itself must not change, since reading hints instead of opening every PCM is what keeps the menu from freezing. Three passes:

1. **Routing families go.** `plughw` (a conversion plug), `dmix` / `dsnoop` (the mixer itself), `surround*`, `usbstream`, `null`, and the rate/format plugins. What deliberately stays: `default`, `pulse` and `pipewire` — not hardware, but the right answer for most people most of the time, and on a PipeWire desktop often the only one that works; and `hdmi` / `iec958`, which are how those outputs are reached at all.
2. **A wrapper over hardware already on the list goes.** `front:CARD=PCH,DEV=0` beside `hw:CARD=PCH,DEV=0` is the same output twice, and the `hw` spelling is the one exclusive output can use. The names share nothing, so the pairing is done on the **card token** — the closest thing a hint carries to a driver identity — with a missing `DEV` read as `0`, which is what `sysdefault:CARD=X` means. A card with no `hw:` row keeps its wrapper: hiding the only way to reach a device would be worse than showing an alias.
3. **Rows that would read identically are disambiguated.** Two of the same DAC describe themselves with the same string; the ids differ so both picks work, but the list showed one line twice and the second device was effectively invisible. The card token is appended to both.

### What a device says it can do

The picker listed names, and someone choosing between three outputs on an audiophile player is choosing on facts a name does not carry (#593). `audio/capabilities.rs` answers for one device at a time: the formats it accepts, the rates **each of those formats** runs at, the channel count, and the smallest period the device will take — one quantity asked the same way of every backend, since a sheet showing a minimum on one platform and a maximum on another invites a comparison that means nothing.

**Asked on demand, never during enumeration.** Filling a capability table while listing would bring back exactly the one-to-two second freeze the ALSA-hint shortcut exists to prevent — for every device, every time the menu opens. So it is its own command, called per device once the menu is open, answered on the blocking pool, and memoised for the session. Only real answers are memoised: a device another client was holding must be asked again rather than remembered as unavailable for the rest of the run.

**And the sheet says which question was asked**, because what a driver *declares* and what it will *accept in exclusive mode* are different questions, and a shared-mode path can advertise rates it reaches by resampling. `CapabilitySource` carries that:

| Platform | How | What it means |
| -------- | --- | ------------- |
| Windows | `is_supported_exclusive_with_quirks` per (format, rate) | The strongest of the three: the same call the real open makes, quirks and all (#405). Nothing is initialised, so it is safe while the device plays — including while we hold it exclusively. |
| Linux | `snd_pcm_hw_params` on the raw `hw:` device | The hardware itself, with no plug layer. Opened **non-blocking** so a busy device answers `EBUSY` at once instead of waiting — which also means the device we are playing on exclusively cannot answer, and says so. A name the plug layer *routes* rather than names — `default`, or no pin at all — has no hardware sheet to show: it resolves to card 0, which is not necessarily what it plays through, so it answers unavailable rather than describing the wrong card. |
| macOS | the physical stream formats and nominal rates | What the device reports to the HAL. There is no exclusive-mode question to ask: hog mode takes the device as it is rather than negotiating a format. |

The rates are kept **per format**, not per device, because the two are not independent: a DAC that takes 32-bit to 96 kHz and 24-bit to 192 kHz accepts neither pair the two maxima would suggest. In the menu, each row therefore carries its deepest format and the top rate *that format* runs at, with the Hi-Res marker decided on that same pair; the sheet behind it adds the channel count, the smallest period the device takes, the formats as a set, and the rates **grouped into named tiers** — CD quality, Hi-Res, Studio, Ultra Hi-Res — with the numbers themselves on the tooltip. A column running from 44.1 to 384 tells an expert something and a normal user nothing; the tiers tell both.

### The pin and the endpoint are two different things

Every open path falls back to the default endpoint when the pinned name is no longer enumerated — `build_stream_inner` in shared mode, `pick_device` under WASAPI, `resolve_device` under CoreAudio — and each used to leave nothing behind but a `warn!`. `OutputHandle` kept the **requested** name, deliberately, so the pin survives until the device comes back; the picker read that field and ticked a device that was playing nothing (#612).

So the handle now carries both. `device_name` is still the pin. `opened_device` is what the backend actually opened, and each backend fills it with what it can honestly claim:

- **cpal shared** — the display name read back off the device that was chosen, so a fallback names the endpoint it landed on.
- **WASAPI** — `Device::get_friendlyname()` on the endpoint `pick_device` returned.
- **ALSA** — the pin itself: `resolve_hw_device` either places it on a real card or fails outright, never quietly taking another. An absent pin lands on card 0, which has no name in the picker's terms, so it reports unknown.
- **CoreAudio** — the pin when it matched, unknown after the fallback: a device id is all we have there, and inventing a name would be the very bug.

`player_list_output_devices` flags `is_active` from the opened device (falling back to the pin, then to the OS default) and `is_pinned` from the pin. They differ exactly during a fallback, which is what lets the menu mark the pinned row unavailable rather than pretend it is playing.

A pin that has vanished matches no enumerated row, so the listing appends it as a row of its own — `is_pinned`, never `is_active`, since it isn't there to be playing. Without that the flag would be absent in the one case it exists for, and the menu would say nothing at all about the fallback.

Both names come from `current_output_devices()`, which reads them under a **single** lock. Two separate reads let a rebuild swap the handle in between, and the listing would then describe one stream's endpoint next to another stream's pin.

### The pin outlives the handle

`self.output` holds an `OutputSlot`, not a bare handle: the installed stream **and** the device the user picked, under one lock.

The pin needs to survive the handle because the handle really does go away — `publish_output_lost_if_gone` exists for exactly that state, and both `force_rebuild_output` and `set_output_device` reach it when a spawn fails after the old stream was released. While the pin lived only inside `OutputHandle`, the engine answered "nothing pinned" in that window: a forced reopen targeted the OS default rather than the user's device, the picker could not flag the pinned row it had just failed to open — the very case the marker above was written for — and MPD reported no device at all (#630). The persisted `profile_setting['audio.output_device']` was read exactly once, at startup, and never consulted again.

`OutputSlot::pinned_device()` answers from the live handle first and falls back to the seeded pin, and it is what `RebuildDevice::Pinned`, the DoP re-open and both accessors resolve through. Two details are deliberate:

- **One lock, not two.** A separate mutex for the pin would let a reader pair "nothing is playing" with a pin read after a new stream was installed — the same shape of defect the single-lock read above had to fix.
- **`set_output_device` still compares the *handle*, not `pinned_device()`.** With no stream installed, picking the pinned device again has to reach the rebuild; comparing against the pin would early-return on the equality check and leave the user no way back.

### Reading the device under the lock that replaces it

`set_exclusive_output` used to read the active device with one acquisition, release it, then take the lock again to rebuild. In between, `player_set_output_device` could install **and persist** device B; the toggle then rebuilt on A, the name it had captured, and the stream contradicted the saved preference until something else rebuilt (#629). It now resolves its target from the slot **after** taking the lock, the same cure `reopen_output_device` took with `RebuildDevice::Pinned`.

That leaves the whole surface honest: `set_output_device` holds one acquisition throughout, and the device-error recovery computes its own target deliberately. Those three are every entry point that changes the **device**; `switch_output_for_track` replaces the handle too — it is the DoP re-open, driven by the track rather than by the user — and it follows the same discipline, resolving its target from the slot inside the acquisition it rebuilds under.

### Re-selecting the active device

`set_output_device` returns early when the pick equals the current pin, which is right — but the picker also disabled its active row, so a user whose audio had drifted onto another endpoint had no way to force a fresh lookup except picking another device and coming back. The row is now clickable and calls `player_reopen_output_device`, which goes to `force_rebuild_output` — the same path device-error recovery uses. It changes no preference, so nothing is persisted.

Two details make that reopen safe. It carries the guards every deliberate swap needs and that `force_rebuild_output` doesn't own — `begin_deliberate_output_change()` around it, cancelled if nothing was installed, and the exclusive preference ANDed with the `#322` session suppression so a click can't revive a mode a flap storm gave up on. And it names its target as `RebuildDevice::Pinned` rather than reading the pin beforehand: that read would take the output lock and give it back, and a device pick landing in the gap installs *and persists* another device, after which the rebuild would reinstall the stale one. `Explicit` keeps the recovery path's own deliberately computed target. The same window still exists in `set_exclusive_output`, which predates this and does its teardown inline — #629.

### Following the OS default

A stream opened with no pin is bound to whatever endpoint was the default **at that moment**, and it stays there. cpal 0.17 exposes no notification API at all, so before #627 a default that moved — a headset waking, an HDMI sink appearing — left playback on the old device; after #612 the picker at least said which one that was.

[`audio::default_device`](../../src-tauri/crates/app/src/audio/default_device.rs) is the subscription, one implementation per platform:

| Platform | Mechanism |
| -------- | --------- |
| Windows  | `IMMNotificationClient` registered on the `IMMDeviceEnumerator`, on a thread of its own |
| macOS    | a HAL property listener on `kAudioHardwarePropertyDefaultOutputDevice` |
| Linux    | nothing — PipeWire and PulseAudio migrate a running stream to the new default sink themselves, and ours is one of their clients (cpal opens the `default` alias) |

Four things about it are load-bearing:

- **Only the console role.** Windows fires `OnDefaultDeviceChanged` once **per role** for a single physical change, and we keep `eRender` + `eConsole` only — because that is the role both of our open paths ask for: cpal's `default_output_device` calls `GetDefaultAudioEndpoint(flow, eConsole)`, and so does `wasapi::get_default_device` on the exclusive side. Reacting to multimedia or communications would rebuild for a default we would never have opened.
- **The listener rebuilds nothing.** Both platforms hand the event to `output::schedule_default_device_follow`, which takes the same 300 ms backoff and the same `RebuildGate` the device-loss recovery takes. That matters more here than there: one physical change can produce several notifications, and our own reopen makes the outgoing stream fail, which schedules a recovery rebuild of its own.

  Two details keep the shared gate honest in both directions. It is armed **only once the cheap checks say a rebuild will really happen** — arming for a follow that turns out to be a no-op would open a settle window a genuine `DeviceNotAvailable` gets swallowed by, dropped and never retried, which is silence until the user intervenes. And a gate that is busy makes the follow **defer, not drop**: `FollowOutcome::Deferred` sends the scheduler back after the settle window, three times at most. A default moved twice inside two seconds is two notifications, not a repeating signal, so a dropped one would leave the stream on the intermediate device for good. Every retry re-reads the pin and the default, so it converges on the current state rather than replaying a stale one.
- **Only while nothing is pinned**, and the decision is made **inside the lock that installs pins**. A user who picked a device asked for that device. `AudioEngine::follow_os_default_output` reads the pin first, but that read is advisory — a pick landing between it and the rebuild would slip through; `RebuildDevice::OsDefaultIfUnpinned` asks again under `force_rebuild_output`'s own acquisition, and that answer is the binding one. Same lesson as #629 above.
- **Nothing that changes is left unchanged, nothing else is touched.** A default that is already the endpoint we are playing on is not reopened (a rebuild is an audible gap), and a default that vanished entirely is not followed at all — rebuilding onto "no device" would drop audio we still have, and a device we really lost arrives as `DeviceNotAvailable`, which owns its own recovery. `should_follow_default` is that decision, pure and unit-tested. Exclusive output follows the preference minus the #322 session suppression, but does **not** reset it and does **not** record a flap: the system moving its default is not a device resetting under us, and counting it would let a few legitimate switches disable exclusive for the session.

## Output-stream lifecycle & recovery

Three paths replace the output stream, and they must all end in the same place: `exclusive_output_active` updated and a `player:audio-mode-changed` event emitted, because that event is the only thing that keeps Settings' Exclusive-output toggle honest ([`ExclusiveModeCard`](../../src/components/views/settings/ExclusiveModeCard.tsx) re-reads on it).

| Path                   | Trigger                                 | Order                                                                                                                    |
| ---------------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| `set_output_device`    | user picks another endpoint             | order from `must_release_before_reopening`; a failed open after releasing first reopens the previous device              |
| `set_exclusive_output` | user toggles the mode                   | order from `must_release_before_reopening` — see below                                                                  |
| `force_rebuild_output` | automatic recovery after a device error | the same rule                                                                                                            |

[`must_release_before_reopening`](../../src-tauri/crates/app/src/audio/engine.rs) owns the order, and answers "release first" in **two** cases:

- **The old stream is exclusive**, on any platform. It owns the device outright, so nothing — shared _or_ exclusive — can open that device until it lets go. Re-opening the **same** endpoint while an exclusive stream still holds it always fails, and when that failure landed inside `set_exclusive_output` the command returned `Err` before persisting anything, leaving the toggle latched on the mode the user was trying to leave (#405). This is the #322 / #405 lesson.
- **We are entering exclusive on macOS**, even from a *shared* stream. CoreAudio registers hog mode against a **PID**, not a stream, so the client it would have to evict is our own cpal stream in this very process — and it evicts nothing. The new `AudioUnit` then comes up on a device the old one is still driving and renders nothing: no sound, position counter frozen. On Windows a shared client is no obstacle at all (measured on Windows 11: an exclusive `Initialize` succeeds alongside one of our own shared streams on the same endpoint) and Linux's reservation protocol makes the sound server hand the card over, so neither needs the widening — which is exactly why the macOS case stayed hidden until PCM hog mode existed.

Spawn-first is the order we want everywhere else: a failed open then costs nothing, because the stream the user is listening to is still installed and still playing. Releasing first costs less than it looks in the macOS case, because `spawn_output_with_mode` falls back to shared mode on its own — so a refused exclusive open still leaves the caller holding a stream. It is only when the shared fallback *also* fails that there is no output thread at all, and that path is the one described at the end of this section.

`set_output_device` needs the first case too (#604): a pinned device that has vanished falls back to the default endpoint, which can be the very one the old exclusive stream still holds. Releasing first there gives up the rollback spawn-first had, so the switch buys it back: when the new device won't open at all, it reopens the previous one and still returns the error, so the new choice isn't persisted. If the previous one won't reopen either, it schedules the same rebuild `set_exclusive_output` does, aimed explicitly at that previous device. It decides on the release, not by comparing endpoints — a name is no identity (ALSA reaches one card as `default`, `plughw:0,0` and `hw:0,0`), and a wrong "different" verdict would put the collision back.

Device loss reaches the recovery path from two independent places, since the two backends have separate failure surfaces:

- **cpal shared** — the stream's `err_fn` callback fires on an arbitrary thread.
- **Exclusive backends** — each output loop returns an `ExitReason` whose `DeviceLost` variant is re-checked against the shutdown channel first, so a deliberate teardown isn't mistaken for a failure: [`wasapi_exclusive`](../../src-tauri/crates/app/src/audio/wasapi_exclusive.rs) (a failed `wait_for_event` / `write_to_device`), [`alsa_exclusive`](../../src-tauri/crates/app/src/audio/alsa_exclusive.rs) (a write that fails past recovery), [`coreaudio_exclusive`](../../src-tauri/crates/app/src/audio/coreaudio_exclusive.rs) (the `IsAlive` property listener, with a periodic `get_hogging_pid` query as fallback).

Both then call the shared [`output::notify_device_lost`](../../src-tauri/crates/app/src/audio/output.rs) (park the player, emit `player:state` + `player:error`, sync the OS media controls) and [`output::schedule_device_rebuild`](../../src-tauri/crates/app/src/audio/output.rs) (300 ms backoff, then a same-device rebuild).

### What a rebuild puts back

Every rebuild interrupts the decoder with a `Stop` before it can swap the ring producer, so it owes the session something afterwards. **What it owes is whatever the decoder was actually holding, not what was playing when the rebuild started** (#634).

The difference is a few hundred milliseconds wide — the cost of an exclusive open — and a track picked inside it used to disappear entirely: it was delivered before the `Stop`, loaded, unloaded by that `Stop`, and the rebuild's own resume, carrying an older intent, was then dropped on arrival exactly as [the ordering rule](#ordering-the-loads) requires. Nothing played, while the player bar named the track the user had just picked. The engine could not re-dispatch that track either, because only its producer knew the payload — a path, or a URL plus a fallback and a ReplayGain value.

So the decoder records it. `SharedPlayback::last_load` holds the load it most recently **accepted**, payload included, written inside `accept_load` — the one funnel every load passes through, including the auto-advance, which sends straight down the channel without going through `AudioEngine::send`. A rebuild reads it after the swap and re-dispatches it **with that load's own intent**, never a fresh one: an intent already delivered is accepted again (the rule drops what is *older*), while a pick that claimed after it still wins. Minting a new intent there would do the opposite and outrank a selection that had not reached the channel yet.

Two things fell out of that. The resume no longer goes to the database for a file path, so it is synchronous and keeps the gain and the **source** the track came from — a play credited to `device-rebuild` was hidden from every statistic that filters on the source, because the audio device changed. And the position, which has to be read *before* the stop (opening the replacement writes its own sample rate into the shared block, and a position derived from the old rate's sample count is simply a wrong number), is stamped with the load it belongs to: `resume_start_ms` uses it only when the decoder was on that same load, by intent *and* by track, and otherwise starts where the load itself asked to.

A rebuild also has to **interrupt** a session that is still loading, and that is a separate question from what it resumes. The decoder is inside `play_track` from the moment it accepts a load, and a `SwapProducer` that reaches it there is dropped: both drains fall through to a catch-all, on the stated assumption that the engine always sends a `Stop` first. Treating a loading track as "nothing playing" broke that assumption — the swap was lost, and the decoder went on writing into a ring whose consumer had just been torn down, which is silence until something else rebuilds the output. `Loading` therefore counts as a session, and resuming it is the right answer too: the load is in `last_load` and goes back out under its own intent.

### What a rebuild does **not** put back

Two sessions are parked instead of resumed, and `AudioEngine::park_session` handles both: it keeps the load so play picks up *this* session, lands the state on `Idle` (it cannot stay `Paused` — the `Stop` unloaded the track and the decoder ignores a `Resume` with nothing loaded, so the button would be dead), and writes a library track's resume point so the session survives a restart. `resume_last` prefers that park over the persisted resume point, which is what lets a radio station or a server track be parked at all: the resume point is always a *library* track's, so before #617 parking one of those brought back the last local track instead, and they were resumed unconditionally.

- **The user had paused it** (#611), whichever rebuild is running. The question is asked of two sources, because one of them lags: `paused_output` is the decoder's answer, raised when the `Pause` is *processed*, and `AudioEngine::pause_pending` is the user's, recorded at the `send` boundary. A rebuild reading only the first inside that gap decided `Play` for a session that had just been paused — and its resume then cleared the flag and started the music. `notify_device_lost` has turned a playing session into `Paused` by the time the recovery runs, so the decision reads `paused_output`, which on the playback path only the decoder's own pause raises. Resuming these started music on whatever device the system fell back to — and the same was true of a *deliberate* rebuild: picking a device or flipping the exclusive toggle while paused started the track, because those two paths asked "was something loaded" rather than "was it playing". All three now run the same `rebuild_resume` and park what they should not start.
- **The device went away and the fallback is a different one** (#617), while `audio.pause_on_device_loss` is on — it is, unless the profile says otherwise. This is the reported case: headphones off, Windows moves the output to the built-in speakers, the album keeps playing out loud. A device that flaps and comes back is *not* this case: the rebuild reopens the same endpoint, `endpoint_moved` says no, and the automatic recovery of #175 works as it always did. An endpoint neither side can name never counts as a move either — a wrong "moved" stops the music for a user who asked for none of this, a wrong "not moved" only costs the pause.

The resume point is written against the profile that was active when the rebuild ran (`require_profile_pool_for`), and skipped when that can't be read without waiting — a switch is then under way, and must not receive it.

### A device that disappears is not a default that changes

Unplugging raises both signals at once: the stream breaks, **and** the system default moves. They arrive on two independent paths — the device-loss recovery above, and the [default-device follow](#following-the-os-default) — and each one takes the same `RebuildGate`, so whichever armed first decided what happened: the pause would land, or not, on a coin toss.

`notify_device_lost` therefore stamps `AudioEngine::note_device_loss` at the moment of the error, before either backoff, and the follow stands down for `DEVICE_LOSS_OWNERSHIP` (3 s). It settles rather than defers: the recovery ends on the endpoint the system fell back to, which is the very endpoint this follow would have opened. The follow keeps its own job — a default the user re-points elsewhere while the device they were on is still there is not a loss, and playback follows it without pausing.

Two gates keep the recovery from thrashing:

- **`RebuildGate`** (`REBUILD_SETTLE_WINDOW`, 2 s) — one rebuild per burst of device errors, and also the gate a default-device change goes through (see above). `begin_deliberate_output_change()` opens the same window around a mode toggle, because seizing the endpoint exclusively kicks the outgoing shared client off it and that self-inflicted `DeviceNotAvailable` would otherwise schedule a rebuild that undoes the switch. Both paths that arm the gate release it through `RebuildGateGuard`, on every exit including a panic — a rebuild that bailed out while `armed` stayed latched would leave no later device event able to schedule anything.
- **`FlapWindow`** (`EXCLUSIVE_FLAP_THRESHOLD` / `EXCLUSIVE_FLAP_WINDOW`) — a device that resets on every exclusive grab gives up on exclusive for the rest of the session (session-only: the persisted preference is untouched, so the next launch tries again). Cleared by an explicit toggle or device switch.

Every failure path that ends with no output thread at all publishes `exclusive_output_active = false` + the event before returning the error — a toggle describing a stream that no longer exists is the exact shape of #405.

### Saying what actually happened

Playback can become something other than what was asked for, and for a long time the only trace was a log line — `player:error` reached a `console.error` and stopped there, and the three exclusive backends each fall back to shared mode on their own. #597 closes that, with two registers that must not be confused:

- **`player:error` is a fault.** The device went away, the file would not open, the decoder crashed. It carries a technical `message` — kept, because that is what a bug report needs — and a `kind` (`device-lost`, `track-failed`, `stream-failed`, `decoder-crashed`, `decoder-stopped`, `offline`) the UI turns into a sentence in the user's language. The message rides in the tooltip; the sentence is what is on screen. An unknown kind falls back to a generic sentence rather than rendering a raw key, so a frontend and a backend of different vintages still say something sane.
- **`player:notice` is not.** Exclusive requested and refused, DoP refused by the DAC, playback parked because the device went away (#617). Nothing failed — playback works — it simply contradicts a choice the user made, and that reads differently. `AudioEngine::publish_output_mode` decides, because it is the single place a successful open records what it opened *as*, and `announce_output_notice` emits **only on a transition**: a DAC that never accepts exclusive says so once, not at every track.

`AudioEngine::output_mode` is the same question asked as state rather than as an event: `shared`, `exclusive`, `dop`, or `exclusive-refused`. It is computed in the engine, under one acquisition of the output lock, so the badge in the player, the notice and the Settings card cannot drift apart. `exclusive-refused` is precisely the state the UI could not name before: `player_get_exclusive_output` reports what *engaged*, and nothing reported what was *asked for*.

On screen, `PlayerContext` owns both listeners and keeps one slot — the newest message describes the situation, and stacking them would nag. `PlaybackAlertToast` renders it in the two registers; `AudioQualityFooter` carries the badge, for the three modes that say something. Shared mode gets no chip: it is the normal case, and a badge on every track is noise rather than information.

### Playing at the track's own rate

Exclusive output takes the system mixer out of the path, but the sample rate stayed a **preference**: every backend opened at a rate the *device* offered and the decoder's resampler met it. Better than the system doing it — it is our resampler and nothing else is mixed in — but not the same as the source reaching the DAC untouched, which is why the word "bit-perfect" left the exclusive-output copy in #577.

`audio.match_source_rate` (#600) is the other half, and it is **off by default**. Not because it is worse: reopening the device costs an audible gap, so every rate change becomes a break in the music, and for most listeners our resampler with nothing else in the path is the better trade. The preference is what decides, and the decision is stated rather than discovered.

It avoids the resampler rather than abolishing it: a playback speed other than 1× feeds rubato a deliberately false source rate whatever the device is opened at, and that is the user asking for it. With it on, the decoder asks for the track's rate **after** `ActiveStream::open` — that is when the container has told us what it is — and before the first packet, because the resampler is built against whatever the output ends up at. Four conditions rule it out, in order: the preference, a DoP format that already pinned the rate (a demand must not be fought by a preference), an output that does not own its device (in shared mode the mixer is in the path whatever we open at), and a container that declares no rate at all — AAC in MP4 only reveals its rate once decoding starts, and re-clocking a DAC to a guess is worse than resampling.

`RequestedFormat` is what carries either demand to the backends, and the difference between them is what a refusal means: DoP fails the open (the caller then plays DSD → PCM), a rate request falls back to a rate the device does offer. Per backend:

- **WASAPI** — the track's rate goes to the head of the candidate list, on each layout the device offered, with the device's own rates kept behind it (`layouts_at_requested_rate`, pure and unit-tested). Deduped on the (rate, channels) pair actually probed, since each probe is a COM round trip.
- **ALSA** — the rate rides into `open_pcm_negotiated` as the one to ask the card for, ahead of whatever the last stream opened at. `set_rate` is asked with `ValueOr::Nearest`, so a card that cannot do it opens at what it can.
- **CoreAudio** — the device is re-clocked through the same `find_matching_physical_format` + `set_device_physical_stream_format` the DoP path uses, with the same guard putting the old format back on every exit path. Without that, quitting a 96 kHz track would leave every other app on the machine talking to a DAC clocked at 96 kHz.

**The guard that keeps this from thrashing** is `AudioEngine::last_requested_rate`: the next track compares against what the last open *asked* for, never against what it got. A device that refused 96 kHz has its own rate installed, and comparing against that would tear the output down and rebuild it for every single track, forever. It is recorded inside `publish_output_mode`, so an open that asks for nothing in particular — a device switch, a mode toggle — clears it rather than leaving a stale rate behind.

The pill in the pipeline popover needs no change: it already compares the source rate against the output rate, so it starts telling the truth when this is on and stops the moment a device refuses.

## OS media controls

[`media_controls.rs`](../../src-tauri/crates/app/src/media_controls.rs) bridges the engine to [`souvlaki 0.8`](https://crates.io/crates/souvlaki):

- **Windows** — SMTC. Now-Playing artwork is served to SMTC over a tiny localhost HTTP shim because Windows expects a URL, not a file path.
- **Linux** — MPRIS via D-Bus.
- **macOS** — MediaRemote (NowPlayingInfoCenter).

Initialised after the main window exists (needs an HWND on Windows). State transitions are driven through `transition_state()` so the OS overlay flips at the same instant as the in-app controls; the brief `Loading` state is skipped to avoid a 50 ms "controls flash off" between tracks.

Play and Toggle from the overlay go through [`player_actions`](../../src-tauri/crates/app/src/player_actions.rs) rather than sending `AudioCmd::Resume` to the engine. The decoder only handles `Resume` inside its pause loop, so with nothing open it was dropped and Play did nothing at all — after a launch, and at the end of the queue (#609). `player_actions::play` resumes or loads the persisted resume point and never pauses; Toggle follows the tray's rule.

The overlay also learns about the restored track **at launch**: `player_get_state` publishes it paused, at its persisted position, starting no audio — and only while the engine holds nothing, since once a track is loaded the decoder's own transitions own the overlay, and that command runs again on profile switch and re-hydration. Before that the only caller of `update_metadata` outside live radio was `emit_track_changed`, on an actual track start, so there was no session for Play to appear on (#609). What the `PlatformConfig` souvlaki is given here cannot express is advertising Previous / Next only when they would do something.

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

[`queue.rs`](../../src-tauri/crates/app/src/queue.rs) — persistent SQLite-backed queue with shuffle (Fisher-Yates with seeded xorshift), repeat (off/all/one), auto-advance and drag-and-drop reorder.

**Shuffle is three-way** (#618): off, tracks, albums. In album mode
only the order of the records is randomised — inside each one the
tracks are put back into disc and track order, so a record that arrived
in the queue scrambled still plays the way it was pressed. The record
you are in carries on rather than restarting: the current track keeps
position 0, the rest of its album follows, and the tracks before it
come round at the end. A track with no album is its own record, so
loose files still shuffle like tracks instead of being welded into one
block that always plays together. The grouping is a pure function
([`album_runs`](../../src-tauri/crates/app/src/queue.rs)) so it is
testable without a database and nothing about it depends on chance.

Persisted as the existing on/off switch crossed with a grouping rather
than one three-valued key. `player.shuffle` stays authoritative for "is
shuffle on", which is what MPD's `random` flag maps onto and what an
older build reads; `player.shuffle_grouping` is remembered while
shuffle is off, so a preference for whole records survives an off/on;
and a profile that predates this needs no migration. MPD's `random 1`
therefore turns shuffle on **without** forcing tracks — a remote that
cannot express the grouping should not quietly undo it.

**A reorder keeps each row's source.** `write_queue_order` used to
write every row back as `source_type = 'manual'`, which threw away the
source a `play_event` is attributed to and the boundary `fill_queue`
uses to tell queued-up "play next" items from the source queue around
them — so shuffling an album silently cost both. Sources now survive,
one per occurrence, so a queue holding the same track twice hands each
copy back its own. The frontend operates on a virtualised list so a 6000-track shuffle doesn't lock the UI.

**User queue vs context tail.** Every `queue_item` carries a `source_type` (`'album'`, `'playlist'`, `'smart'`, `'manual'`, …). The Spotify-style split flows out of that flag:

- `fill_queue` (Play album / Play playlist / Play smart) populates the queue with `source_type = 'album' | 'playlist' | …` from the current view.
- `insert_after_current` (Play next, context-menu action) drops the picks at `current_index + 1` with `source_type = 'manual'` — pushes the rest of the queue down by N.
- `append_to_user_queue` (Add to queue, context-menu action) finds the boundary `MIN(position) WHERE position > current AND source_type != 'manual'` — i.e. the first context-tail item — and inserts the new picks right before it with `source_type = 'manual'`. Falls back to `append` when the entire post-cursor tail is already manual (or there's nothing past the cursor), and to `fill_queue` when the queue is empty.

Net effect matches Spotify's behaviour: the manual block stacks between Now Playing and the album / playlist tail. "Play next" pushes to the top of that block, "Add to queue" stacks at the bottom, and the album resumes once the user queue drains. No tracks get banished to the very end past the rest of the album any more.

### Ordering the loads

Handing a track to the decoder is never immediate: a profile snapshot, the
queue read, a ReplayGain lookup — and for a remote queue a reconciliation
lookup and a streaming ticket — all happen first. Seventeen sites do this,
and each of them used to send its load whenever its own preparation
finished, so two loads started close together reached the decoder in the
order they got *ready*, not the order they were asked for. Press Play from a
stopped player and then Next within a few tens of milliseconds, or send
`play` then `next` from a client, and the slower of the two ended up in
charge: the track the user had already left behind started playing (#622).

Every load now carries a **`LoadIntent`**, a monotonic number claimed from
`AudioEngine::next_load_intent` **when the intent starts** — before the
first await, not before the send. The decoder keeps the highest intent it
has been handed in `SharedPlayback::newest_load_intent` and drops any load
below it, so a preparation that finishes late is discarded instead of
overwriting a newer selection.

**Where "the intent starts" actually is** takes care in three places that
are not a Tauri command:

- **The auto-advance** starts when the track *ends*, so the decoder claims
  the intent there (`handle_playback_outcome`) and it travels inside the
  `TrackEnded` / `RemoteTrackEnded` message. Claiming it in the analytics
  task instead would put it after the channel hop *and* after the
  `play_event` write, the repeat-mode read and the queue advance — ranking
  the auto-advance above a Next the user pressed in between.
- **An output rebuild** (a device error, a device switch, the exclusive
  toggle) claims its intent with the **track snapshot** it will
  re-dispatch, before the stop, the device open and the producer swap. An
  exclusive open costs tens to hundreds of milliseconds, and an intent
  claimed after it would rank the resume above a track the user picked
  while the device was reopening.
- **The remote module's shared tails** claim none of their own. `advance`
  takes the intent from its caller, because a Next claims it when the key is
  pressed and the auto-advance when the track ended; `play_entries` takes it
  for the same reason — `start` and `play_track_ids` read their entries out
  of SQLite first (one query per track, for a whole album), and an intent
  claimed at the end of that would outrank a local track the user picked
  while it ran.

Three details are what make it hold:

- **The token is compulsory.** `LoadIntent`'s field is private, so a load
  command cannot be built without asking the engine for one. Serialising
  only the two paths a bug report names would leave the other fifteen
  reordering exactly as before, while reading as though ordering were
  guaranteed.
- **Arbitration happens at receipt, not at execution.** The drain returns
  `ControlFlow::LoadNext`, which stops the current track before anything
  looks at the payload; a load discarded any later would leave the decoder
  with nothing playing at all. The three receive points — the idle loop,
  the drain between packets, the drain inside the pause loop — all go
  through `accept_load`.
- **The comparison is against what arrived**, never against the engine's
  allocation counter. An intent can be claimed and never sent — Next on an
  exhausted queue, a track row that has since vanished — and that must not
  silence a load still on its way.

**A producer must not publish for a load that will be dropped.**
`resume_last` is the one path that publishes state ahead of the decoder —
it parks the player on `Loading` so a second Play cannot read `Idle`
(#609) — and it now checks `load_intent_superseded` first: a resume whose
load the decoder is going to drop emits no track change and touches no
state, because relabelling the player bar over the track that is actually
playing, or parking `Loading` on top of the decoder's own transition, would
leave every surface that gates on `Loading` dead for the session. The check
is best effort by nature (only the decoder knows what it was handed), so
the `Loading` write itself goes through `SharedPlayback::try_set_state`: a
compare-exchange, so it can never overwrite a transition published for
another track. The same rule covers the one producer whose publication is
not cosmetic: starting a **remote session** installs a queue that takes
over next / previous and end-of-track for every surface, so
`remote::playback::play_entries` installs nothing when its intent is
already superseded, and rolls its install back — through
`RemotePlayback::clear_if`, which fires only while the session is still
the one it installed — when it is superseded during the ticket
round-trip. "Still the one it installed" counts navigations, not just
installs: a jump or a step inside that session makes it the navigator's,
and a rollback that ignored them would clear the queue the user can
hear. Without that, a remote start the user had already
abandoned would reinstall itself on top of the clear
`emit_track_changed` performs, which is the invariant that module
documents.

### Claiming the dispatch before publishing it

Dropping the load was only half the cure. The producer of that dropped
load had already published: `emit_track_changed` had put its title in the
player bar, and the queue paths had already written `queue.current_index`.
So the interface named one track while another played, and the cursor sat
on a row nothing was playing — which is where the *next* auto-advance
stepped from (#632).

So the arbitration moves one step earlier, to the producer:
`AudioEngine::claim_dispatch` is a compare-and-set on the **same**
high-water mark the decoder reads. It succeeds only for an intent at least
as new as the newest already claimed, publishes that claim in the same
operation, and is called immediately before the **first side effect** —
after which a `false` means give up entirely. It does not replace the
decoder's check: two claims can succeed in order and still reach the
channel out of order, and only the decoder sees what was delivered.

The shape every producer takes:

```
peek where the step lands   →  fetch ReplayGain
  →  take the publish lock  →  claim  →  write the cursor  →  emit  →  send
```

Two things make that sequence hold:

- **Peek and commit are separate.** `queue.rs` grew `peek_step`,
  `peek_jump` and `commit_index` beside `advance` / `jump_to`, with the
  arithmetic itself in a pure `stepped_index` (so wrap-around, repeat-one
  and the empty queue are unit-tested without a database). A producer
  that never claims still uses the old pair.
- **The publish half runs under one lock**
  (`AudioEngine::lock_publish`), held across the claim and everything it
  authorizes. The claim cannot order that by itself, for two reasons that
  both bite: `commit_index` is a database write, so an older producer can
  be overtaken *during* it and land its cursor write last; and the
  runtime is multi-threaded, so "no await between the claim and the send"
  buys nothing against a producer running on another worker. Under the
  lock, claim order **is** publish order — and the claim keeps its own
  job, because lock acquisition can invert intent order and something has
  to tell the older one to stop.

**What the lock covers is the queue snapshot and everything downstream of
it.** It is taken before the `peek_*` that reads the queue, so the index a
producer commits always belongs to the queue it read that index from —
committing it after somebody else replaced the queue would put the cursor
on a track this load never resolved. The ReplayGain lookup sits inside the
serialized section at the stepping paths (`player_next`,
`player_previous`, `player_jump_to_index`, `player_actions::step`, the
auto-advance), because it happens after the lock is taken; it is a single
indexed read, and the alternative — dropping the lock for it and taking it
again — would reopen the window it exists to close.

What stays outside is the preparation that precedes the queue: the profile
snapshot and the pool lease. Two paths are shaped differently, each
because one genuinely slow step must not be serialized:

- **`player_play_tracks` claims twice.** It cannot peek — replacing the
  queue *is* its effect — so it claims before `fill_queue`, the last
  moment at which giving up is free, and holds the lock across the
  replacement — `fill_queue` is inside the serialized section, and a Next
  pressed during it waits, then steps through the album it was given
  rather than the one it replaced. The lock then goes back the moment the
  queue is replaced and the panel told, because what follows can be slow
  in a way no other surface should wait on: the ReplayGain read, and, when
  the file is gone, an HTTP round-trip to mint a streaming ticket. Both
  branches re-take it and re-claim immediately before they publish, and
  the same intent re-claims successfully as long as nothing newer did.
- **A remote session rolls back instead.** `play_entries` claims, then
  mints a streaming ticket over HTTP; holding the publish lock across a
  network call would park every other surface, so it keeps the
  `load_intent_superseded` + `clear_if` rollback described above.

`SetNextTrack` is deliberately outside all of this: it arms the gapless
prefetch rather than taking over, and dropping one would cost a gapless
hand-off to prevent nothing audible. The decoder's own repair path (a
reconciled file that will not open, falling back to the server) re-sends
under the **same** intent it was handed — it is continuing that load, not
starting a newer one.

### Resume point

Where playback was, kept in two `profile_setting` rows — `player.last_track_id` and `player.last_position_ms`, written by `queue::persist_resume_point`. `queue::restore_state` prefers that pair at mount, falls back to the queue's current track at 0 ms when the track is gone, and gives up when the queue is empty too. `player_actions::resume_last` loads the same pair, so every surface that offers to pick playback back up — the in-app Play from idle, the tray, the taskbar buttons, the OS overlay, MPD — starts from it.

Three sites write it, and none coordinates with the others, because writing the same pair twice costs nothing:

- **`RunEvent::Exit`** in [`lib.rs`](../../src-tauri/crates/app/src/lib.rs) — every way out of the app reaches it. Until #624 the only writer was `WindowEvent::Destroyed`, which `AppHandle::exit` (the tray's Quit) never triggers and a close-to-tray X never reaches either, so the pair sat at its initial `0` and every launch fell through to the queue's current track at the start.
- **A ten-second ticker** spawned in `setup` — a crash, a kill or a power loss leaves no exit event to hook at all. It skips the write while neither the track nor the second it sits on has moved, so a paused or idle session doesn't wake the [single SQLite writer](../architecture/invariants.md#single-writer-to-sqlite) for nothing.
- **The device-loss rebuild** in [`audio/engine.rs`](../../src-tauri/crates/app/src/audio/engine.rs) — parks a user-paused library track idle with its position saved, pinned to the profile that was active when the rebuild ran (see [Output-stream lifecycle & recovery](#output-stream-lifecycle--recovery) above).

Only a real library track is worth saving: radio and the remote queue use negative ids and nothing loaded is `0`, and `restore_state` has no `track` row to find for either, so the write is skipped there.

Two details keep concurrent writers honest. Each write is **pinned to the profile that was playing** — the id is captured before the await, without waiting for the lock, and a lock that can't be taken means a switch is under way, so the write is dropped rather than landing this track's position in the profile being switched to (#485). And `persist_resume_point` sets both rows **in one transaction**, so two writers can't leave one track's id beside another's position. The ticker only remembers a point that actually landed, so a write lost to a busy database is retried on the next tick instead of being skipped as unchanged.
