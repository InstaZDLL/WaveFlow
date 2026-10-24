# WebKitGTK bench

Measures what the interface costs under WebKitGTK — frame cadence, memory, GPU — on the real release binary, skin by skin, with the GPU renderer and with the software one. Nothing here ships with the app or runs in CI; it is a tool run by hand on a Linux machine with a desktop session, before a release or after a change to a skin, an animation or a blur.

It exists because WebKitGTK is where WaveFlow's visual effects have gone wrong before: memory climbing by gigabytes under the glass skins (#817, fixed in part by #820, "a longer soak is still needed"), the immersive view flashing on 2.54 (#791), a white window on first launch (#595), and every skin but Studio reported heavy on one machine.

## What it does

1. `gen-library.sh` writes a fixture library — 240 albums, 480 tracks, 60 artists — with folder covers (pale, dark and saturated, the three cases the cover-lit backgrounds have to survive), artist photos, synced `.lrc` lyrics, and a beat at a known tempo per album so Mood Radio has moods to show. Nobody's music is needed.
2. `run-app.sh` launches the built binary on the machine's desktop from an SSH shell, with a throwaway `XDG_DATA_HOME` (no real profile is touched, logs included), WebKit's remote inspector on a local port (`WEBKIT_INSPECTOR_HTTP_SERVER`), and `WAVEFLOW_RENDERER=gpu|software`.
3. `bench.ts` connects to the inspector, injects `page.js` (helpers that read the views' `data-*` landmarks and the React fibres, never a label), sets the fixture library up on first run, and for each skin plays a track at volume 0 and runs the scenarios:
   - **home** — the Home at rest for 8 s, the record playing: every always-running animation (the wave, the vinyl, the horizon, the aurora);
   - **albums** — the album grid scrolled for 6 s with the Lyrics panel open, the protocol #928 used for Liquid;
   - **tracks** — the track table scrolled for 6 s;
   - **immersive** — the immersive view for 8 s, lyrics scrolling.

   For each one it records the `requestAnimationFrame` cadence (p50 / p95 / p99, frames over 20 and 50 ms) and what the processes spent meanwhile, read from `/proc`: CPU of the UI process and of `WebKitWebProcess`, their PSS, the GPU memory the DRM driver reports resident for them (`drm-resident-*` in `fdinfo`, where the compositing surfaces of #820 live) and the GPU render-engine time. It saves a screenshot of each view and flags one that is blank (one colour).

4. `--soak <minutes>` loops the four views and a skin switch for that long per skin, sampling memory every second, and reports the slope (MiB per minute) after the first minute — the leak test #820 could not run.
5. `bisect.ts` answers "which piece costs the frames": it measures one view at rest, then again with one piece switched off by an injected stylesheet (the grain, the pools, the blurred covers, the mood waves, the progress, every animation, every filter, every shadow), one at a time.

Reports of past runs, kept to compare against, are in [`reports/`](reports/).

## Running it

On the Linux machine, from the repository:

```bash
bash scripts/bench/webkitgtk/gen-library.sh ~/.wf-bench/library
bun run tauri build --no-bundle
bun scripts/bench/webkitgtk/bench.ts --renderer gpu --out ~/.wf-bench/runs/gpu
bun scripts/bench/webkitgtk/bench.ts --renderer software --out ~/.wf-bench/runs/software
bun scripts/bench/webkitgtk/bench.ts --renderer gpu --soak 5 --out ~/.wf-bench/runs/soak
bun scripts/bench/webkitgtk/bisect.ts --renderer software --skin fluid --view Home
```

Each run writes `report.json`, `report.md` and the screenshots in its `--out` directory. The window opens on the machine's screen and must stay unobscured (WebKitGTK throttles a hidden window); the track plays at volume 0.

## Limits

The cadence is the main thread's, not the compositor's: a frame the compositor drops after the page produced it is not seen. Timings vary from run to run by a few frames; compare runs on the same machine, the same day, rather than against fixed thresholds. The software renderer measures the worst case (no GPU, a virtual machine); GPU-specific defects (DMA-BUF, proprietary drivers) only show on the hardware that has them.
