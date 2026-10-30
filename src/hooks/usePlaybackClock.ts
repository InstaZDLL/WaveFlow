import { useCallback, useEffect, useRef } from "react";
import { usePlayer } from "./usePlayer";
import { usePlayerPosition } from "./usePlayerPosition";

/** Monotonic clock, guarded for non-browser hosts (tests). */
function now(): number {
  return typeof performance !== "undefined" ? performance.now() : 0;
}

/**
 * The playback position between two position events.
 *
 * The decoder throttles `player:position` to one event per 250 ms
 * ([`POSITION_EMIT_INTERVAL`](../../src-tauri/crates/app/src/audio/decoder/mod.rs)),
 * so reading `positionMs` straight is up to a quarter of a second behind.
 * Each event becomes an *anchor* (position + the `performance.now()` at
 * which we saw it), and `estimate()` extrapolates from it, scaled by
 * `playbackSpeed` so it still tracks at 0.5× or 2×. Paused, it holds.
 *
 * `estimate` is stable and reads refs only, so a frame loop or a key
 * handler can call it without re-rendering anything. `positionMs` and
 * `isPlaying` are returned for the effects that must re-run when the
 * backend says something new — a seek while paused moves no clock.
 */
export function usePlaybackClock() {
  const { isPlaying, playbackSpeed } = usePlayer();
  const positionMs = usePlayerPosition();

  // The point `estimate` extrapolates from: a position, the moment we
  // adopted it, and the speed and play state in force from that moment
  // on. `at` is seeded on the first render so the very first estimate
  // doesn't measure the whole page lifetime as elapsed playback.
  const anchorRef = useRef({
    positionMs,
    at: now(),
    speed: playbackSpeed,
    playing: isPlaying,
  });
  // What the last position *event* carried, so we can tell "the backend
  // told us something new" from "only the speed or play state changed".
  const lastEventPositionRef = useRef(positionMs);

  useEffect(() => {
    const at = now();
    const previous = anchorRef.current;
    const fromEvent = positionMs !== lastEventPositionRef.current;
    lastEventPositionRef.current = positionMs;
    // Where playback had actually reached, advanced with the speed that
    // was in force — not the one that just took effect.
    const carried =
      previous.positionMs +
      (previous.playing ? Math.max(0, at - previous.at) * previous.speed : 0);
    anchorRef.current = {
      // A fresh position from the backend is authoritative. Otherwise the
      // trigger was a speed change or a play/pause, and re-anchoring on
      // `positionMs` would discard up to 250 ms of already-extrapolated
      // progress — a karaoke fill would visibly jump backwards on every
      // pause and every speed change. Carrying the extrapolated value over
      // also means a resume excludes the paused time for free: `at` is
      // reset here, so the elapsed time restarts at zero.
      positionMs: fromEvent ? positionMs : carried,
      at,
      speed: playbackSpeed,
      playing: isPlaying,
    };
  }, [positionMs, isPlaying, playbackSpeed]);

  const estimate = useCallback(() => {
    const anchor = anchorRef.current;
    // Only extrapolate while actually playing: paused, the position must
    // hold where it is instead of drifting on.
    const elapsed = anchor.playing ? Math.max(0, now() - anchor.at) : 0;
    // The anchor's own speed, not the live one — they differ between a
    // speed change and the next position event.
    return anchor.positionMs + elapsed * anchor.speed;
  }, []);

  return { estimate, positionMs, isPlaying, playbackSpeed };
}
