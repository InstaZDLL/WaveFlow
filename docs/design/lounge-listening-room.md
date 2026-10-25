# Lounge — Listening Room

The fifth material on the 2.0 interface, beside [Fluid Editorial](./fluid-editorial.md), [Liquid — Aurora Glass](./liquid-aurora-glass.md), [Pulse — Neon Deck](./pulse-neon-deck.md) and [Editorial — Broadsheet](./editorial-broadsheet.md). All five sit on the same structure — the canvas, the rail, the control rail, the tabbed panel, the page heroes — and this page writes down what Lounge makes of it, so the web app, the Android app and the landing site can carry it. The desktop implementation is the `lounge` skin ([`src/styles/skins/lounge.css`](../../src/styles/skins/lounge.css) over [`fluid.css`](../../src/styles/skins/fluid.css), [`src/lib/skins.ts`](../../src/lib/skins.ts)); this page is the intent and the values.

The one-line brief: **a listening room lit by the record — the cover is the light of the whole window, the chrome is smoked wine glass standing in it, and the music fills the room as it plays.**

Where Fluid stages the music, Liquid diffuses it, Pulse wires it and Editorial prints it, Lounge lets it light the room you sit in. It is the 1.x "Listening Room" carried onto the 2.0 structure — the cover blurred behind everything, the burgundy glass, the floating rounded sheets, the slow motion — with two changes of principle: the accent is the theme's, not a rosé of its own, and the room's blur is computed once per record, not once per frame.

## Principles

1. **The record is the light.** The playing cover, blurred to a colour field, fills the whole window behind the interface — every page, not only the Home. It is darkened and veiled in wine so a white sleeve tints the room without lighting it white, and it cross-fades when the record changes. With no record, the room keeps a static wine light.
2. **Smoked wine glass, and nothing blurs.** The rail, the panel and the player bar are sheets of dark burgundy glass floating in the room, a warm light along their top and a rosé hairline for their edge. None of them blurs what is behind it: what is behind it is already a blurred field.
3. **One accent, the theme's.** The accent (the theme's 400 stop, or the custom colour, or the cover's through the accent modes) is spent on the primary action, the open item, the played part of the progress, the needle, focus. The rosé is a tint of the material only — the glass, its edges, the warm inks — never a state or an action. The cover accent mode makes the whole room the record's.
4. **A quiet voice.** The structure's DM Sans set lighter (display at 440, titles at 460); the greeting keeps the layout's serif italic. Inks warm, rosé-white to dusty rose.
5. **Slow.** One ease-out, longer than Fluid's. The needle swings onto a station, the light glides along the horizon, the room cross-fades between records. Nothing loops; reduced motion makes every one of them a cut.

## Tokens

| Token      | Value                                                                                                                                                      | Role                                                                     |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| `bg`       | `#14070c`                                                                                                                                                  | Under the room; what high contrast leaves.                               |
| `surface`  | `#1f0b13` · `#29101a` · `#190910`                                                                                                                          | The solid glass of the fallbacks; the second step.                       |
| `glass`    | `rgb(32 9 18 / 0.64)` · card `rgb(36 11 21 / 0.5)` · dense `rgb(28 8 16 / 0.96)`                                                                           | Rail, panel, bar · cards on the page · menus, dialogs, the table header. |
| `lamp`     | a radial of `rgb(255 176 196 / 0.07)` from the top centre                                                                                                  | The warm light a sheet's top catches.                                    |
| `rim`      | `inset 0 1px 0` rosé-white 11 % + `inset 0 0 0 1px` rosé 12 %                                                                                              | A sheet's edge.                                                          |
| `drop`     | `0 8px 18px -10px rgb(6 0 3 / 0.8)`                                                                                                                        | Under a floating sheet. Short on purpose — see the cost below.           |
| `ink`      | `#fbf1f4` · `#dccbd3` · `#bda5b2`                                                                                                                          | Title · body · label.                                                    |
| `hairline` | rosé `rgb(255 156 188)` at 12 % · 20 %                                                                                                                     | Dividers, rings.                                                         |
| `tone`     | rosé-white `rgb(255 214 225)` at 4.5 / 7 / 9.5 / 12 %                                                                                                      | Hover, the open row, a module on the glass.                              |
| `accent`   | the theme's 400 stop, dark ink                                                                                                                             | Primary action, open item, played part, needle, focus.                   |
| `room`     | the cover drawn into a canvas at most 192 px wide (`PrecomputedBlur`), `blur(136px) saturate(2.6) brightness(0.62)` measured against the window, stretched | The whole window's light.                                                |
| `veil`     | wine `rgb(20 5 11)` at 40 %, a vignette to 50 % at the edges, a foot gradient to 45 %                                                                      | Over the room: keeps the ground dark under any sleeve.                   |
| `ease`     | `cubic-bezier(0.22, 1, 0.36, 1)`, 450 ms                                                                                                                   | Every transition.                                                        |

## Components

- **The room** (`fluid.atmosphere: "room"`, [`SkinAmbientBackdrop`](../../src/components/layout/SkinAmbientBackdrop.tsx)): the playing cover, blurred once per record into a small canvas (`PrecomputedBlur`, which blurs the pixels itself, so it works on WebKit too), stretched to the window behind everything, the veil over it. It replaces the layout's two pools and the canvas's blurred copy of the cover. Read from the preset, never from the skin id.
- **The horizon** (`fluid.progress: "horizon"`, `ProgressBar`'s `horizon` variant, structure in `fluid.css`): the progress as a straight line along the player bar's top edge, the times at its ends; the played part is the accent, lit, and under it the accent's light spills down into the bar — the bar fills with light as the record plays. Moved by transforms from one 0–1 factor; the 4 Hz ticks glide over 250 ms.
- **Mood Radio — the tuner** (`fluid.moodRadio: "tuner"`, [`MoodTuner`](../../src/components/views/home/MoodTuner.tsx), structure in `fluid.css`): a receiver's tuning dial — forty graduations and a major one over each station, the five moods as stations slowest to fastest, a needle swung onto the chosen one with a warm backlight that travels with it — and a readout beside it: the tempo window, the mood's name and line, its wave in the accent, the start button and the count. Not Fluid's band, not Liquid's capsules: one dial, one needle.
- **Rail, panel, player bar**: smoked glass sheets inset 0.75 rem from the window, 1.5 rem corners, the lamp and the rim; modules on the rail a warm tone, the open one ringed in the accent with its light rising from the foot.
- **Control rail**: the search a smoked pill with the rim, the accent's ring and a soft halo on focus; the two clusters on the same glass.
- **Pages**: cards (extensions, folders, settings groups, the suggestion band) as the card glass with the lamp; covers on a rosé hairline and a warm drop; an artist's photo dissolving into the room at its foot and sides rather than into a ground colour the room is not; the track table's header the dense glass.
- **Menus, popovers, dialogs**: the dense glass, the rim, 1–1.25 rem corners.
- **Immersive**: shared with Fluid, unchanged here.

## The cost, measured

The 1.x Lounge was the heaviest of the skins: a 140 px CSS blur on a full-window element scaled 1.6×, and 49–55 surfaces carrying a `backdrop-filter`. Measured the way Liquid was (#928): the album grid of 1 004 albums scrolled for 4 s, the Lyrics panel open, a track playing, three passes, rAF intervals on a 144 Hz display, 1600 × 900.

| Configuration                                       | p50  | p95          | frames over 20 ms (of ~500) |
| --------------------------------------------------- | ---- | ------------ | --------------------------- |
| Fluid                                               | 7 ms | 13.9 ms      | 1–18 (10 typical)           |
| Lounge 1.x (main before this change)                | 7 ms | 20.8 ms      | 32–38                       |
| Lounge 2.0, room as the 1.x CSS blur                | 7 ms | 13.9 ms      | 18–20                       |
| Lounge 2.0, 60 px drops under the floating sheets   | 7 ms | 13.9–20.8 ms | 14–28                       |
| **Lounge 2.0 as shipped** (canvas room, short drop) | 7 ms | 13.9 ms      | 5–7                         |

Two things carried the cost, and both are gone: the per-frame blur of the room (now computed once per record) and the long, soft drop shadows under the three floating sheets (now short). The glass's translucency, the horizon's glide and the tuner cost nothing measurable. Not measured on modest hardware.

## Fallbacks

Reduced transparency: the glass turns solid wine and the room goes, the static wine light stands in. The WebKitGTK safeguard (#820): no room at all (the component does not mount), no backdrop layer anywhere, solid glass. Reduced effects (`data-effects="reduced"`): solid glass on the flat wine ground — no room, no wine light, no lamp on the sheets, no drop under them, no glow on the tuner. High contrast: no room, no pools, solid glass, brighter inks and hairlines. Reduced motion: the needle, the horizon and the room's cross-fade become cuts.

## What not to do

No blur at runtime: the room is blurred once, and no sheet blurs what is behind it. No rosé on a state or an action: it is the material's. No long, soft shadow under a sheet that sits beside a scrolling page. No light of the skin's own beyond the lamp: the room's light is the record's. Nothing loops.

## Carrying it to another surface

The tokens map to CSS variables as Fluid's do: the three glass densities, the lamp, the rim, the short drop, the warm inks, the rosé hairline and tones. The room is the one piece that needs care: compute the blur once per record (a small bitmap, blurred and stretched — on Android a `RenderEffect` blur on a downscaled bitmap, kept, not re-run per frame), then a veil over it. The tuner is a row of five stations under a strip of graduations with a needle that animates its offset; the horizon a line, a scaled fill and a scaled gradient under it. The mark, the type scale and the rhythm come from [Fluid Editorial](./fluid-editorial.md) unchanged.
