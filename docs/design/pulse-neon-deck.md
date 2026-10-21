# Pulse — Neon Deck

The third material on the 2.0 interface, beside [Fluid Editorial](./fluid-editorial.md) and [Liquid — Aurora Glass](./liquid-aurora-glass.md). All three sit on the same structure — the canvas, the rail, the control rail, the tabbed panel, the page heroes — and this page writes down what Pulse makes of it, so the web app, the Android app and the landing site can carry it. The desktop implementation is the `pulse` skin ([`src/styles/skins/pulse.css`](../../src/styles/skins/pulse.css) over [`fluid.css`](../../src/styles/skins/fluid.css), [`src/lib/skins.ts`](../../src/lib/skins.ts)); this page is the intent and the values.

The one-line brief: **a club control panel — OLED black, the chrome drawn as hairlines, and the accent as neon on whatever is live.**

Where Fluid stages the music and Liquid diffuses it, Pulse wires it: nothing is filled, everything is outlined, and the thing in use lights up. It is the 1.x Pulse's "OLED minimalism with neon edge lighting" carried onto the 2.0 structure — the floating pill of a player bar, the vinyl that turns, the `///` marker, the spring — with one change of principle: the neon is no longer its own.

## Principles

1. **One neon, the theme's.** The 1.x Pulse owned two hard-coded neons (cyan for hover, magenta for active). The 2.0 Pulse takes the theme's accent — the picked colour, or the cover's, through the same accent modes as every other skin — for every live state: hover, open, playing, focus. The second light is the record's own (`glow`), in the atmosphere and under the artwork and the player. Two colours, as before; both the music's.
2. **The chrome reads as absent.** The rail and the panel are the page's black with a 1 px hairline for their edge. A module on them is transparent and ringed; a card on the page is a step of black and ringed; nothing has a fill at rest.
3. **Live means lit.** A hovered thing takes the accent's ring and a halo outside it, and lifts a pixel. An open or playing thing is filled 7 % with the accent, ringed in it, with a halo outside and a little light inside — the one place a fill is allowed. A focused thing takes the ring and the halo, without the fill.
4. **OLED black, not near-black.** The page is `#000`. The two pools of light at its corners and the hairlines are what keep it from reading as a void. No grain, no scanlines: the black is the texture.
5. **Two faces.** Space Grotesk for the display and the body, at the layout's settled weights; Space Mono for the utility voice — labels and kickers with the `///` marker in the accent, time codes, the search, the inputs, the audio facts, and the greeting line, which Fluid sets in a serif italic.
6. **The spring.** One easing, `cubic-bezier(0.34, 1.56, 0.64, 1)`: a slight overshoot that lands as a snap — the rise on the Home, the lift on a hover, the deck opening a segment. Reduced motion puts the layout's ease-out back.
7. **Blur on what never scrolls only.** The floating player bar blurs the page running under it; the rail and the panel sit over the fixed pools, which do not move, so a translucent black is enough for them.

## Tokens

| Token           | Value                                                                                                                                            | Role                                                                                                 |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| `bg`            | `#000000`                                                                                                                                        | The page.                                                                                            |
| `surface`       | `#0a0a0c` · `#121216` (second step) · `#050507` (what scrolls under a sticky header)                                                             | Cards, menus, dialogs, the table header.                                                             |
| `chrome`        | black at 55 %                                                                                                                                    | The rail and the panel, over the fixed pools. Solid `surface` in the fallbacks.                      |
| `bar`           | `rgb(6 6 8)` at 74 %, blurred 20 px                                                                                                              | The floating player bar. Solid in the fallbacks.                                                     |
| `ink`           | `#f4f4f5` · `#a9a9b2` · `#7a7a85`                                                                                                                | Title · body · label. Cool, not warm: Fluid's inks are warm.                                         |
| `hairline`      | white 10 % · 18 % (strong)                                                                                                                       | The edge of everything: rails, rings, dividers.                                                      |
| `accent`        | the theme's 400 stop, black ink                                                                                                                  | The neon.                                                                                            |
| `glow`          | sampled from the cover; default `#4a1f5a`                                                                                                        | The second pool, the halo under the artwork and the player bar, the pool at the record's end of it.  |
| `ring`          | `inset 0 0 0 1px hairline` · `inset 0 0 0 1px accent` · the accent at 55 % for hover                                                             | A surface's edge, a live surface's edge.                                                             |
| `halo`          | `0 0 20px -6px` accent 55 % (hover) · `0 0 28px -4px` accent 70 % + `inset 0 0 14px -8px` accent 45 % (open) · `0 0 28px -6px` accent 75 % (CTA) | The light outside a live surface.                                                                    |
| `fill`          | accent at 7 % (open, playing) · 11 % (a playing row hovered)                                                                                     | The one fill.                                                                                        |
| `neon line`     | `drop-shadow(0 0 4px accent 80 %)`                                                                                                               | On the mark: the flow line, the wave, the chosen mood's wave, the tab's underline, the brand's mark. |
| `ease`          | `cubic-bezier(0.34, 1.56, 0.64, 1)`, the layout's ease-out under reduced motion                                                                  | Every transition.                                                                                    |
| `font` / `mono` | Space Grotesk (400–700, the layout's weights map onto 500 and 600) / Space Mono (400, 700)                                                       | Display and body / the utility voice.                                                                |

The atmosphere: two static pools at the corners of the window — the accent at 14 % top-left, the glow at 55 % bottom-right — fading over 900 ms when the record changes. Under the canvas text and the suggestion band, the layout's pool of the glow stays (at 0.45 and 0.35) but its blurred copy of the cover goes: on pure black a pale sleeve read as a grey smudge.

### States

| State   | Treatment                                                                                     |
| ------- | --------------------------------------------------------------------------------------------- |
| idle    | transparent (a module) or `surface` (a card), the hairline ring                               |
| hover   | the accent's ring at 55 %, the hover halo, 1 px lift (2 px for a card); the accent as ink     |
| active  | `scale(0.94–0.97)` on pointer-down                                                            |
| open    | the 7 % fill, the accent's ring, the open halo; the icon chip filled 14 %                     |
| focused | the accent's ring and the hover halo, no fill                                                 |
| playing | the row filled 7 % with the accent's edge and its light inset; the record's thumbnail turning |

## Components

- **Rail**: the chrome black with a hairline on its right; the wordmark as a tracked capital label with the mark lit; the profile a ringed capsule, its avatar in the two lights; the navigation modules two to a row, ringed, the icon in a ringed disc, the label in the mono voice; the open module filled and lit; playlists as rows, the open one filled with the accent's edge glowing.
- **Control rail**: transparent; the navigation squares ringed and lit on hover; the search a black field with a hairline, the mono voice inside, the accent's ring and halo on focus with the cover's light under it; theme switch and profile on one ringed capsule.
- **Canvas** (Home): the layout's grid; the greeting in Space Mono; the kicker with the `///` marker; the hero cover ringed, on the record's light; the CTA the accent filled and lit; the strip of recent records ringed, the current one ringed in the accent.
- **Mood Radio — the tempo deck**: Fluid's band, dimmed to 42 % and saturated under a black scrim; the segments parted by hairlines; the chosen one filled 7 %, ringed in the accent and lit inside, its wave in the accent as a lit tube (the others' in white, softly lit); the start pill the accent filled. Not Liquid's capsules: the moods stay one band whose axis is tempo — a deck, not five objects.
- **Player bar — the floating pill**: inset 1.25 rem from the sides and 0.75 rem from the foot, 1.75 rem corners, the chrome blurred; the cover's light pooled at the record's end, the glow's halo under it; the wave along its top as the neon tube with a lit thumb; the record a disc that turns while the music plays; the transport in ink, the accent on hover, a snap on press; the play button the accent filled and lit; the audio facts in the mono voice, following the pill's corners.
- **Side panel**: the chrome black with a hairline on its left and the cover's light from its top; the tabs in the mono voice, capitals, the open one's underline lit; Details with the layout's cover edge to edge, the pills ringed; Lyrics with the sung line carrying the accent's light; Queue with the playing row filled and lit.
- **Cards and tables**: cards `surface` with a hairline ring, the accent's ring and halo on hover; the folder tile's icon in a ringed disc, in the accent; the table's playing row filled and lit, the hover from the accent at 8 %.
- **Menus, popovers, dialogs**: `surface`, 1–1.25 rem corners, a strong hairline ring, a faint halo of the accent, the deep shadow; a row takes the accent's fill and ink on hover.
- **Immersive**: shared with Fluid, unchanged here.

## What not to do

Nothing runs forever except the vinyl, and only while the music plays (off under reduced motion). No second neon of Pulse's own: the hover and the open state are the same colour at different strengths. No fill at rest, no fill on hover — a fill is a state. No grain, no scanlines, no gradient on a surface (the player bar's pool of the cover's light and the panel's are the two exceptions, and they are the record's). No glass on the page: a card is black with an edge, never a sheet. The marker `///` opens a block; it never decorates a title or a button.

## Carrying it to another surface

The tokens map to CSS variables as Fluid's do: the three surfaces, the hairline, the accent, the glow, the ring and halo recipes. The two faces ship as webfonts (Space Grotesk 400–700, Space Mono 400 and 700). The neon line is one `drop-shadow` on the mark. On Android, the halos are a coloured elevation shadow and the rings a 1 dp stroke; the vinyl is a rotation tied to the playback state, as the desktop's `data-is-playing` mark is. The mark, the type scale and the rhythm come from [Fluid Editorial](./fluid-editorial.md#the-mark) unchanged.
