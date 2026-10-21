# Fluid Editorial — the design language

WaveFlow's visual direction since 2.0, written down so the other surfaces — [waveflow-server](https://github.com/InstaZDLL/waveflow-server)'s web app, the Android app, the landing site — can speak it without reading the desktop app's stylesheet. The desktop implementation is the `fluid` skin ([`src/styles/skins/fluid.css`](../../src/styles/skins/fluid.css), [`src/lib/skins.ts`](../../src/lib/skins.ts)); this page is the intent and the values, not the CSS.

The one-line brief: **cinematic, dark, music first — the cover lights the page, the type carries the editorial voice, the wave is the mark.**

## Principles

1. **The music is the picture.** The playing or last-played cover is the largest thing on a page, and its colour, blurred and darkened, is the page's light. Nothing else is decorated: no gradients of our own, no illustrations, no coloured icon boxes.
2. **Depth by tone, not by line.** Surfaces are steps of one dark, a hairline at most where two surfaces meet, no borders around cards, no drop shadows except under a cover. A raised element is a lighter tone; a pressed one, darker.
3. **One accent, spent carefully.** The accent (the theme's, the user's, or the cover's) appears on the primary action, the open item's edge, the sung word, the focus ring — and nowhere decorative. Everything else is ink on ground.
4. **Editorial typography.** A geometric sans at a settled weight for everything; a serif italic for one line per page at most (the greeting, the suggestion's kind), never for running text. Large sizes are tight and short; small sizes are tracked out and uppercase for labels only.
5. **Rhythm over rules.** Blocks are told apart by spacing, not separators. Groups breathe (1.75 rem between sections in a rail, 3.5 rem between sections on a page); items inside a group sit close.
6. **Calm motion.** One ease-out for everything, no springs, no bounces; a reveal rises a few pixels and fades; the hover is a tone change and a one-pixel lift at most. Reduced-motion turns all of it off, and the design still reads.
7. **Recompose, don't restyle.** Where the language calls for a different shape (a canvas instead of a banner and stat cards, tiles instead of rows, one tabbed panel instead of three), build that shape. A restyled legacy layout still reads as legacy.

## Tokens

### Colour

| Token                | Value                                                                                   | Role                                                                                                                 |
| -------------------- | --------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `bg`                 | `#0b0b0e`                                                                               | The page. Never pure black: it has to hold the cover's light.                                                        |
| `surface`            | `#121216`                                                                               | Rails, panels, cards at rest.                                                                                        |
| `surface-2`          | `#17171d`                                                                               | Popovers, a card on hover, the second step.                                                                          |
| `ink`                | `#f3f1ec`                                                                               | Titles, the open item, anything that must be read first. Warm, not pure white.                                       |
| `ink-2`              | `#b5b2aa`                                                                               | Body text, names, the second line.                                                                                   |
| `ink-3`              | `#8e8b83`                                                                               | Labels, counts, timestamps, placeholders, icons at rest.                                                             |
| `hairline`           | `white / 7 %`                                                                           | Dividers, the edge of a rail, the ring of a field at rest.                                                           |
| `hairline-strong`    | `white / 12 %`                                                                          | A chip's ring, an input's ring.                                                                                      |
| `accent`             | the theme's 400 stop (the default theme's: `oklch(0.765 0.177 163)`, emerald `#34d399`) | The primary action's fill, the open item's edge, the focus ring, the sung word.                                      |
| `accent-ink`         | `#0b0b0e`                                                                               | Text on an accent fill. The accent is light enough to take dark ink.                                                 |
| `glow` / `glow-deep` | sampled from the cover; defaults `#1f5a48` / `#0f2b22`                                  | The cover's light: two radial pools behind the page, the player bar's top gradient, the shadow under a hovered tile. |

Tones over surfaces (hover, pressed, a tile's ground) are white at 4 %, 6 %, 8 % — never a new colour. Light theme: there is none; the direction is dark-only, and a light variant is an open question, not a planned one.

### Typography

| Role         | Face                                  | Size                                                                                                                                       | Weight  | Tracking                    | Notes                                                           |
| ------------ | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | ------- | --------------------------- | --------------------------------------------------------------- |
| Display      | DM Sans (variable, optical sizing on) | `clamp(2rem, 2.6vw, 3.25rem)` on a canvas; `clamp(2.25rem, 3vw, 3.5rem)` on a page header; `clamp(3rem, 5vw, 5.25rem)` on an artist banner | 560–600 | −0.035 to −0.04 em          | Line height 0.95–0.98, `text-wrap: balance`, two lines at most. |
| Title        | DM Sans                               | 1.5 rem (section), 1.625 rem (band)                                                                                                        | 540     | −0.022 em                   |                                                                 |
| Editorial    | Playfair Display italic               | 1.375–2 rem                                                                                                                                | 400     | −0.01 em                    | One line per page: the greeting, the suggestion's kind.         |
| Body         | DM Sans                               | 0.875–0.9375 rem                                                                                                                           | 480–500 | 0                           | `ink-2`.                                                        |
| Label        | DM Sans                               | 0.6875–0.75 rem                                                                                                                            | 540     | +0.1 to +0.12 em, uppercase | `ink-3`. Kickers, section eyebrows, table headers.              |
| Mono figures | DM Sans with `tnum`                   | inherits                                                                                                                                   |         |                             | Durations, counts, dates — tabular, never a monospace face.     |

Fallbacks: `system-ui, -apple-system, "Segoe UI", sans-serif` and `Georgia, serif`. On the web and Android, ship the two faces (DM Sans variable, Playfair Display italic) rather than relying on the system; the direction depends on DM Sans's low-contrast geometry.

### Shape and space

| Token                | Value                                                                              |
| -------------------- | ---------------------------------------------------------------------------------- |
| Card radius          | 0.875 rem (14 px); a hero cover 1.25 rem; a small tile 0.625 rem                   |
| Button / pill radius | full                                                                               |
| Input radius         | 0.625 rem; the search field, full                                                  |
| Hairline             | 1 px, as an inset shadow rather than a border, so it never changes the box         |
| Rail width           | 16 rem full, 5 rem collapsed                                                       |
| Page gutters         | 2 / 3 / 3.5 rem at 1024 / 1280 / 1536 px; content column up to 1680 px             |
| Section gap          | 3.5 rem on a page, 1.75 rem in a rail                                              |
| Density              | rows 2.25–2.5 rem high; the top bar 3.5 rem; the player bar 4.25 rem plus the wave |

### Motion

| Token          | Value                                                                                               |
| -------------- | --------------------------------------------------------------------------------------------------- |
| Ease           | `cubic-bezier(0.22, 1, 0.36, 1)` — one ease-out, everywhere                                         |
| Duration       | 180 ms for colour and tone, 320 ms for position and shape, 900 ms for a light fading in             |
| Reveal         | `fe-rise`: 10 px up and fade over 640 ms, staggered 70 ms per block on a page                       |
| Hover          | tone up one step; a tile lifts 3 px with the glow as its shadow; a CTA lifts 1 px and brightens 6 % |
| Reduced motion | no stagger, no drift, no lift, no blur transitions; opacity only                                    |

## The mark

A short flowing line — one sine-like path, `M0.5 5 C 5 0.5, 9 0.5, 13.5 5 S 22 9.5, 26.5 5 S 35 0.5, 39.5 5` in a 40 × 10 box — drawn in the accent at 1.6 px. It is the underline of a section title, the marker of the open tab, the progress of the player (the played part of the line becomes a wave, the rest stays straight), the shape of the mood spectrum's bands. It is the one ornament the language allows, and it is always functional: it marks, it measures, it never fills.

## Components

- **Canvas** (the Home's head): a 12-column grid; text on 7, the cover on 5 from 1024 px; text · queue · cover in three columns from 1700 px. Greeting in the editorial italic with the name after it in sans; a kicker; the display title; artist · album · year; a pill CTA with the time left in tabular figures; a text link beside it. The cover's blurred copy sits behind the text at 20 % opacity, darkened to 70 % so a pale sleeve never pales the ground.
- **Band** (the suggestion of the day): a surface-toned strip, the cover left on its own light, label + editorial kind + title + artist + a quiet reason line, actions right (a text link, a pill CTA).
- **Spectrum** (Mood Radio): one continuous band whose axis is tempo; segments carry the mark as a wave at their own frequency; the chosen one opens with a description, a count and a CTA.
- **Tiles**: square cover, radius 14 px, the title and artist under it in ink and ink-2; a lead tile twice the size with a kicker. Tiles rise on hover with the glow underneath.
- **Rail** (sidebar): wordmark; profile as a small panel (avatar, name, the library's size); the pinned navigation as tiles two to a row (icon on a tone, name under it, the open one ringed in the accent); section labels; rows for the library's facets; liked / recently played as tiles; playlists as rows with a 40 px cover. The open row carries a 2 px accent edge with a faint glow.
- **Control bar** (top): transparent; three navigation squares in one cluster; the search 26 rem wide and 2.25 rem high on the bar's own middle — a tone, a hairline, and on focus the accent ring plus a soft halo of the cover's light; theme switch and profile as one cluster on a tone.
- **Panel** (right): one shell, tabs as text with the mark under the open one; Details = the cover edge to edge with the identity over its foot on a scrim, the album it plays from, two ghost pills, a glimpse of the lyrics on a quiet card; Lyrics = the sung line sharp and large, the others fading and blurring with their distance; Queue = rows.
- **Page header**: kicker, display title, one meta line (`ink-2`, counts in `ink-3`), the primary action as a pill in the accent, secondaries as ghost pills (white 6 %), icon actions as bare squares. An artist with a photo gets a full-width banner (22–25 rem tall) with the name at its foot over a scrim; no card around any of it.
- **Cards** (extensions, folders): surface tone, 14 px radius, no border, a tone up on hover; a title, a muted path or subtitle, chips for facts, actions in the top-right corner that appear on hover.
- **Tables**: header on the page's tone with a hairline under it, rows parted by white 5 %, the playing row on a tone with the accent's edge; no outer frame.
- **Menus and popovers**: `surface-2`, 14 px radius, no border, an inset top highlight and a deep soft shadow; rows with their own 8 px radius that take a tone on hover.
- **Player bar**: one surface, a top gradient from the glow, the wave as its top edge; the transport centred on the window.

## Two materials on one structure

The second material has its own page: [Liquid — Aurora Glass](./liquid-aurora-glass.md). The table below is the short form of the difference.

The structure above — the canvas, the band, the spectrum, the rail, the control bar, the panel, the heroes — is the 2.0 interface, and it is shared. A skin on it chooses a **material**: what the surfaces are made of, how the light behaves. Fluid's material is the one in the tables: tone over a near-black, no blur, hairlines. **Liquid** is the second: Apple's glass, translated for this structure.

| Where Fluid says…         | Liquid says…                                                                                                                                                                      |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| a surface is a tone       | a surface is a translucent tint that blurs and saturates what is behind it (`28 px` for the rail, the bar and the panel; `20 px` for cards; `44 px` and near-opaque for popovers) |
| depth by tone             | depth by material weight: heavy (rail, bar, panel), light (cards, clusters, tiles are tones on them — two light sheets never stack), dense (menus, dialogs)                       |
| a hairline at an edge     | the light a top edge catches: an inset rim, white at 10–18 % on dark, 70–95 % on a pale ground, and the eight-layer inset recipe that makes a translucent rectangle read as glass |
| the cover lights the page | the cover lights the page **and** is what the glass refracts: three slow pools of its colour behind everything (a 72 s drift, off under reduced motion), pastel on a pale theme   |
| dark only                 | light or dark, from the theme: black tones and white edges on `#e9e9ee`, the accent's 600 stop with white ink                                                                     |
| the serif italic line     | the same line in the sans at a light weight: a serif over glass reads as a sticker                                                                                                |
| rails and bars are flush  | the rail, the bar and the panel float: inset `0.75–1 rem` from the window, `1.5–1.75 rem` corners                                                                                 |
| a hover is a tone         | a hover is a tone, and a press is instant — `scale(0.94–0.97)` on pointer-down, before anything else                                                                              |

What does not change between the two: the type, the mark, the accent rule, the rhythm, the ease. A third material (a paper, a neon) would be written the same way — the tokens redeclared, the few surface rules added, nothing moved.

## Carrying it to another surface

- **Web (waveflow-server)**: the colour and shape tokens map one to one onto CSS variables; the two faces ship as webfonts; the mark is one inline SVG path reused as a mask. Keep the dark-only rule; the server's light theme, if it has one, is a different direction and should not borrow these inks.
- **Android**: tokens as a Compose theme — the three inks and three surfaces, the accent from the album's palette through the same "400 stop" rule, DM Sans variable and Playfair italic as font families, 14 dp card radius, full-radius pills, the one ease. The canvas and the band translate as-is; the rail becomes a bottom bar whose active item carries the accent's edge.
- **Landing site**: the same page rhythm (display title, one quiet line, a pill), a cover-lit hero, the mark under section titles. No stock gradients, no device frames with coloured glows.
- **What not to carry**: Tailwind class names, the `data-*` landmarks (they are the desktop app's contract between its views and its skins), and the Studio skin's values — Studio is the desktop's safe-mode baseline, not a sibling direction.

## Where it came from

Prototyped as a dev-only page (#905), reviewed by the user and an external design review (8.5/10 on the first pass, then the second pass with a wider canvas, the cover's light behind the text and the edge progress), integrated as the sixth skin, then extended slice by slice to every page and recomposed where restyling fell short (the panel, the rail, the control bar, the extensions page). Made the default in 2.0. The history, with the reasons for each shape, is in the commits of the `feat/fluid-editorial-skin` branch and in [`ui.md`](../features/ui.md#skins).
