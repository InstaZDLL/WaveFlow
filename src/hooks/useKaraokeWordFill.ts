import { useCallback, useEffect, useRef } from "react";
import { usePlayer } from "./usePlayer";
import { usePrefersReducedMotion } from "./usePrefersReducedMotion";
import type { LyricsWord } from "../lib/tauri/lyrics";

/** CSS custom property the fill layer's `clip-path` reads. */
const FILL_VAR = "--kw-fill";

/** Monotonic clock, guarded for non-browser hosts (tests). */
function now(): number {
  return typeof performance !== "undefined" ? performance.now() : 0;
}

/**
 * Drives the progressive sweep across the active karaoke word (issue
 * #491) — a continuous fill rather than the per-word step the column
 * used before.
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
  // The point the loop extrapolates from: a position, the moment we
  // adopted it, and the speed in force from that moment on. `at` is
  // seeded on the first render so the very first frame doesn't measure
  // the whole page lifetime as elapsed playback.
  const anchorRef = useRef({ positionMs, at: now(), speed: playbackSpeed });
  // What the last position *event* carried, so we can tell "the backend
  // told us something new" from "only the speed or play state changed".
  const lastEventPositionRef = useRef(positionMs);
  const wasPlayingRef = useRef(isPlaying);

  useEffect(() => {
    const at = now();
    const previous = anchorRef.current;
    const fromEvent = positionMs !== lastEventPositionRef.current;
    lastEventPositionRef.current = positionMs;
    // Where the fill had actually reached, advanced with the speed that
    // was in force — not the one that just took effect.
    const carried =
      previous.positionMs +
      (wasPlayingRef.current
        ? Math.max(0, at - previous.at) * previous.speed
        : 0);
    wasPlayingRef.current = isPlaying;
    anchorRef.current = {
      // A fresh position from the backend is authoritative. Otherwise the
      // trigger was a speed change or a play/pause, and re-anchoring on
      // `positionMs` would discard up to 250 ms of already-extrapolated
      // progress — the fill would visibly jump backwards on every pause
      // and every speed change. Carrying the extrapolated value over also
      // means a resume excludes the paused time for free: `at` is reset
      // here, so `elapsed` restarts at zero.
      positionMs: fromEvent ? positionMs : carried,
      at,
      speed: playbackSpeed,
    };
  }, [positionMs, isPlaying, playbackSpeed]);

  const start = word?.timeMs ?? -1;
  const end = word?.endMs ?? -1;
  // A word needs a real, forward-going, *finite* span to sweep across.
  // `endMs` is normally filled in by `fillLineAndWordEnds`, but the last
  // word of the last line can stay -1, a sloppy source can stamp two
  // words at the same millisecond, and `LyricsWord`'s contract allows
  // `+∞` for a final word — which passes `end > start` while making every
  // ratio 0, so the word would never light up at all.
  const hasSpan =
    Number.isFinite(start) && Number.isFinite(end) && start >= 0 && end > start;

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
      // Only extrapolate while actually playing: paused, the fill must
      // hold where it is instead of drifting to the end of the word.
      const elapsed = isPlaying ? Math.max(0, now() - anchor.at) : 0;
      // The anchor's own speed, not the live one — they differ for the
      // frames between a speed change and the next position event.
      const estimated = anchor.positionMs + elapsed * anchor.speed;
      const ratio = (estimated - start) / (end - start);
      const clamped = ratio <= 0 ? 0 : ratio >= 1 ? 1 : ratio;
      el.style.setProperty(FILL_VAR, `${(clamped * 100).toFixed(2)}%`);
      // Stop at a full word: there is nothing left to move, and a long
      // word would otherwise hold a frame callback for its whole tail.
      // Safe because `positionMs` is a dependency of this effect — a seek
      // backwards inside the same word re-runs it and repaints from the
      // new anchor, which is what stopping here used to break.
      if (isPlaying && clamped < 1) raf = requestAnimationFrame(paint);
    };
    paint();

    return () => {
      if (raf) cancelAnimationFrame(raf);
      // Hand the element back in a neutral state — it may be reused for
      // a different word before React drops it.
      el.style.removeProperty(FILL_VAR);
    };
    // `positionMs` is a dependency on purpose. It only changes at 4 Hz, so
    // re-running this is cheap, and it's what repaints after a seek while
    // *paused* — no loop is running then, and the anchor effect above
    // updates a ref, which by itself repaints nothing.
  }, [start, end, hasSpan, isPlaying, playbackSpeed, reduceMotion, positionMs]);

  return useCallback((el: HTMLElement | null) => {
    elRef.current = el;
  }, []);
}
