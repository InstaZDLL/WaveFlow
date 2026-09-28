import { Fragment } from "react";

import type { LyricsReading } from "../../lib/tauri/lyrics";

/**
 * The background vocals of a line (TTML `x-bg`), drawn smaller under
 * it. Shared by every lyrics surface so they all read the same.
 *
 * Their words run on their own clock, so the highlight follows
 * `activeWordIndex` — the background's own, from `useTrackLyrics` — and
 * not the lead's. No progressive fill: `useKaraokeWordFill` drives one
 * element, and the sweep belongs to the lead the eye follows.
 */
export function BackgroundVocals({
  reading,
  active,
  activeWordIndex,
  className = "",
}: {
  reading: LyricsReading;
  /** Whether the line it belongs to is the one being sung. */
  active: boolean;
  activeWordIndex: number;
  className?: string;
}) {
  if (!active || !reading.words) {
    return (
      <span className={`block ${className}`} style={{ opacity: 0.55 }}>
        {reading.text}
      </span>
    );
  }
  const words = reading.words;
  return (
    <span className={`block ${className}`}>
      {words.map((word, wi) => (
        <Fragment key={wi}>
          <span
            style={{
              opacity:
                wi === activeWordIndex ? 1 : wi < activeWordIndex ? 0.75 : 0.4,
              transition: "opacity 150ms ease",
            }}
          >
            {word.text}
          </span>
          {wi < words.length - 1 && " "}
        </Fragment>
      ))}
    </span>
  );
}

/**
 * Three dots shown through an instrumental stretch, filling one by one
 * as it runs out and breathing while it lasts (the breathing stops under
 * `prefers-reduced-motion`, see `app.css`). Decorative: the stretch has
 * nothing to read.
 */
export function InterludeDots({
  progress,
  className = "",
}: {
  /** How far into the interlude, 0 to 1. */
  progress: number;
  className?: string;
}) {
  return (
    <span aria-hidden="true" className={`wf-interlude ${className}`}>
      {[0, 1, 2].map((i) => {
        // Each dot owns a third of the stretch and lights across it.
        const lit = Math.min(1, Math.max(0, progress * 3 - i));
        return (
          <span
            key={i}
            className="wf-interlude__dot"
            style={{ opacity: 0.3 + 0.7 * lit }}
          />
        );
      })}
    </span>
  );
}
