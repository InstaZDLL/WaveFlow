# Studio — Refined

The one skin on the classic layout, beside the five materials on the 2.0 interface ([Fluid Editorial](./fluid-editorial.md), [Liquid — Aurora Glass](./liquid-aurora-glass.md), [Pulse — Neon Deck](./pulse-neon-deck.md), [Editorial — Broadsheet](./editorial-broadsheet.md), [Lounge — Listening Room](./lounge-listening-room.md)). Studio is no longer the default; it is the skin safe mode and the crash marker fall back to (`RECOVERY_SKIN_ID`), and the one to pick on a machine where the others' effects cost too much — a virtual machine, a driver that draws on the CPU, some Linux set-ups under WebKitGTK. This page writes down what it is now, so it stays that way. The desktop implementation is the `studio` skin: [`src/styles/skins/studio.css`](../../src/styles/skins/studio.css) for the chrome, the classic Home's own markup ([`HomeView`](../../src/components/views/HomeView.tsx), [`HomeSuggestionCard`](../../src/components/views/home/HomeSuggestionCard.tsx), the classic branch of [`MoodRadioGrid`](../../src/components/views/home/MoodRadioGrid.tsx)), and the preset in [`src/lib/skins.ts`](../../src/lib/skins.ts).

The one-line brief: **a precise desktop music player — a neutral canvas, flat surfaces, 1 px lines, one accent, the covers as the only colour, and nothing that costs a frame.**

Where the five materials stage the music, Studio files it. It is not a stripped-down Fluid: it keeps the classic layout (the Home as a stack of sections, three right-edge panels, a bordered sidebar and bar) and makes it more exact, rather than borrowing a shape it would then have to draw cheaply.

## Principles

1. **Fast is the first rule, not a constraint on the others.** Studio is where recovery lands, so a Studio surface may only be drawn with colours, borders, radii, padding and 1 px inset lines. No `backdrop-filter`, no `filter`, no mask, no gradient, no blurred shadow, no animation that loops, no `will-change`. The rule is checked against the implementation, not the look: the classic Home's two banner glows looked cheap and held its frame rate at 12 fps without a GPU while an animation ran beside them ([the bench's first report](../../scripts/bench/webkitgtk/reports/2026-10-25-fedora-iris-xe.md)).
2. **The music is the colour.** The interface is neutral — zinc inks on the theme's ground — and the covers bring the colour. No coloured banners, no gradient fallbacks behind a mix, no coloured icon tiles for the library's figures. Where a cover is missing, a neutral tile with an icon stands in (`Artwork`'s stand-in, marked `data-artwork-placeholder`, loses the accent's gradient under Studio).
3. **One accent, spent on state.** The theme's accent (or the custom one, or the cover's: the accent mode decides upstream) marks what is open, what is playing, what has the keyboard's focus, and the one primary action of a block. Mood Radio is the one place with other colours, each mood's own, as a tint and a small mark.
4. **Lines, not depth.** Surfaces are one step apart and parted by a 1 px line; there is no shadow under a card, a cover or the bar. A hover is a tone; the open item is a stronger tone with the accent as a 2 px edge on its start side; the playing row of a list keeps the accent's tint every list draws.
5. **Dense where you navigate, calm where you listen.** The sidebar's rows, the tables and the top bar are compact and on one grid; the Home gives the music the room, in one grid of covers for every row.
6. **Light and dark.** Studio follows the theme's mode, both ways, and every token below has its two values.
7. **Still.** Colour transitions of 150 ms on a gesture, the Home's one fade-in when it opens; no lift, no scale, nothing that moves on its own. Reduced motion takes the fade-in too. The one thing that loops is a loading skeleton's pulse, for as long as a row loads, and reduced motion stops it.

## Tokens

Declared on `:root[data-skin="studio"]` in `studio.css`, light then dark.

| Token         | Light                                     | Dark                                       | Role                                                          |
| ------------- | ----------------------------------------- | ------------------------------------------ | ------------------------------------------------------------- |
| `canvas`      | the theme's ambient under zinc-50 at 50 % | the theme's ambient under zinc-900 at 50 % | The page, and the top bar, which is the page's own colour.    |
| surface       | white (`--color-surface-light`)           | zinc-900; the rails `--color-surface-dark` | Cards, the sidebar, the panels, the player bar.               |
| `field`       | white                                     | white at 5 %                               | The search field.                                             |
| `line`        | zinc-200                                  | zinc-800                                   | Every edge: a card, the bar's foot, a table, a field at rest. |
| `line-strong` | zinc-300                                  | zinc-700                                   | An edge under the pointer.                                    |
| `tone`        | zinc-900 at 4.5 %                         | white at 5 %                               | Hover.                                                        |
| `tone-strong` | zinc-900 at 7.5 %                         | white at 8.5 %                             | The open item.                                                |
| `ink`         | zinc-900                                  | white                                      | Titles, names, figures.                                       |
| `ink-2`       | zinc-600                                  | zinc-300                                   | Second lines, navigation at rest.                             |
| `ink-3`       | zinc-500                                  | zinc-400                                   | Labels, counts, meta.                                         |
| `accent`      | the theme's 500 stop; 600 for text        | the theme's 400 stop                       | The open item's edge, focus, the primary action.              |
| `accent-soft` | the accent at 10 %                        | the accent at 12 %                         | The focus ring around a field.                                |

High contrast moves `line` and `line-strong` to zinc-500 / 600 (light) and 500 / 400 (dark), on top of the app-wide secondary-ink remap.

### Type

Inter, then the system's sans; tabular figures wherever a number is read in a column or a row of figures.

| Role          | Size       | Weight | Tracking                |
| ------------- | ---------- | ------ | ----------------------- |
| Page title    | 1.75 rem   | 600    | −0.02 em                |
| Section title | 1.125 rem  | 600    | −0.01 em                |
| Body, names   | 0.875 rem  | 500    | 0                       |
| Meta          | 0.75 rem   | 400    | 0                       |
| Label         | 0.6875 rem | 600    | +0.06–0.08 em, capitals |

### Shape and space

| Token                   | Value                                                                   |
| ----------------------- | ----------------------------------------------------------------------- |
| Card radius             | 0.75 rem; a cover, a button, a field 0.5 rem; a sidebar row 0.375 rem   |
| Control                 | 32 px square (navigation, what's new), 32 px high (buttons, profile)    |
| Search field            | 36 px high                                                              |
| Top bar                 | 56 px, opaque, a line under it                                          |
| Sidebar rows            | 32 px for the navigation, 40 px for a row with a cover, 1 px apart      |
| Cover grid              | `repeat(auto-fill, minmax(9.5rem, 1fr))`, 1.25 rem across, 1.5 rem down |
| Section gap on the Home | 2.5 rem                                                                 |

## Components

- **Home header**: a small status line with the accent's dot, the greeting as the page title, one muted line, and the two actions on its right — Browse my library (secondary) and Import folder (primary). No banner, no glow.
- **The library's figures**: one row of four cells (library, liked, recently played, playlists), each a button to the page it counts — a neutral icon on a tone, the figure in tabular ink, the label in `ink-3` — parted by 1 px lines (the row's tone through a 1 px gap). Two columns below 1024 px.
- **Suggestion of the day**: a flat card — the cover at 64 px, the kind as an accent label, the title, artist · album, the reason in `ink-3`, Another suggestion as a ghost button and Play as the primary one. No blurred cover behind it.
- **For you, Recently played, Recently added**: the same grid and the same tile — a square cover with its 0.5 rem corners, the title in `ink` at 500, one meta line in `ink-3` (a mix's family and its track count; the artist; the album's artist). A hover is a tone around the tile; the playing track's cover takes a 2 px accent ring.
- **Mood Radio**: five tiles of one size, each on the surface tinted 5 % (dark: 7 %) with its mood's colour, an icon on a 14 % chip of it, a small static wave of bars at the mood's pace (slow for Sleep, fast for Party), the name in `ink`, the description in `ink-2`, the count in `ink-3`. Hover darkens the edge in the mood's colour; while a radio starts, the tile is busy (`aria-busy`) and its icon a spinner. Nothing in it moves on its own.
- **First steps**: the shared card, flat, at 0.75 rem corners and a smaller head; it renders nothing once closed or complete.
- **Top bar**: opaque, the page's own colour and a line under it; navigation as three 32 px squares; the search 36 px high with 0.5 rem corners, a line at rest, the accent's edge and a 3 px ring of its tint on focus; what's new and the profile on the same 32 px.
- **Sidebar**: the brand, the profile as one compact row, the navigation at 32 px a row, section labels in `ink-3` capitals, playlists at 40 px with their 32 px covers. The open item: `tone-strong`, `ink`, its icon in the accent, a 2 px accent edge on its start side (the end side in a right-to-left language).
- **Tables**: left as the functional surface they are — the frame at the cards' 0.75 rem corners, the header in `ink-3` labels; the rows keep the states every list of tracks shares (a tone on hover, the accent's tint on the playing row, the blue selection).
- **Empty states**: the shared icon, title and line, without the soft glow that breathes behind them in the other skins (`.soft-glow` is hidden under Studio — the same goes for a profile's initial in the profile picker).
- **Panels and player bar**: flat surfaces with a line on their inner edge — the classic layout's own, unchanged by this direction.

## The flat form

Every material has a flat form for a machine without a GPU ([`ui.md`](../features/ui.md#visual-effects)). Studio's is itself: there is no atmosphere, blur, glow or loop to switch off. What Reduced still cuts under Studio is what is not Studio's own — Tailwind's shadows on shared components (menus, dialogs, the album grid's covers), through their variables.

## Where it came from

Rebuilt from the user's brief for 2.0 ("Studio Refined — Functional Elegance"): Studio as the lightweight, predictable fallback, intentionally modest — more precise, more cohesive, not more spectacular. The 1.x Studio was the app's Apple Music–like baseline: a large gradient banner with two glows, four stat cards with coloured icon boxes, saturated gradient mood cards, mixes as wide images under a dark gradient, a translucent top bar over a backdrop blur, rounded 12–24 px everything. Its cost is measured the way every skin's is, with the WebKitGTK bench ([`scripts/bench/webkitgtk`](../../scripts/bench/webkitgtk/README.md)), Studio against itself before and after a change.
