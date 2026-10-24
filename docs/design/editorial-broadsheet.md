# Editorial — Broadsheet

The fourth material on the 2.0 interface, beside [Fluid Editorial](./fluid-editorial.md), [Liquid — Aurora Glass](./liquid-aurora-glass.md) and [Pulse — Neon Deck](./pulse-neon-deck.md). All four sit on the same structure — the canvas, the rail, the control rail, the tabbed panel, the page heroes — and this page writes down what Editorial makes of it, so the web app, the Android app and the landing site can carry it. The desktop implementation is the `editorial` skin ([`src/styles/skins/editorial.css`](../../src/styles/skins/editorial.css) over [`fluid.css`](../../src/styles/skins/fluid.css), [`src/lib/skins.ts`](../../src/lib/skins.ts)); this page is the intent and the values.

The one-line brief: **a broadsheet — cream paper with its grain, ink for everything, rules where the other materials put tone, and the music as a halftone plate.**

Where Fluid stages the music, Liquid diffuses it and Pulse wires it, Editorial prints it. It is the 1.x "WaveFlow Gazette" carried onto the 2.0 structure — the masthead, the Playfair display, the Lora body, the double rules, the dotted dividers, the "Fig." captions, the halftone covers, the ruler, the page numbers — with two changes of principle: the structure is the shared one (the canvas, not a front-page grid of its own), and the one colour is the theme's accent, not a press red of its own.

## Principles

1. **Paper, in both themes.** The ground is cream paper with a multiplied grain, the inks near-black, and the dark theme changes nothing: a newspaper is not printed on black. The `dark:` utilities that still fire under the dark theme are folded back onto the inks and the paper.
2. **No light.** Print has no glow: no pools of the cover's colour, no blurred copy of the cover under the canvas, no gradient on the player bar or the panel. Depth is a rule, never a shadow — except the one offset hard shadow under a floating sheet (a menu, a dialog), which is how a pasted-up proof casts it.
3. **One accent, the theme's, as the rubric colour.** It is spent the way a broadsheet spends its one colour: the kickers and section labels, the open item's edge, the sung word, the played part of the ruler, the focus ring. The primary action is **not** the accent: it is set in ink, paper type on a black field, the way a broadsheet sets its one box. The 1.x press red is gone; a warm theme or the custom accent mode (#895) gives the gazette its red back. The stop is the 800, not the 600 the dark materials use: the accent is set as small type on paper, where the 600 stop falls to 2.7–3.9:1 for five of the seven scales and the 800 holds 5.5:1 at least.
4. **Two faces.** Lora for the body (`--fe-font`), Playfair Display for the display: the greeting (the layout's editorial slot, in italic), the masthead, every display and section title at 900, the page heroes. Labels are Lora at 700, tracked and in capitals.
5. **Rules, not tone.** A section title sits on a double rule, a table header on a rule, rows on dotted rules; the rail and the panel are parted from the page by a 2 px rule, the player bar by the masthead's 3 + 1 rule inverted. Cards are a 1 px frame; the "light" a top edge catches in the other materials is here a thin ink rule. Corners are square everywhere the structure rounds them — portraits excepted, which stay vignettes.
6. **The halftone plate.** The canvas cover under a dot screen, the strip under it, the record on the player bar, the covers on the rail and the hero of the panel are grayscale; the library's grids keep their colour — they are the catalogue, not the illustration.
7. **Paper is slow.** One ease, `cubic-bezier(0.4, 0, 0.2, 1)`; nothing loops; the structure's one-shot rise stays; a hover is a tone or an inversion, never a lift.

## Tokens

| Token     | Value                                                                                              | Role                                                                                          |
| --------- | -------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `paper`   | `#f0ebd8` · `#e8e2ce` (warm, a sheet on the page) · `#ddd4b6` (deep)                               | The page, the bands and the menus, a field.                                                   |
| `grain`   | an SVG noise at 7 %, multiplied, tiled at 200 px                                                   | Over the paper, off under high contrast.                                                      |
| `ink`     | `#1a1a1a` · `#3a3530` · `#6a6258`                                                                  | Title · body · label and caption.                                                             |
| `rule`    | ink at 28 % · at 78 % (strong) · at 14 % (soft)                                                    | Dividers, frames, the dotted rows.                                                            |
| `tone`    | ink at 4 / 6 / 8 / 11 %                                                                            | Hover, the open row, the playing row — over paper, so black, where Fluid's are white.         |
| `edge`    | ink at 14 % · 28 %                                                                                 | The structure's "top light" of a card, printed as a thin rule along its top.                  |
| `accent`  | the theme's 800 stop, white ink (the 600 stop falls under 4.5:1 as small type on paper)            | The rubric colour: kickers, the open item's edge, the sung word, the played part, focus.      |
| `glow`    | `#8a7f6a`                                                                                          | Declared so the few shadows the structure mixes it into read as a print shadow; never a pool. |
| `proof`   | `6px 6px 0` ink at 12 %                                                                            | The hard offset shadow under a menu or a dialog.                                              |
| `screen`  | a radial dot at 0.6 px, tiled at 3 px, multiplied at 55 %                                          | Over the canvas plate.                                                                        |
| `ease`    | `cubic-bezier(0.4, 0, 0.2, 1)`                                                                     | Every transition.                                                                             |
| `display` | Playfair Display 900 (the greeting in its 400 italic at 700)                                       | The masthead, the display titles, the section titles, the page heroes, the sung line.         |
| `body`    | Lora 400 / 500 / 700, with `onum` in running text and `lnum tnum` in columns and on the player bar | Everything else; labels at 700, tracked 0.16 em, in capitals.                                 |

### States

| State   | Treatment                                                                             |
| ------- | ------------------------------------------------------------------------------------- |
| idle    | paper, a 1 px frame in the rule colour (a module, a card, a pill)                     |
| hover   | the frame goes to full ink and the surface takes the 4 % tone; a ghost pill inverts   |
| open    | inverted: paper type on an ink field (a rail module, the chosen mood column)          |
| playing | the 6 % tone with a 3 px bar of the rubric colour on its left                         |
| focused | a 2 px outline of the rubric colour, inset; on an ink button, a ring over a paper gap |

## Components

- **Masthead** (`EditorialMasthead`, over the canvas): the edition strip (edition · the date in the user's locale · price) on a rule, "WaveFlow" in the display face at 900 in capitals with "Gazette" in italic under it, the 4 + 1 rule, the subtitle quoted between two rules. It is a piece of the preset (`fluid.greeting: "masthead"`), read by `FluidHome` — never a branch on the skin id — and it takes the date with it: the canvas's counters line drops it.
- **Canvas**: the layout's grid; the greeting in Playfair italic; the kicker in the rubric colour; the title in the display face; the resume button in ink; the cover as the plate — grayscale under the dot screen, a 2 px ink frame — with the "Fig. 1 — From «album» by artist" caption under it, below the strip of recent records.
- **Rail**: paper with a 2 px rule on its right; the wordmark in the display face; the profile framed, its initial on an ink disc; the navigation modules framed, numbered "P. n" in their corner (a counter on the rail), the open one inverted; the section titles in the rubric colour on a rule; playlists as rows parted by dotted rules.
- **Control rail**: transparent; the search a field with an ink underline and an italic placeholder, the rubric colour underlining it on focus; the theme switch and the profile on a framed cluster.
- **Mood Radio — the engraving**: the layout's band on paper, its gradient gone; five columns parted by ink rules, the waves in ink; the chosen column inverted — paper type on ink, its wave in paper — with the start button a paper box on it; the BPM ruler under the band on a rule.
- **Player bar**: paper under the masthead's rule inverted (3 px of ink, a paper gap, a 1 px rule); the record as a framed halftone plate; the play button an ink square; and the signature, the progress as a **ruler in picas** on the inline row (`fluid.progress: "inline"`): minor ticks every 5 %, major ticks every 25 %, the played part in the rubric colour behind them, the thumb a square of ink.
- **Side panel**: the facing page, paper with a 2 px rule on its left; the tabs in capitals with an ink underline under the open one; Details with the plate (the cover or the clip, grayscale) under a stronger scrim and the identity in white, the pills framed and in capitals, the lyrics glimpse framed and italic; Lyrics in Lora with no blur, the sung line in the display face, the sung word in the rubric colour; Queue with the playing row on a tone with the rubric's bar.
- **Pages**: the heroes in the display face with the kicker in the rubric colour; the primary action an ink box, the secondaries framed; the artist banner's scrim fades into the paper; cards (extensions, folders, settings groups, genres, years) framed, a tone and full ink on hover; the biography with a **drop cap** in the display face; tables with a rule under the header and dotted rules between rows.
- **Menus, popovers, dialogs**: paper with the grain, a 1 px ink frame, the proof's shadow; a row inverts on hover.
- **Immersive**: shared with Fluid (always dark), in this skin's faces.

## What not to do

No light: no pool of the cover's colour, no blurred cover, no glow under anything, no gradient on a surface. No shadow but the proof's. No rounded corner but a portrait. No fill in the accent on the primary action — ink, always. No colour in the mood band: the engraving is ink on paper. No halftone on the library's grids: the covers there are the catalogue. Nothing loops.

## Carrying it to another surface

The tokens map to CSS variables as Fluid's do: the paper, the three inks, the rule at three strengths, the tones as black at 4–11 %, the proof's shadow, the two faces (Playfair Display 400 italic and 900, Lora 400 / 500 / 700, shipped as webfonts). The dot screen is one tiled radial gradient multiplied over the plate; the ruler is two repeating linear gradients behind the played part. On Android, the frames are a 1 dp stroke, the proof's shadow an offset with no blur, the drop cap a first-letter span in the display face. The structure, the type scale and the rhythm come from [Fluid Editorial](./fluid-editorial.md) unchanged; the mark becomes a rule.
