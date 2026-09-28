import { useState, type CSSProperties } from "react";

import { usePlayer } from "../../hooks/usePlayer";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import type { LyricsWord } from "../../lib/tauri/lyrics";

/** How long a letter takes to come back to rest once the note ends. */
const RELEASE_MS = 350;
/** The shortest a letter's whole rise-hold-release may last. */
const MIN_LETTER_MS = 450;

/**
 * A held word, a letter at a time: each letter lifts, swells and glows as
 * the fill reaches it, stays up while the note is held, and every letter
 * comes back to rest together as the note ends — so the movement follows
 * the voice along the word instead of running ahead of it.
 *
 * Rendered in both layers of a karaoke word, the base and the fill, with
 * the same timing: a transform does not move the layout, so the fill's
 * clip stays over the letters it covers. The glow is on the base layer
 * (`glow`): the fill layer is clipped at the fill's edge, and a halo
 * drawn there is cut into a hard bar where the fill stops.
 *
 * CSS animations, one per letter, rather than a per-frame loop: the
 * browser runs them, and nothing re-renders while the note is held. The
 * delays are set once, from where the word was when it came on screen, so
 * arriving mid-word (a seek) joins the wave where it is. The wave pauses
 * with playback. Under reduced motion the word is plain text.
 */
export function HeldNoteText({
  word,
  glow = false,
}: {
  word: LyricsWord;
  glow?: boolean;
}) {
  const { positionMs, isPlaying } = usePlayer();
  const reduceMotion = usePrefersReducedMotion();
  // Fixed at mount: the component lives exactly as long as the word is
  // the one being sung, and re-reading the position every render would
  // shift the running animations by a quarter second at a time.
  const [elapsedMs] = useState(() => Math.max(0, positionMs - word.timeMs));

  const leading = word.text.match(/^\s*/)?.[0] ?? "";
  const trailing = word.text.match(/\s*$/)?.[0] ?? "";
  const core = word.text.slice(
    leading.length,
    word.text.length - trailing.length,
  );
  if (reduceMotion || core.length === 0) return <>{word.text}</>;

  const letters = Array.from(core);
  const heldMs = word.endMs - word.timeMs;
  // How much of the effect the note earns: a note barely long enough
  // stays subtle, one held three seconds and more gets all of it.
  const strength = Math.min(1, Math.max(0, (heldMs - 800) / 2700));

  return (
    <>
      {leading}
      {letters.map((letter, i) => {
        // A letter rises when the fill is halfway across it — the sweep
        // is linear over the word — and holds until the note ends, so
        // every letter's animation finishes at the same moment.
        const startMs = (heldMs * (i + 0.5)) / letters.length;
        const durationMs = Math.max(
          MIN_LETTER_MS,
          heldMs - startMs + RELEASE_MS,
        );
        return (
          <span
            key={i}
            className={`wf-held-letter${glow ? " wf-held-letter--glow" : ""}`}
            style={
              {
                animationDuration: `${durationMs}ms`,
                animationDelay: `${startMs - elapsedMs}ms`,
                animationPlayState: isPlaying ? "running" : "paused",
                "--held": strength.toFixed(2),
              } as CSSProperties
            }
          >
            {letter}
          </span>
        );
      })}
      {trailing}
    </>
  );
}
