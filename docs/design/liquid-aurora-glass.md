# Liquid — Aurora Glass

The second direction of WaveFlow 2.0, beside [Fluid Editorial](./fluid-editorial.md). Both sit on the same interface — the canvas, the rail, the control rail, the tabbed panel, the page heroes — and both are written down here so the web app, the Android app and the landing site can carry them. The desktop implementation is the `liquid` skin ([`src/styles/skins/liquid.css`](../../src/styles/skins/liquid.css) over [`fluid.css`](../../src/styles/skins/fluid.css), [`src/lib/skins.ts`](../../src/lib/skins.ts)); this page is the intent and the values.

The one-line brief: **music as light and material — a luminous, airy glass, tinted by what is playing, with a light that answers the hand.**

Where Fluid is the music staged (cinematic, dark, artwork-led, editorial), Aurora Glass is the music diffused: bright, soft, polished, tactile. The two share the type, the mark, the one-accent rule and the rhythm; they differ in what a surface is made of and what light does.

## Principles

1. **Musical glass, not system glass.** Every surface is a sheet of frosted glass, but the light behind it is the record's: the cover's colour, mixed into three pastel pools that are always there, so the page is never grey and never white. A neutral cover lights the page lavender and ice; a warm one, blush and amber.
2. **Material weight is the hierarchy.** Four glass levels, each with its own tint, blur, rim and shadow, and nothing sits on a sheet of its own weight: a tile on the rail is a tone, a card on the page is a sheet, a menu is the dense one. If two surfaces look alike, one of them is wrong.
3. **Separation by light, not by line.** A surface is bounded by the light its top edge catches (a white rim) and by a soft, coloured shadow under its foot, never by a dark border. Dividers inside a sheet are the faintest dark hairline, and only where rows need parting.
4. **Pastel, restrained.** Five tints — lavender, ice, aqua, mint, blush — for atmosphere and for the moods, never for text or actions. The one accent stays the theme's: the primary action, the playing row's edge, the focus light.
5. **Light answers the hand.** A hover catches light (a sheen on the top of the surface, a brighter rim); a press is instant and small; focus charges the surface with the accent's light; the playing item carries a slow sheen. Nothing bounces.
6. **Luminous, not filled.** The active item is brighter than its neighbours — a whiter sheet, a soft inner glow — rather than painted in a colour.
7. **Bright, never washed.** Ink stays near-black and cool; a sheet is translucent enough to show the aurora, opaque enough to read on. Where the system asks for less transparency, every sheet turns solid and the direction still reads.

## Tokens

### Ground and tints

| Token      | Light                                     | Dark (dusk)                                 | Role                                                              |
| ---------- | ----------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------- |
| `ground`   | `#eef0f6`                                 | `#15161d`                                   | The page, cool and slightly blue; never pure.                     |
| `lavender` | `#cfc3fa` / deep `#7c6ad6`                | tint mixed 40 % into the ground / `#cfc3fa` | Sleep; the first pool.                                            |
| `ice`      | `#c4dcfb` / deep `#5f8fd6`                | same rule                                   | Focus; the second pool.                                           |
| `aqua`     | `#b2e6e6` / deep `#3fa9ad`                | same rule                                   | Workout; the light progress's start.                              |
| `mint`     | `#c0ecd5` / deep `#3fae7a`                | same rule                                   | Chill; the third pool.                                            |
| `blush`    | `#f8c9d8` / deep `#d4668c`                | same rule                                   | Party.                                                            |
| `glow`     | sampled from the cover; default `#9fb8ee` | default `#3b3f73`                           | Mixed into each pool, the soft shadows, the focus light's warmth. |
| `accent`   | the theme's 600 stop, white ink           | the theme's 400 stop, dark ink              | Primary action, playing edge, focus light.                        |
| `ink`      | `#1d1e2b` · `#5a5c70` · `#8b8da3`         | `#f4f4f8` · `#c0c2d2` · `#8c8ea2`           | Title · body · label.                                             |

The atmosphere: three pools, each a tint mixed with the glow (60 / 40), placed top-left, top-right and bottom-left, drifting over 36 s in alternation. The dark theme uses the same pools at a fifth of the strength.

### Glass levels

| Level      | Tint (light) | Blur  | Rim (inset, white) | Shadow              | Used by                                                          |
| ---------- | ------------ | ----- | ------------------ | ------------------- | ---------------------------------------------------------------- |
| `subtle`   | white 38 %   | 12 px | 55 %               | none                | control-rail clusters, nav modules at rest, chips, table header  |
| `panel`    | white 56 %   | 24 px | 80 % + top light   | `glow-soft`         | the rail, the player plate, the side panel, cards, mood capsules |
| `floating` | white 72 %   | 32 px | 90 % + top light   | `glow-soft`, lifted | the search field, the cover object, the suggestion band          |
| `overlay`  | white 88 %   | 44 px | 95 % + top light   | deep soft shadow    | menus, popovers, dialogs                                         |

Dark: the same levels as indigo-grey tints (`rgb(32 34 46)` at 62 % for a panel, 92 % for an overlay) with rims at 8–20 % white. Radii: sheets 1.25 rem, floating objects and the rail 1.5 rem, the player plate 1.75 rem, pills full. The rail, the player and the panel float, inset 0.75–1 rem from the window.

### Light

| Token            | Value                                                                                                    | Where                                          |
| ---------------- | -------------------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| `top light`      | `inset 0 1px 0 white 90 %` (dark: 12 %)                                                                  | every panel-level sheet and above              |
| `glow-soft`      | `0 24px 60px -28px` of the glow at 55 %                                                                  | the shadow under a sheet: coloured, never grey |
| `glow-focus`     | `0 0 0 4px` accent 18 % + `0 10px 32px -10px` accent 40 %, charged over 480 ms                           | focus, the search on focus                     |
| `glow-accent`    | `0 16px 36px -14px` accent 60 %                                                                          | under the primary action                       |
| `sheen`          | a top gradient, white 35 % to nothing by 45 %, on hover; animated across the playing row over 6 s        | hover, the playing item                        |
| `light progress` | the player's progress as a luminous line: a white track, an aqua→lavender fill with a halo, a bright dot | the player plate                               |

### States

| State    | Treatment                                                                           |
| -------- | ----------------------------------------------------------------------------------- |
| idle     | the level's tint and rim                                                            |
| hover    | sheen on top, rim one step brighter, 1 px lift on sheets                            |
| active   | `scale(0.97)` on pointer-down, 100 ms                                               |
| selected | a whiter sheet (+16 %), an inner glow of the accent at 10 %, the icon in the accent |
| focused  | `glow-focus`, charged                                                               |
| playing  | the accent's edge, the slow sheen                                                   |

## Components

- **Rail**: a panel-level sheet, floating, with the aurora showing through. The pinned navigation as glass modules two to a row (subtle level, white rim), the open one luminous; the library's facets as rows; liked / recently played as modules; playlists as rows with a 40 px cover.
- **Control rail**: transparent; the three navigation squares on one subtle capsule; the search a floating sheet with an opalescent interior (a faint lavender → ice → mint sweep) that charges with the accent's light on focus; theme switch and profile on one subtle capsule.
- **Canvas** (Home): the same grid as Fluid, but the cover is a floating glass object — a thick white rim, the sheen across its top, the glow under it — and the text sits on the atmosphere alone, no blurred copy of the cover behind it.
- **Mood Radio — the capsules**: five sheets, one per mood, each tinted with its pastel and carrying a slow inner light wave; tempo on the right, the count under the name; the chosen one brighter, with its subtitle and the start pill. Not Fluid's continuous spectrum: the moods are objects of light, not segments of a band.
- **Player plate**: a panel-level sheet floating at the foot, the transport on it, the progress as the light line; audio facts under it in the label voice.
- **Side panel**: the same sheet beside the page; Details with the cover edge to edge under a stronger scrim, so the identity reads on a pale cover too.
- **Cards and tables**: cards are panel sheets that catch light on hover; the track table's header a subtle sheet, rows parted by the faint hairline, the playing row on the accent's edge with the sheen.
- **Menus, popovers, dialogs**: the overlay level, 1.25 rem corners, the deep shadow.
- **Immersive**: shared with Fluid (always dark, the cover's blur), unchanged here.

## What not to do

No verre everywhere (two light sheets never stack); no dark borders; no hard grey shadows; no saturated tint on text; no gradient on a button; no bounce; no sheet so clear the text sits on the aurora. A pale theme that reads as "white with blur" has lost the tints — check the pools first.

## Carrying it to another surface

The tokens map to CSS variables as Fluid's do; the four levels become four classes or four Compose surfaces; the pools are three radial gradients of the cover's palette mixed with the tints; the light progress is one gradient and one halo. The mark, the type and the rhythm come from [Fluid Editorial](./fluid-editorial.md#the-mark) unchanged.
