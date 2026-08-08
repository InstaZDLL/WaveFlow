import { useCallback, useEffect, useRef } from "react";
import { usePlayer } from "./usePlayer";
import { usePrefersReducedMotion } from "./usePrefersReducedMotion";
import type { LyricsWord } from "../lib/tauri/lyrics";

/** CSS custom property the fill layer's `clip-path` reads. */
const FILL_VAR = "--kw-fill";

/**
 * Drives the progressive sweep across the active karaoke word (issue
 * #491) — Apple Music's continuous fill rather than the per-word step
 * the column used before.
 *
 * Two problems this solves, both invisible in a naive implementation:
 *
 * 1. **The position only arrives at 4 Hz.** The decoder throttles
 *    `player:position` to one event per 250 ms
 *    ([`POSITION_EMIT_INTERVAL`](../../src-tauri/crates/app/src/audio/decoder.rs)),
 *    so painting straight from `positionMs` would advance the fill in
 *    250 ms steps — worse than the old discrete highlight, not better.
 *    Each event becomes an *anchor* (position + the `performance.now()`
 *    at which we saw it) and every frame extrapolates from it, scaled by
 *    `playbackSpeed` so the sweep still tracks at 0.5× or 2×.
 * 2. **A frame-rate React update would re-render the world.**
 *    `useTrackLyrics` is shared by the immersive column and the side
 *    panel, and the column renders every line — so `setState` per frame
 *    would re-render both trees ~60 times a second. Nothing here touches
 *    React state: the loop writes a CSS variable straight onto the one
 *    element it owns, and the browser handles the rest.
 *
 * Returns a ref callback to attach to the active word's element **only**.
 * Attaching it to a word means "this one is being sung"; React detaches
 * it as the active word moves on, which restarts the loop against the
 * new bounds.
 *
 * Falls back to no fill (the caller keeps its discrete styling) when
 * motion is reduced, when the word has no usable duration, or when
 * playback is paused mid-word.
 */
export function useKaraokeWordFill(word: LyricsWord | null | undefined) {
  const { positionMs, isPlaying, playbackSpeed } = usePlayer();
  const reduceMotion = usePrefersReducedMotion();

  const elRef = useRef<HTMLElement | null>(null);
  // Latest position event + when it landed, so a frame can estimate
  // "now" between two events instead of waiting for the next one.
  const anchorRef = useRef({ positionMs, at: 0 });

  useEffect(() => {
    anchorRef.current = {
      positionMs,
      at: typeof performance !== "undefined" ? performance.now() : 0,
    };
  }, [positionMs]);

  const start = word?.timeMs ?? -1;
  const end = word?.endMs ?? -1;
  // A word needs a real, forward-going span to sweep across. `endMs` is
  // normally filled in by `fillLineAndWordEnds`, but the last word of the
  // last line can stay -1, and a sloppy source can stamp two words at the
  // same millisecond.
  const hasSpan = start >= 0 && end > start;

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    if (reduceMotion || !hasSpan) {
      // Leave the variable unset so the fill layer stays collapsed and
      // the caller's discrete styling shows through unchanged.
      el.style.removeProperty(FILL_VAR);
      return;
    }

    let raf = 0;
    const paint = () => {
      const anchor = anchorRef.current;
      const now = typeof performance !== "undefined" ? performance.now() : 0;
      // Only extrapolate while actually playing: paused, the fill must
      // hold where it is instead of drifting to the end of the word.
      const elapsed = isPlaying ? Math.max(0, now - anchor.at) : 0;
      const estimated = anchor.positionMs + elapsed * playbackSpeed;
      const ratio = (estimated - start) / (end - start);
      const clamped = ratio <= 0 ? 0 : ratio >= 1 ? 1 : ratio;
      el.style.setProperty(FILL_VAR, `${(clamped * 100).toFixed(2)}%`);
      // Once the word is full there is nothing left to animate; the next
      // word remounts this ref and starts its own loop.
      if (clamped < 1 && isPlaying) raf = requestAnimationFrame(paint);
    };
    paint();

    return () => {
      if (raf) cancelAnimationFrame(raf);
      // Hand the element back in a neutral state — it may be reused for
      // a different word before React drops it.
      el.style.removeProperty(FILL_VAR);
    };
  }, [start, end, hasSpan, isPlaying, playbackSpeed, reduceMotion]);

  return useCallback((el: HTMLElement | null) => {
    elRef.current = el;
  }, []);
}
