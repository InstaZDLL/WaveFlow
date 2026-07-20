import { useEffect, useState } from "react";

import {
  fetchAlbumMotionArtwork,
  type MotionArtwork,
} from "../lib/tauri/plugins";

/**
 * Process-wide dedupe for motion-artwork lookups.
 *
 * The hook is mounted by several surfaces at once (ImmersiveNowPlaying +
 * NowPlayingPanel), so a single track change fired three to four identical
 * `fetch_album_motion_artwork` calls for the same album — each one a full
 * fan-out to every enabled metadata plugin, taking that plugin's lock and
 * (cold) hitting Apple's API. `inFlight` collapses concurrent callers onto
 * one promise; `resolved` keeps the answer so remounting a panel, or
 * flipping back to a recent album, costs nothing.
 *
 * `resolved` is capped and evicted oldest-first: a long listening session
 * would otherwise retain an entry per album played. The backend has its own
 * caches, so a rare re-fetch after eviction is cheap.
 */
const MAX_RESOLVED = 64;
const inFlight = new Map<string, Promise<MotionArtwork | null>>();
const resolved = new Map<string, MotionArtwork | null>();

/** `\0` can't appear in a tag value, so it can't forge a collision.
 *  `albumId` rides along so a manual override (issue #408) — which is
 *  resolved by id, not text — gets its own cache entry rather than
 *  reusing one keyed only by a name that could collide across albums. */
function cacheKey(
  artist: string,
  album: string,
  albumId: number | null | undefined,
): string {
  return `${artist}\0${album}\0${albumId ?? ""}`;
}

function rememberResolved(key: string, value: MotionArtwork | null): void {
  // Re-insert to refresh insertion order, then evict from the front.
  resolved.delete(key);
  resolved.set(key, value);
  while (resolved.size > MAX_RESOLVED) {
    const oldest = resolved.keys().next();
    if (oldest.done) break;
    resolved.delete(oldest.value);
  }
}

function lookup(
  artist: string,
  album: string,
  albumId: number | null | undefined,
): Promise<MotionArtwork | null> {
  const key = cacheKey(artist, album, albumId);
  if (resolved.has(key)) {
    return Promise.resolve(resolved.get(key) ?? null);
  }
  const pending = inFlight.get(key);
  if (pending) return pending;

  const request = fetchAlbumMotionArtwork(artist, album, albumId ?? null)
    .then((motion) => {
      rememberResolved(key, motion);
      return motion;
    })
    // A failed lookup is NOT remembered: it's usually transient (offline,
    // rate limit), and caching it would suppress retries for the rest of
    // the session.
    .catch(() => null)
    .finally(() => {
      inFlight.delete(key);
    });

  inFlight.set(key, request);
  return request;
}

/**
 * Resolve animated album artwork for `(artist, album)` via enabled
 * metadata plugins (Phase 3). Returns `null` when the inputs are missing
 * (e.g. a radio stream with no album), when offline, when no metadata
 * plugin is installed, or when none has motion artwork for the album —
 * callers render the static cover in that case.
 *
 * setState only fires inside the promise callbacks (never synchronously in
 * the effect body — `react-hooks/set-state-in-effect`), and a `cancelled`
 * guard drops a stale in-flight result when the track changes fast.
 */
export function useAlbumMotionArtwork(
  artist: string | null | undefined,
  album: string | null | undefined,
  albumId?: number | null,
): MotionArtwork | null {
  const [motion, setMotion] = useState<MotionArtwork | null>(null);

  useEffect(() => {
    let cancelled = false;
    const apply = (m: MotionArtwork | null) => {
      if (!cancelled) setMotion(m);
    };
    // Clear the previous track's artwork right away so a stale overlay
    // never lingers over the new cover. Goes through a resolved promise
    // (not a synchronous setState in the effect body) to satisfy
    // `react-hooks/set-state-in-effect`, and runs before any fetch
    // resolves, so the new artwork only ever replaces `null`.
    Promise.resolve<MotionArtwork | null>(null).then(apply);
    if (artist && album) {
      lookup(artist, album, albumId).then(apply, () => apply(null));
    }
    return () => {
      cancelled = true;
    };
  }, [artist, album, albumId]);

  return motion;
}
