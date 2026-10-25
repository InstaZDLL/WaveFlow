import type { CSSProperties } from "react";

import type { ThemePreset } from "../../../lib/themes";
import type { SkinId } from "../../../lib/skins";
import type { ContrastMode } from "../../../lib/contrast";
import type { EffectsLevel } from "../../../lib/effects";
import type { ReplayGainMode } from "../../../lib/tauri/player";

/*
 * The small drawings on the settings' choice cards: what an option looks
 * like before it is chosen. Drawn with boxes and gradients, never an
 * image or a blur (a CSS blur repaints with the page), and always
 * `aria-hidden` by the card that holds them: the card's label says what
 * the drawing shows.
 *
 * Their shapes live in `settings.css` under `.sp-*`; what depends on the
 * option (a theme's colours, a skin's ground) comes in as custom
 * properties, so one stylesheet draws every variant.
 */

type Vars = CSSProperties & Record<`--${string}`, string>;

/** A miniature window: a sidebar, a header, three tiles and a player bar,
 *  in the theme's own colours. */
export function ThemePreview({ preset }: { preset: ThemePreset }) {
  const dark = preset.mode === "dark";
  const bg = preset.ambient ?? (dark ? "#121212" : "#ffffff");
  const vars: Vars = {
    "--sp-bg": bg,
    "--sp-side": dark
      ? (preset.surfaceDark ?? "#181818")
      : (preset.surfaceLight ?? "#f4f4f5"),
    "--sp-bar": dark
      ? (preset.surfaceDarkElevated ?? "#1c1c1c")
      : (preset.surfaceLightElevated ?? "#ffffff"),
    "--sp-ink": dark ? "rgb(255 255 255 / 0.82)" : "rgb(24 24 27 / 0.78)",
    "--sp-ink-2": dark ? "rgb(255 255 255 / 0.2)" : "rgb(24 24 27 / 0.16)",
    "--sp-edge": dark ? "rgb(255 255 255 / 0.06)" : "rgb(24 24 27 / 0.08)",
    "--sp-accent": dark ? preset.accent[400] : preset.accent[500],
    "--sp-accent-2": dark ? preset.accent[700] : preset.accent[200],
  };
  return (
    <span className="sp-app" style={vars}>
      <span className="sp-app__side">
        <span className="sp-dot" />
        <span className="sp-line sp-line--ink" />
        <span className="sp-line" />
        <span className="sp-line" />
      </span>
      <span className="sp-app__main">
        <span className="sp-app__hero" />
        <span className="sp-line sp-line--ink sp-line--short" />
        <span className="sp-app__tiles">
          <span />
          <span />
          <span />
        </span>
      </span>
      <span className="sp-app__bar">
        <span className="sp-app__play" />
        <span className="sp-app__progress" />
      </span>
    </span>
  );
}

/** A skin's layout and ground: Studio's sidebar, top bar and full-width
 *  bar; the fluid skins' rail, display title, band and floating bar, each
 *  on its material (the near-black and the cover's light, the aurora
 *  behind glass, the neon, the paper, the wine room). */
export function SkinPreview({ skinId }: { skinId: SkinId }) {
  if (skinId === "studio") {
    return (
      <span className="sp-skin sp-skin--studio">
        <span className="sp-studio__side">
          <span className="sp-dot" />
          <span className="sp-line sp-line--ink" />
          <span className="sp-line" />
          <span className="sp-line" />
          <span className="sp-line" />
        </span>
        <span className="sp-studio__top">
          <span className="sp-studio__search" />
        </span>
        <span className="sp-studio__grid">
          <span />
          <span />
          <span />
          <span />
        </span>
        <span className="sp-studio__bar">
          <span className="sp-app__play" />
          <span className="sp-app__progress" />
        </span>
      </span>
    );
  }
  return (
    <span className={`sp-skin sp-skin--fluid sp-skin--${skinId}`}>
      <span className="sp-fluid__rail">
        <span className="sp-dot" />
        <span className="sp-fluid__mark" />
        <span className="sp-fluid__mark" />
        <span className="sp-fluid__mark" />
      </span>
      <span className="sp-fluid__page">
        <span className="sp-fluid__kicker" />
        <span className="sp-fluid__title" />
        <span className="sp-fluid__band">
          <span />
          <span />
          <span />
        </span>
      </span>
      <span className="sp-fluid__bar">
        <span className="sp-app__play" />
        <span className="sp-app__progress" />
      </span>
    </span>
  );
}

/** A card of text at the contrast the mode gives; "auto" shows both,
 *  split on the diagonal, since it follows the system. */
export function ContrastPreview({ mode }: { mode: ContrastMode }) {
  return (
    <span className={`sp-contrast sp-contrast--${mode}`}>
      <span className="sp-contrast__half sp-contrast__half--normal">
        <span className="sp-line sp-line--ink" />
        <span className="sp-line" />
        <span className="sp-line sp-line--short" />
        <span className="sp-contrast__focus" />
      </span>
      <span className="sp-contrast__half sp-contrast__half--high">
        <span className="sp-line sp-line--ink" />
        <span className="sp-line" />
        <span className="sp-line sp-line--short" />
        <span className="sp-contrast__focus" />
      </span>
    </span>
  );
}

/** A cover lighting the page around it (full), the same page flat
 *  (reduced); "auto" shows both, split on the diagonal. */
export function EffectsPreview({ level }: { level: EffectsLevel }) {
  return (
    <span className={`sp-effects sp-effects--${level}`}>
      <span className="sp-effects__half sp-effects__half--full">
        <span className="sp-effects__cover" />
        <span className="sp-line sp-line--ink" />
        <span className="sp-line sp-line--short" />
      </span>
      <span className="sp-effects__half sp-effects__half--reduced">
        <span className="sp-effects__cover" />
        <span className="sp-line sp-line--ink" />
        <span className="sp-line sp-line--short" />
      </span>
    </span>
  );
}

/** The path the sound takes: through the system mixer, alongside the
 *  other applications, or straight to the device. */
export function OutputPathPreview({ exclusive }: { exclusive: boolean }) {
  return (
    <svg
      className="sp-path"
      viewBox="0 0 160 64"
      fill="none"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {/* The player */}
      <rect
        className="sp-path__node sp-path__node--app"
        x="6"
        y="22"
        width="26"
        height="20"
        rx="6"
      />
      <path className="sp-path__glyph" d="M15 37v-9l9-2v9" />
      <circle className="sp-path__glyph" cx="13.5" cy="37" r="1.6" />
      <circle className="sp-path__glyph" cx="22.5" cy="35" r="1.6" />
      {exclusive ? (
        <>
          <path className="sp-path__wire sp-path__wire--direct" d="M34 32h88" />
          <rect
            className="sp-path__lock"
            x="70"
            y="24"
            width="16"
            height="12"
            rx="3"
          />
          <path className="sp-path__glyph" d="M74 24v-3a4 4 0 0 1 8 0v3" />
        </>
      ) : (
        <>
          {/* Other applications joining the mixer */}
          <rect
            className="sp-path__node sp-path__node--other"
            x="42"
            y="6"
            width="18"
            height="12"
            rx="4"
          />
          <rect
            className="sp-path__node sp-path__node--other"
            x="42"
            y="46"
            width="18"
            height="12"
            rx="4"
          />
          <path className="sp-path__wire" d="M34 32h34" />
          <path
            className="sp-path__wire sp-path__wire--other"
            d="M60 12c8 0 8 12 10 16"
          />
          <path
            className="sp-path__wire sp-path__wire--other"
            d="M60 52c8 0 8-12 10-16"
          />
          <circle
            className="sp-path__node sp-path__node--mixer"
            cx="80"
            cy="32"
            r="11"
          />
          <path className="sp-path__glyph" d="M75 35v-6M80 37v-10M85 34v-4" />
          <path className="sp-path__wire" d="M91 32h31" />
        </>
      )}
      {/* The device */}
      <rect
        className="sp-path__node sp-path__node--device"
        x="124"
        y="18"
        width="30"
        height="28"
        rx="7"
      />
      <circle className="sp-path__glyph" cx="139" cy="35" r="5" />
      <circle className="sp-path__glyph" cx="139" cy="25" r="1.6" />
    </svg>
  );
}

/** Signal-strength bars: `level` of `of` lit. */
export function PrecisionBars({ level, of }: { level: number; of: number }) {
  return (
    <span className="sp-bars">
      {Array.from({ length: of }, (_, i) => (
        <span
          key={i}
          data-lit={i < level || undefined}
          style={{ height: `${30 + (70 * (i + 1)) / of}%` }}
        />
      ))}
    </span>
  );
}

/** Loudness per track: each track brought to the line ("track"), or each
 *  album as a whole, its tracks keeping their differences ("album"). */
export function ReplayGainPreview({ mode }: { mode: ReplayGainMode }) {
  // Two albums of three tracks; heights in % of the box.
  const raw = [52, 78, 64, 34, 48, 40];
  const albumMean = [64.7, 40.7];
  const heights = raw.map((h, i) => {
    if (mode === "track") return 58;
    const album = i < 3 ? 0 : 1;
    // Album gain: the album's mean on the line, its spread kept.
    return Math.round(58 + (h - albumMean[album]) * 0.6);
  });
  return (
    <span className={`sp-gain sp-gain--${mode}`}>
      <span className="sp-gain__target" />
      {heights.map((h, i) => (
        <span
          key={i}
          className={i < 3 ? "sp-gain__bar" : "sp-gain__bar sp-gain__bar--b"}
          style={{ height: `${mode === "auto" && i % 2 ? 58 : h}%` }}
        />
      ))}
    </span>
  );
}

/** A window framed by the system (its title bar above the app) or by the
 *  app itself (the app up to the top edge, its own buttons in it). */
export function WindowFramePreview({
  kind,
}: {
  kind: "system" | "adwaita" | "app";
}) {
  return (
    <span className={`sp-frame sp-frame--${kind}`}>
      <span className="sp-frame__titlebar">
        <span />
        <span />
        <span />
      </span>
      <span className="sp-frame__body">
        <span className="sp-frame__side" />
        <span className="sp-frame__main">
          <span className="sp-line sp-line--ink sp-line--short" />
          <span className="sp-line" />
        </span>
      </span>
    </span>
  );
}

/** Two tracks meeting: the outgoing one fading under the incoming one
 *  over `seconds`, drawn on a scale of `max` seconds. */
export function CrossfadePreview({
  seconds,
  max,
}: {
  seconds: number;
  max: number;
}) {
  // The overlap, as a share of the width; the tracks take the rest.
  const overlap = Math.max(0, Math.min(1, seconds / max)) * 0.44;
  const aEnd = 0.56 + overlap / 2;
  const bStart = 0.44 - overlap / 2 + (seconds === 0 ? 0.12 : 0);
  const w = 200;
  const h = 40;
  const ax = aEnd * w;
  const bx = bStart * w;
  // Fade ramps: A falls from full to nothing across the overlap, B rises.
  const fadeStart = seconds === 0 ? ax : bx;
  const fadeEnd = seconds === 0 ? bx : ax;
  const a = `M0 ${h} V6 H${fadeStart} L${ax} ${h} Z`;
  const b = `M${bx} ${h} L${fadeEnd} 6 H${w} V${h} Z`;
  return (
    <svg
      className="sp-fade"
      viewBox={`0 0 ${w} ${h}`}
      preserveAspectRatio="none"
    >
      <path className="sp-fade__a" d={a} />
      <path className="sp-fade__b" d={b} />
    </svg>
  );
}
