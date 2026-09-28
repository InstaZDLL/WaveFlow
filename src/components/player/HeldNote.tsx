import { useState, type CSSProperties } from "react";

import { usePlayer } from "../../hooks/usePlayer";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import type { LyricsWord } from "../../lib/tauri/lyrics";

/**
 * A held word, a letter at a time: each letter lifts, swells and — on the
 * sung layer — glows in turn, so the movement travels along the word
 * while it is held instead of the whole word pulsing.
 *
 * Rendered in both layers of a karaoke word, the base and the fill, with
 * the same timing: a transform does not move the layout, so the fill's
 * clip stays over the letters it covers. `glow` marks the fill layer.
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
  // The wave crosses the word over the first two thirds of the hold, and
  // each letter's swell lasts half of it: the last letter settles as the
  // note ends.
  const stepMs = (heldMs * 0.66) / letters.length;
  const swellMs = Math.max(450, heldMs * 0.5);
  // How much of the effect the note earns: a note barely long enough
  // stays subtle, one held three seconds and more gets all of it.
  const strength = Math.min(1, Math.max(0, (heldMs - 800) / 2700));

  return (
    <>
      {leading}
      {letters.map((letter, i) => (
        <span
          key={i}
          className={`wf-held-letter${glow ? " wf-held-letter--glow" : ""}`}
          style={
            {
              animationDuration: `${swellMs}ms`,
              animationDelay: `${i * stepMs - elapsedMs}ms`,
              animationPlayState: isPlaying ? "running" : "paused",
              "--held": strength.toFixed(2),
            } as CSSProperties
          }
        >
          {letter}
        </span>
      ))}
      {trailing}
    </>
  );
}
