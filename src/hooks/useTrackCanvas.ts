import { useEffect, useState, useSyncExternalStore } from "react";

import { getTrackCanvas } from "../lib/tauri/canvas";

/**
 * Process-wide dedupe + cache for per-track Canvas lookups (issue #442),
 * mirroring `useAlbumMotionArtwork`.
 *
 * The hook is mounted by several surfaces at once (the immersive top bar
 * decides toggle visibility, `ImmersiveNowPlaying` renders the stage,
 * `NowPlayingPanel` does both), so a single track change would otherwise
 * fire the same `get_track_canvas` query three or four times. `inFlight`
 * collapses concurrent callers onto one promise; `resolved` keeps the
 * answer so remounting a panel or flipping back to a recent track is free.
 *
 * `resolved` is capped and evicted oldest-first so a long session doesn't
 * retain an entry per track played.
 */
const MAX_RESOLVED = 128;
const inFlight = new Map<number, Promise<string | null>>();
const resolved = new Map<number, string | null>();

function rememberResolved(trackId: number, value: string | null): void {
  resolved.delete(trackId);
  resolved.set(trackId, value);
  while (resolved.size > MAX_RESOLVED) {
    const oldest = resolved.keys().next();
    if (oldest.done) break;
    resolved.delete(oldest.value);
  }
}

// Per-trackId generation, bumped by `invalidateTrackCanvas`. A request
// captures the generation live at creation time; if a set/clear bumps it
// while the request is in flight, that request's result is stale and must
// neither be cached nor allowed to clear a newer request's inFlight entry.
const generation = new Map<number, number>();
function generationOf(trackId: number): number {
  return generation.get(trackId) ?? 0;
}

function lookup(trackId: number): Promise<string | null> {
  if (resolved.has(trackId)) {
    return Promise.resolve(resolved.get(trackId) ?? null);
  }
  const pending = inFlight.get(trackId);
  if (pending) return pending;

  const myGeneration = generationOf(trackId);
  const request: Promise<string | null> = getTrackCanvas(trackId)
    .then((canvas) => {
      const path = canvas?.localPath ?? null;
      // Only cache when this request is still the current generation — an
      // invalidation (set/clear) since it started means the answer is stale.
      if (generationOf(trackId) === myGeneration) rememberResolved(trackId, path);
      return path;
    })
    // A failed lookup is NOT remembered so a transient error doesn't
    // suppress the Canvas for the rest of the session.
    .catch(() => null)
    .finally(() => {
      // Only clear inFlight if we're still the registered request: an
      // invalidation may have replaced us with a newer one, which we must
      // not delete.
      if (inFlight.get(trackId) === request) inFlight.delete(trackId);
    });

  inFlight.set(trackId, request);
  return request;
}

// Invalidation signal: bumped whenever a Canvas is set/cleared so every
// mounted `useTrackCanvas` re-resolves, even for the same trackId (its
// effect deps wouldn't otherwise change). A plain epoch + listener set,
// read through `useSyncExternalStore`.
let epoch = 0;
const epochListeners = new Set<() => void>();
function subscribeEpoch(cb: () => void): () => void {
  epochListeners.add(cb);
  return () => epochListeners.delete(cb);
}
function getEpoch(): number {
  return epoch;
}

/**
 * Invalidate a track's cached Canvas path — call after setting or clearing
 * one so the mounted surfaces re-resolve instead of serving the stale
 * answer. Drops the cache entry and bumps the shared epoch to re-trigger
 * every `useTrackCanvas` effect.
 */
export function invalidateTrackCanvas(trackId: number): void {
  resolved.delete(trackId);
  inFlight.delete(trackId);
  // Bump the generation so any request already in flight for this track is
  // treated as stale when it resolves (won't overwrite the fresh answer).
  generation.set(trackId, generationOf(trackId) + 1);
  epoch += 1;
  for (const cb of epochListeners) cb();
}

/**
 * Resolve a track's Canvas clip local path, or `null` when it has none
 * (or the id is missing). setState only fires inside the promise callbacks
 * (never synchronously in the effect body — `react-hooks/set-state-in-effect`),
 * and a `cancelled` guard drops a stale in-flight result on a fast track
 * change.
 */
export function useTrackCanvas(
  trackId: number | null | undefined,
): string | null {
  // Store the resolved path together with the id it belongs to, so the
  // render can gate on a match below — a bare path would flash the previous
  // track's Canvas for one render after `trackId` changes but before the
  // effect resolves.
  const [resolved, setResolved] = useState<{
    id: number;
    path: string | null;
  } | null>(null);
  // Re-run the effect when a set/clear bumps the epoch, even for the same
  // trackId — otherwise the surface would keep the stale answer.
  const currentEpoch = useSyncExternalStore(subscribeEpoch, getEpoch, getEpoch);

  useEffect(() => {
    let cancelled = false;
    const apply = (p: string | null) => {
      if (!cancelled) setResolved({ id: trackId as number, path: p });
    };
    if (trackId != null && trackId >= 0) {
      lookup(trackId).then(apply, () => apply(null));
    }
    return () => {
      cancelled = true;
    };
    // `currentEpoch` is a deliberate dep: a bump forces a re-resolve.
  }, [trackId, currentEpoch]);

  // Only surface the path when it belongs to the currently-requested track;
  // a mismatch (track just changed, effect not resolved yet) reads as null,
  // so the previous track's clip never bleeds onto the new one.
  return resolved && resolved.id === trackId ? resolved.path : null;
}
