# 2026-10-25 — Fedora 44, Intel Iris Xe, WebKitGTK 2.54.1

The first run of the bench, on `main` at `0da6b378` (2.0 before the beta: the five materials on the fluid layout, the Wave W mark). Release build (`tauri build --no-bundle`), GNOME on Wayland, 1920×1019 window at scale 1, a 60 Hz display (a frame is 16.7 ms). The fixture library of `gen-library.sh` (240 albums, 480 tracks), analysed; a track playing at volume 0 throughout. `data-webkitgtk-safe` is set, as on every WebKitGTK launch, so Lounge's room and Liquid's backdrop blurs are already off here (#820).

## With the GPU (`WAVEFLOW_RENDERER=gpu`)

| skin      | Home at rest | album grid, Lyrics open | track table | immersive |
| --------- | ------------ | ----------------------- | ----------- | --------- |
| studio    | 59.9 fps · 0 | 58.6 · 26               | 47.2 · 96   | 59.5 · 12 |
| fluid     | 59.9 · 0     | 59.6 · 15               | 50.0 · 81   | 59.3 · 10 |
| liquid    | 59.9 · 0     | 56.4 · 24               | 44.8 · 92   | 59.6 · 7  |
| pulse     | 59.9 · 1     | 56.8 · 30               | 44.9 · 95   | 59.1 · 10 |
| editorial | 59.9 · 0     | 59.5 · 19               | 46.8 · 68   | 59.3 · 9  |
| lounge    | 59.9 · 2     | 57.2 · 36               | 47.2 · 95   | 59.0 · 13 |

fps · frames over 20 ms. Every skin holds 60 fps at rest and in the immersive view, and scrolls the album grid at 56–60. **The track table is the weak view under every skin, Studio included**: 45–50 fps, a third of the frames over 20 ms, the web process at 70–86 % of a core. It is the table, not a material. GPU render-engine busy at rest: Studio 9 %, Fluid 19 %, Pulse 19 %, Liquid 14 %, Lounge 14 %, Editorial 2 % (no animation running).

## Without the GPU (`WAVEFLOW_RENDERER=software`)

The path #595's fallback puts a machine on after a launch that never painted, and the one a virtual machine or a machine with no usable driver is on.

| skin      | Home at rest | album grid | track table | immersive |
| --------- | ------------ | ---------- | ----------- | --------- |
| studio    | 12.2 fps     | 5.0        | 25.4        | 6.5       |
| fluid     | **1.7**      | 4.4        | 8.6         | 3.7       |
| liquid    | 11.8         | 8.0        | 17.1        | 8.9       |
| pulse     | 12.2         | 5.2        | 10.8        | 3.8       |
| editorial | **63**       | 4.5        | 17.8        | 9.9       |
| lounge    | 7.0          | 3.1        | 7.6         | 7.0       |

The web process sits at 100 % of a core everywhere. **The default skin's Home draws a frame every 620 ms.** Only Editorial's Home, where nothing animates, holds 60.

### What costs the frames (`bisect.ts`, one piece switched off at a time)

Fluid's Home, software: baseline 1.8 fps → **48 fps with every CSS `filter` off**. Alone, the canvas's blurred copy of the cover (`.fe-canvas__aura img`, `blur(96px)`) takes it to 11.6; the suggestion band's (`blur(72px)`) to 3.1; the wave progress, which repaints the bar 4 times a second, to 4.1. The grain, the pools, the mood waves and the shadows change nothing measurable.

Studio's Home, software: baseline 12.2 fps → **60 with every animation off, or with every filter off**. The classic Home's two `blur-3xl` orbs repaint under an infinite animation (the "ready to listen" dot's `animate-pulse`); either half of the pair alone is cheap.

The rule both show: **a CSS blur in a region that repaints every frame costs the whole frame without a GPU.** A blur computed once (as Lounge's room now is, in a canvas) or a region that never repaints costs nothing.

## Memory (`--soak 5`, GPU, 5 minutes per skin looping the views, the immersive view and a skin switch)

| skin      | web PSS start → end (peak), MiB | UI PSS    | GPU resident start → end (peak), MiB | slope web / UI / GPU, MiB/min |
| --------- | ------------------------------- | --------- | ------------------------------------ | ----------------------------- |
| studio    | 454 → 501 (695)                 | 152 → 163 | 929 → 686 (1248)                     | 3.7 / 1.7 / 9.1               |
| fluid     | 562 → 541 (731)                 | 163 → 166 | **2256** → 699 (**2593**)            | −3.1 / 0.4 / −3.8             |
| liquid    | 580 → 546 (756)                 | 166 → 169 | 886 → 851 (1247)                     | 8.7 / 0.5 / 4.6               |
| pulse     | 591 → 581 (763)                 | 169 → 171 | 1072 → 778 (1374)                    | 2.1 / 0.3 / 14.0              |
| editorial | 605 → 609 (781)                 | 171 → 173 | 867 → 778 (1213)                     | −0.8 / 0.5 / 22.3             |
| lounge    | 632 → 611 (819)                 | 173 → 175 | 1160 → 669 (1400)                    | 8.0 / 0.3 / −12.2             |

No runaway like #817's within five minutes per skin; the app was alive and painting at the end of the 38-minute run. Two things to follow: the web process's baseline climbs across the run (454 MiB at Studio's start, 632 at Lounge's, about 5 MiB a minute overall) — caches or a slow leak, which only a longer soak on one skin can tell apart; and the GPU memory resident for the app jumped to **2.6 GB** when the run switched to Fluid, then fell back to 0.7 GB. Five minutes is too short to call a slope: the per-skin slopes are within the noise of the views coming and going.

## What this run cannot see

The main thread's cadence, not the compositor's; one machine, one driver (Intel, Mesa), no NVIDIA, no DMA-BUF variations; a 60 Hz display; the desktop lyrics window and the mini-player were not opened.

## Next runs

A 30-minute soak on Fluid alone (the default) to settle the web process's climb and the GPU spike; the track table profiled on its own (it is slow with the GPU, under every skin); the same bench after each fix, to compare against this file.
