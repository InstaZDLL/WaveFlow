import type { CSSProperties } from "react";
import { FeatureArt, type FeatureArtKind } from "./FeatureArt";
import "./discovery.css";

const ROWS = [
  { offset: 0, seconds: 64, reverse: false },
  { offset: 2, seconds: 82, reverse: true },
  { offset: 4, seconds: 72, reverse: false },
];

/** Times the words repeat in one half of a row: one half has to outrun the
 *  illustration's width, even with a few short words. */
const REPEATS = 4;

/**
 * A feature told large: its scene at the centre, over a gradient of the
 * feature's colour with rows of its keywords sliding behind. Purely
 * decorative, since the page says all of it in words.
 *
 * The gradient is painted once and never moves; each row is a layer of its
 * own translated by the compositor, so the motion repaints nothing. Under
 * `prefers-reduced-motion` and the reduced effects level the rows stand
 * still and the scene holds, which reads just as well.
 */
export function FeatureHero({
  art,
  color,
  keywords,
}: {
  art: FeatureArtKind;
  color: string;
  keywords: string[];
}) {
  return (
    <div
      className="feature-hero"
      style={{ ["--hero-color" as string]: color } as CSSProperties}
      aria-hidden="true"
    >
      <div className="feature-hero__rows">
        {ROWS.map(({ offset, seconds, reverse }) => {
          const shift = keywords.length ? offset % keywords.length : 0;
          const words = [...keywords.slice(shift), ...keywords.slice(0, shift)];
          const half = Array.from({ length: REPEATS }, () => words).flat();
          return (
            <div
              key={offset}
              className="feature-hero__track"
              data-reverse={reverse || undefined}
              style={{ animationDuration: `${seconds}s` }}
            >
              {[...half, ...half].map((word, i) => (
                <span key={i} dir="auto" className="feature-hero__chip">
                  {word}
                </span>
              ))}
            </div>
          );
        })}
      </div>
      <div className="feature-hero__plate">
        <FeatureArt kind={art} />
      </div>
    </div>
  );
}
