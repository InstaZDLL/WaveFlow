import { useEffect, useState, useSyncExternalStore } from "react";

import {
  fetchAlbumMotionArtwork,
  type MotionArtwork,
} from "../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "./usePluginAvailability";

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
 *
 * A **`null` answer is remembered too**, and that is what made a miss
 * permanent: installing the metadata plugin afterwards, enabling it, or
 * setting a manual `.mp4` from the album page all leave the "this album has
 * none" entry in place, so the album stayed static until the app restarted
 * (#615). Hence the invalidation below — and a generation counter, because
 * dropping the entry is not enough on its own: the surfaces that show the
 * artwork are already mounted, and their effect only re-runs when something
 * in its dependencies changes.
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

/**
 * Bumped by every invalidation. Mounted hooks read it through
 * `useSyncExternalStore` and re-look-up when it moves.
 */
let generation = 0;
const subscribers = new Set<() => void>();

function subscribe(onStoreChange: () => void): () => void {
  subscribers.add(onStoreChange);
  return () => {
    subscribers.delete(onStoreChange);
  };
}

function getGeneration(): number {
  return generation;
}

function bumpGeneration(): void {
  generation += 1;
  for (const notify of subscribers) notify();
}

/**
 * Forget what we know about one album, so the next render looks it up
 * again. Call this after changing what the answer would be — setting or
 * clearing a manual motion cover.
 *
 * Takes the album id alone because that is all the picker has; the cache
 * key ends with it, so every entry for that album is dropped whatever
 * artist/album text it was keyed under.
 */
export function invalidateAlbumMotionArtwork(
  albumId: number | null | undefined,
): void {
  if (albumId == null) return;
  const suffix = `\0${albumId}`;
  for (const key of [...resolved.keys()]) {
    if (key.endsWith(suffix)) resolved.delete(key);
  }
  for (const key of [...inFlight.keys()]) {
    if (key.endsWith(suffix)) inFlight.delete(key);
  }
  bumpGeneration();
}

/** Forget everything: the set of plugins that can answer just changed. */
export function invalidateAllMotionArtwork(): void {
  resolved.clear();
  inFlight.clear();
  bumpGeneration();
}

// Installing, enabling, disabling or removing a plugin changes who can
// answer, so every remembered answer — above all the `null`s — is stale.
// This is the same bus the sidebar and the Web Radio view already refresh
// from, so all four paths are covered without a second mechanism.
if (typeof window !== "undefined") {
  window.addEventListener(PLUGIN_AVAILABILITY_EVENT, () => {
    invalidateAllMotionArtwork();
  });
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

  // The generation this lookup starts in. Its answer describes the world
  // as it was then, so an invalidation landing before it resolves makes it
  // stale — and remembering it would put back the very entry the
  // invalidation removed. Any invalidation counts, not just this album's:
  // that costs a re-fetch in the rare case, and a per-key counter would
  // buy nothing but bookkeeping.
  const startedAt = generation;
  const request = fetchAlbumMotionArtwork(artist, album, albumId ?? null)
    .then((motion) => {
      if (generation === startedAt) rememberResolved(key, motion);
      return motion;
    })
    // A failed lookup is NOT remembered: it's usually transient (offline,
    // rate limit), and caching it would suppress retries for the rest of
    // the session.
    .catch(() => null)
    .finally(() => {
      // Only our own entry: an invalidation drops it, and the re-render it
      // triggers can already have installed a newer request under the same
      // key — deleting that one would send the next caller off on a third
      // fetch while the second is still running.
      if (inFlight.get(key) === request) inFlight.delete(key);
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
 *
 * Re-runs on every invalidation (#615): an album whose answer was `null`
 * picks its artwork up as soon as a plugin is installed or a manual file is
 * set, rather than at the next restart.
 */
export function useAlbumMotionArtwork(
  artist: string | null | undefined,
  album: string | null | undefined,
  albumId?: number | null,
): MotionArtwork | null {
  const [motion, setMotion] = useState<MotionArtwork | null>(null);
  const cacheGeneration = useSyncExternalStore(subscribe, getGeneration);

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
    // `cacheGeneration` is a dependency rather than a value this effect
    // reads: it is what makes an invalidation re-run the lookup.
  }, [artist, album, albumId, cacheGeneration]);

  return motion;
}
