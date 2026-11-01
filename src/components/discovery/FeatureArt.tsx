import type { CSSProperties, ReactNode } from "react";

/**
 * The little scenes a feature card opens with. One small SVG per kind,
 * drawn in the ink (`currentColor`) with the accent on the one element
 * that carries the idea, animated by `discovery.css` with transforms and
 * opacity only — no blur, no filter, nothing that repaints the frame.
 * The motion stops under `prefers-reduced-motion` and under the reduced
 * visual effects level, and each scene still reads standing still.
 *
 * Shared by the onboarding, the Home's first steps and the "What's new"
 * panel, whose large illustration (`FeatureHero`) draws one at the centre:
 * a kind names an idea, not a screen.
 */
export type FeatureArtKind =
  | "crossfade"
  | "automix"
  | "replaygain"
  | "analysis"
  | "lyrics"
  | "lyricsSources"
  | "canvas"
  | "motionArtwork"
  | "plugins"
  | "lastfm"
  | "discord"
  | "server"
  | "immersive"
  | "miniPlayer"
  | "desktopLyrics"
  | "smartPlaylist"
  | "enrichment"
  | "tagWorkshop"
  | "wrapped"
  | "fluidInterface"
  | "lyricsEditor"
  | "theater"
  | "releaseRadar"
  | "playlistImport";

/** A row that rises in on a loop, to its own resting opacity. */
function rowStyle(delay: number, opacity: number): CSSProperties {
  return {
    animationDelay: `${delay}s`,
    ["--fa-row-opacity" as string]: opacity,
  };
}

const SCENES: Record<FeatureArtKind, ReactNode> = {
  // Two waves: the outgoing one fades as the incoming one rises.
  crossfade: (
    <>
      <path
        className="fa-out"
        d="M10 45 C 30 20, 50 70, 70 45 S 110 20, 130 45"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        opacity="0.55"
      />
      <path
        className="fa-in"
        d="M30 45 C 50 70, 70 20, 90 45 S 130 70, 150 45"
        fill="none"
        stroke="var(--disc-accent)"
        strokeWidth="2.5"
        strokeLinecap="round"
      />
    </>
  ),
  // Two rows of beats sliding until they line up.
  automix: (
    <>
      {[0, 1, 2, 3, 4, 5].map((i) => (
        <rect
          key={`a${i}`}
          x={22 + i * 20}
          y="26"
          width="4"
          height="14"
          rx="2"
          fill="currentColor"
          opacity="0.55"
        />
      ))}
      <g className="fa-slide">
        {[0, 1, 2, 3, 4, 5].map((i) => (
          <rect
            key={`b${i}`}
            x={22 + i * 20}
            y="50"
            width="4"
            height="14"
            rx="2"
            fill="var(--disc-accent)"
          />
        ))}
      </g>
    </>
  ),
  // Uneven levels settling on one line.
  replaygain: (
    <>
      {[0.9, 0.45, 0.75, 0.35, 0.6].map((h, i) => (
        <rect
          key={i}
          className="fa-level"
          style={{ ["--fa-from" as string]: h }}
          x={30 + i * 22}
          y="20"
          width="12"
          height="50"
          rx="3"
          fill={i === 2 ? "var(--disc-accent)" : "currentColor"}
          opacity={i === 2 ? 1 : 0.5}
        />
      ))}
      <line
        x1="22"
        x2="138"
        y1="20"
        y2="20"
        stroke="currentColor"
        strokeDasharray="3 4"
        opacity="0.4"
      />
    </>
  ),
  // A tempo pulse beside a turning key wheel.
  analysis: (
    <>
      <circle
        className="fa-pulse"
        cx="52"
        cy="45"
        r="12"
        fill="var(--disc-accent)"
      />
      <circle
        cx="52"
        cy="45"
        r="22"
        fill="none"
        stroke="currentColor"
        opacity="0.3"
      />
      <g className="fa-turn">
        {Array.from({ length: 12 }, (_, i) => {
          const a = (i / 12) * Math.PI * 2;
          return (
            <circle
              key={i}
              cx={112 + Math.cos(a) * 22}
              cy={45 + Math.sin(a) * 22}
              r={i === 0 ? 4 : 2.5}
              fill={i === 0 ? "var(--disc-accent)" : "currentColor"}
              opacity={i === 0 ? 1 : 0.45}
            />
          );
        })}
      </g>
    </>
  ),
  // Lines of text, the sung one filling word by word.
  lyrics: (
    <>
      <rect
        x="28"
        y="22"
        width="80"
        height="6"
        rx="3"
        fill="currentColor"
        opacity="0.3"
      />
      <rect
        x="28"
        y="42"
        width="104"
        height="8"
        rx="4"
        fill="currentColor"
        opacity="0.3"
      />
      <rect
        className="fa-sweep"
        x="28"
        y="42"
        width="104"
        height="8"
        rx="4"
        fill="var(--disc-accent)"
      />
      <rect
        x="28"
        y="64"
        width="64"
        height="6"
        rx="3"
        fill="currentColor"
        opacity="0.3"
      />
    </>
  ),
  // Sources plugging into the lyrics.
  lyricsSources: (
    <>
      <rect
        x="86"
        y="24"
        width="50"
        height="6"
        rx="3"
        fill="currentColor"
        opacity="0.3"
      />
      <rect
        x="86"
        y="42"
        width="50"
        height="8"
        rx="4"
        fill="var(--disc-accent)"
      />
      <rect
        x="86"
        y="62"
        width="36"
        height="6"
        rx="3"
        fill="currentColor"
        opacity="0.3"
      />
      {[0, 1, 2].map((i) => (
        <circle
          key={i}
          className="fa-feed"
          style={{ animationDelay: `${i * 0.6}s` }}
          cx="30"
          cy={26 + i * 20}
          r="5"
          fill="currentColor"
          opacity="0.6"
        />
      ))}
    </>
  ),
  // A cover whose picture moves inside its frame.
  canvas: (
    <>
      <rect
        x="58"
        y="8"
        width="44"
        height="74"
        rx="6"
        fill="none"
        stroke="currentColor"
        opacity="0.45"
      />
      <g className="fa-drift">
        <circle cx="80" cy="34" r="12" fill="var(--disc-accent)" />
        <rect
          x="66"
          y="54"
          width="28"
          height="5"
          rx="2.5"
          fill="currentColor"
          opacity="0.5"
        />
      </g>
    </>
  ),
  // A sleeve with a light passing over it.
  motionArtwork: (
    <>
      <rect
        x="50"
        y="15"
        width="60"
        height="60"
        rx="8"
        fill="currentColor"
        opacity="0.18"
      />
      <circle cx="80" cy="45" r="16" fill="var(--disc-accent)" opacity="0.85" />
      <rect
        className="fa-shine"
        x="50"
        y="15"
        width="14"
        height="60"
        fill="currentColor"
        opacity="0.35"
      />
    </>
  ),
  // Blocks snapping into place.
  plugins: (
    <>
      <rect
        x="40"
        y="25"
        width="34"
        height="40"
        rx="6"
        fill="currentColor"
        opacity="0.3"
      />
      <rect
        className="fa-snap"
        x="80"
        y="25"
        width="34"
        height="40"
        rx="6"
        fill="var(--disc-accent)"
      />
    </>
  ),
  // Plays flowing into a chart.
  lastfm: (
    <>
      {[14, 24, 34, 20].map((h, i) => (
        <rect
          key={i}
          x={84 + i * 12}
          y={70 - h}
          width="8"
          height={h}
          rx="2"
          fill="currentColor"
          opacity="0.45"
        />
      ))}
      {[0, 1, 2].map((i) => (
        <circle
          key={i}
          className="fa-feed"
          style={{ animationDelay: `${i * 0.6}s` }}
          cx="34"
          cy="45"
          r="5"
          fill="var(--disc-accent)"
        />
      ))}
    </>
  ),
  // A presence card with a live dot.
  discord: (
    <>
      <rect
        x="30"
        y="24"
        width="100"
        height="42"
        rx="10"
        fill="currentColor"
        opacity="0.14"
      />
      <circle cx="52" cy="45" r="11" fill="currentColor" opacity="0.4" />
      <circle
        className="fa-pulse"
        cx="60"
        cy="53"
        r="4"
        fill="var(--disc-accent)"
      />
      <rect
        x="70"
        y="36"
        width="46"
        height="6"
        rx="3"
        fill="currentColor"
        opacity="0.5"
      />
      <rect
        x="70"
        y="49"
        width="32"
        height="5"
        rx="2.5"
        fill="currentColor"
        opacity="0.3"
      />
    </>
  ),
  // Two machines and what travels between them.
  server: (
    <>
      <rect
        x="18"
        y="28"
        width="34"
        height="34"
        rx="6"
        fill="none"
        stroke="currentColor"
        opacity="0.5"
      />
      <rect
        x="108"
        y="28"
        width="34"
        height="34"
        rx="6"
        fill="none"
        stroke="currentColor"
        opacity="0.5"
      />
      <circle
        className="fa-travel"
        cx="62"
        cy="45"
        r="4"
        fill="var(--disc-accent)"
      />
    </>
  ),
  // A frame opening to fill the screen.
  immersive: (
    <>
      <rect
        x="20"
        y="10"
        width="120"
        height="70"
        rx="8"
        fill="none"
        stroke="currentColor"
        opacity="0.35"
      />
      <rect
        className="fa-grow"
        x="60"
        y="27"
        width="40"
        height="36"
        rx="5"
        fill="var(--disc-accent)"
      />
    </>
  ),
  // A small window lifting off the big one.
  miniPlayer: (
    <>
      <rect
        x="18"
        y="12"
        width="96"
        height="62"
        rx="7"
        fill="currentColor"
        opacity="0.14"
      />
      <g className="fa-lift">
        <rect
          x="92"
          y="42"
          width="52"
          height="30"
          rx="6"
          fill="currentColor"
          opacity="0.5"
        />
        <circle cx="106" cy="57" r="7" fill="var(--disc-accent)" />
      </g>
    </>
  ),
  // A line floating over the desktop.
  desktopLyrics: (
    <>
      <rect
        x="14"
        y="10"
        width="132"
        height="70"
        rx="6"
        fill="currentColor"
        opacity="0.1"
      />
      <rect
        x="22"
        y="20"
        width="30"
        height="20"
        rx="4"
        fill="currentColor"
        opacity="0.18"
      />
      <rect
        className="fa-float"
        x="38"
        y="58"
        width="84"
        height="8"
        rx="4"
        fill="var(--disc-accent)"
      />
    </>
  ),
  // Rules sorting tracks into a list.
  smartPlaylist: (
    <>
      <rect
        x="20"
        y="24"
        width="38"
        height="12"
        rx="6"
        fill="var(--disc-accent)"
      />
      <rect
        x="20"
        y="44"
        width="30"
        height="12"
        rx="6"
        fill="currentColor"
        opacity="0.4"
      />
      {[0, 1, 2, 3].map((i) => (
        <rect
          key={i}
          className="fa-row"
          style={rowStyle(i * 0.25, 0.45)}
          x="78"
          y={18 + i * 15}
          width="62"
          height="9"
          rx="4.5"
          fill="currentColor"
          opacity="0.45"
        />
      ))}
    </>
  ),
  // An album gaining what it was missing.
  enrichment: (
    <>
      <rect
        x="22"
        y="18"
        width="54"
        height="54"
        rx="7"
        fill="currentColor"
        opacity="0.2"
      />
      <circle cx="49" cy="45" r="13" fill="currentColor" opacity="0.35" />
      {[0, 1, 2].map((i) => (
        <rect
          key={i}
          className="fa-row"
          style={rowStyle(i * 0.35, i === 0 ? 1 : 0.45)}
          x="88"
          y={24 + i * 16}
          width={i === 0 ? 50 : 38}
          height="9"
          rx="4.5"
          fill={i === 0 ? "var(--disc-accent)" : "currentColor"}
          opacity={i === 0 ? 1 : 0.45}
        />
      ))}
    </>
  ),
  // Rows of fields filled one after the other.
  tagWorkshop: (
    <>
      {[0, 1, 2].map((i) => (
        <g key={i}>
          <rect
            x="20"
            y={20 + i * 18}
            width="34"
            height="9"
            rx="3"
            fill="currentColor"
            opacity="0.3"
          />
          <rect
            x="62"
            y={20 + i * 18}
            width="78"
            height="9"
            rx="3"
            fill="currentColor"
            opacity="0.15"
          />
          <rect
            className="fa-fill"
            style={{ animationDelay: `${i * 0.4}s` }}
            x="62"
            y={20 + i * 18}
            width="78"
            height="9"
            rx="3"
            fill="var(--disc-accent)"
          />
        </g>
      ))}
    </>
  ),
  // The year's cards rising one over the other.
  wrapped: (
    <>
      {[0, 1, 2].map((i) => (
        <rect
          key={i}
          className="fa-row"
          style={rowStyle(i * 0.3, i === 2 ? 1 : 0.25 + i * 0.15)}
          x={44 + i * 14}
          y={22 + i * 8}
          width="46"
          height="56"
          rx="7"
          fill={i === 2 ? "var(--disc-accent)" : "currentColor"}
          opacity={i === 2 ? 1 : 0.25 + i * 0.15}
        />
      ))}
    </>
  ),
  // A window: the rail, a large cover, and tiles settling beside it.
  fluidInterface: (
    <>
      <rect
        x="16"
        y="10"
        width="128"
        height="70"
        rx="9"
        fill="currentColor"
        opacity="0.14"
      />
      <rect
        x="22"
        y="16"
        width="14"
        height="58"
        rx="5"
        fill="currentColor"
        opacity="0.28"
      />
      <rect
        className="fa-drift"
        x="44"
        y="24"
        width="42"
        height="42"
        rx="7"
        fill="var(--disc-accent)"
      />
      {[0, 1, 2].map((i) => (
        <rect
          key={i}
          className="fa-row"
          style={rowStyle(i * 0.3, 0.45)}
          x="94"
          y={22 + i * 17}
          width="44"
          height="12"
          rx="4"
          fill="currentColor"
          opacity="0.45"
        />
      ))}
    </>
  ),
  // Stamps down the margin, a line's words filling one after the other.
  lyricsEditor: (
    <>
      {[0, 1, 2].map((i) => (
        <circle
          key={i}
          cx="24"
          cy={24 + i * 21}
          r="3.5"
          fill="currentColor"
          opacity="0.4"
        />
      ))}
      <rect
        x="36"
        y="20"
        width="92"
        height="8"
        rx="4"
        fill="currentColor"
        opacity="0.28"
      />
      {[0, 1, 2].map((i) => (
        <g key={i}>
          <rect
            x={36 + i * 36}
            y="40"
            width="30"
            height="10"
            rx="5"
            fill="currentColor"
            opacity="0.28"
          />
          <rect
            className="fa-fill"
            style={{ animationDelay: `${i * 0.5}s` }}
            x={36 + i * 36}
            y="40"
            width="30"
            height="10"
            rx="5"
            fill="var(--disc-accent)"
          />
        </g>
      ))}
      <rect
        x="36"
        y="62"
        width="64"
        height="8"
        rx="4"
        fill="currentColor"
        opacity="0.28"
      />
      <circle
        className="fa-pulse"
        cx="24"
        cy="45"
        r="4.5"
        fill="var(--disc-accent)"
      />
    </>
  ),
  // A screen across the room: the cover stays, the controls fade away.
  theater: (
    <>
      <rect
        x="26"
        y="8"
        width="108"
        height="62"
        rx="6"
        fill="none"
        stroke="currentColor"
        opacity="0.55"
      />
      <rect
        x="68"
        y="70"
        width="24"
        height="7"
        fill="currentColor"
        opacity="0.3"
      />
      <rect
        x="54"
        y="77"
        width="52"
        height="4"
        rx="2"
        fill="currentColor"
        opacity="0.4"
      />
      <rect
        className="fa-pulse"
        x="62"
        y="16"
        width="36"
        height="36"
        rx="5"
        fill="var(--disc-accent)"
      />
      <rect
        className="fa-out"
        x="50"
        y="58"
        width="60"
        height="5"
        rx="2.5"
        fill="currentColor"
      />
    </>
  ),
  // A sweep going round, new releases lighting up as it passes.
  releaseRadar: (
    <>
      {[34, 22, 10].map((r) => (
        <circle
          key={r}
          cx="80"
          cy="45"
          r={r}
          fill="none"
          stroke="currentColor"
          opacity="0.3"
        />
      ))}
      <g className="fa-sweep-turn">
        <line
          x1="80"
          y1="45"
          x2="80"
          y2="11"
          stroke="var(--disc-accent)"
          strokeWidth="2.5"
          strokeLinecap="round"
        />
        <circle cx="80" cy="79" r="0.1" fill="none" />
      </g>
      {[
        [100, 30, 0],
        [62, 58, 0.5],
        [96, 62, 0.9],
      ].map(([cx, cy, delay]) => (
        <circle
          key={cx}
          className="fa-pulse"
          style={{ animationDelay: `${delay}s` }}
          cx={cx}
          cy={cy}
          r="4"
          fill="var(--disc-accent)"
        />
      ))}
    </>
  ),
  // A file's entries travelling into a list, a missing one held in place.
  playlistImport: (
    <>
      <rect
        x="16"
        y="20"
        width="38"
        height="50"
        rx="5"
        fill="currentColor"
        opacity="0.22"
      />
      {[0, 1, 2].map((i) => (
        <rect
          key={`l${i}`}
          x="23"
          y={30 + i * 11}
          width="24"
          height="4"
          rx="2"
          fill="currentColor"
          opacity="0.55"
        />
      ))}
      {[0, 1, 2].map((i) => (
        <circle
          key={`d${i}`}
          className="fa-feed"
          style={{ animationDelay: `${i * 0.6}s` }}
          cx="62"
          cy="45"
          r="3.5"
          fill="var(--disc-accent)"
        />
      ))}
      <rect
        x="114"
        y="24"
        width="32"
        height="9"
        rx="4.5"
        fill="var(--disc-accent)"
      />
      <rect
        x="114.5"
        y="41"
        width="31"
        height="8"
        rx="4"
        fill="none"
        stroke="currentColor"
        strokeDasharray="3 3"
        opacity="0.6"
      />
      <rect
        x="114"
        y="57"
        width="32"
        height="9"
        rx="4.5"
        fill="var(--disc-accent)"
      />
    </>
  ),
};

export function FeatureArt({ kind }: { kind: FeatureArtKind }) {
  return (
    <svg
      viewBox="0 0 160 90"
      className="feature-art"
      aria-hidden="true"
      focusable="false"
    >
      {SCENES[kind]}
    </svg>
  );
}
