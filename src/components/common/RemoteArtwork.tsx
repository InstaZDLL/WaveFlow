import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { ListMusic } from "lucide-react";
import { remoteArtwork } from "../../lib/tauri/remoteServer";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { useProfile } from "../../hooks/useProfile";

/**
 * Process-wide cache of resolved `asset://` URLs, keyed by artwork hash, plus
 * a registry of in-flight resolutions. The artwork is content-addressed and
 * immutable, so a hash resolves to the same bytes forever — every remount
 * (scrolling a virtualized list, reopening a view) then reuses the cached
 * URL, and two components mounting the same hash at once share one call
 * instead of racing two.
 *
 * The backend caches the bytes on disk and hands back a path, so this holds
 * short strings rather than base64 blobs, and a second launch resolves from
 * disk without touching the network.
 *
 * Keyed by profile as well as by hash. The cache directory is per-profile, so
 * the same hash resolves to a different file in each one — and while the image
 * behind it is the same (that is what content-addressing means), serving one
 * profile's path to another reaches into a directory that profile does not own
 * and breaks the moment its cache is cleared.
 */
/** Cap the resolved-artwork cache so a long session browsing many remote
 *  tracks can't grow it without bound. */
const ARTWORK_CACHE_CAPACITY = 256;
const artworkCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string | null>>();

/** The cache key. A bare hash would collide across profiles. */
function keyFor(profileId: number | null, hash: string): string {
  return `${profileId ?? "none"}:${hash}`;
}

/** LRU read: `Map` preserves insertion order, so re-inserting on a hit
 *  marks the entry most-recently-used. */
function cacheGet(key: string): string | undefined {
  const url = artworkCache.get(key);
  if (url !== undefined) {
    artworkCache.delete(key);
    artworkCache.set(key, url);
  }
  return url;
}

function cacheSet(key: string, url: string) {
  artworkCache.set(key, url);
  if (artworkCache.size > ARTWORK_CACHE_CAPACITY) {
    // Evict the least-recently-used entry (the oldest insertion).
    const oldest = artworkCache.keys().next().value;
    if (oldest !== undefined) artworkCache.delete(oldest);
  }
}

function loadArtwork(
  profileId: number | null,
  hash: string,
): Promise<string | null> {
  const key = keyFor(profileId, hash);
  const cached = cacheGet(key);
  if (cached !== undefined) return Promise.resolve(cached);
  const pending = inFlight.get(key);
  if (pending) return pending;
  const promise = remoteArtwork(hash)
    .then((path) => {
      // The backend answers with a local path; the asset protocol serves it
      // exactly like a scanned cover.
      const url = resolveArtwork({ full: path }, "full");
      if (url) cacheSet(key, url);
      inFlight.delete(key);
      return url;
    })
    .catch(() => {
      inFlight.delete(key);
      return null;
    });
  inFlight.set(key, promise);
  return promise;
}

/** Forget a resolution that turned out to be dead, so the next attempt asks
 *  the backend again instead of reusing the same broken path. */
function forget(profileId: number | null, hash: string) {
  artworkCache.delete(keyFor(profileId, hash));
}

/**
 * A remote track's cover, resolved by hash. The artwork endpoint is
 * Bearer-only, so a bare `<img src>` pointed at it would 401 — the backend
 * downloads it once into a per-profile disk cache and answers with a path,
 * which the asset protocol then serves. Resolved URLs are cached
 * process-wide (see {@link loadArtwork}) and reused across remounts. Falls
 * back to a neutral tile while it resolves or when there is no hash.
 *
 * Shared by the remote playlist view and the remote queue panel (RFC-005).
 */
export function RemoteArtwork({
  hash,
  className = "w-9 h-9 rounded",
  iconSize = 14,
}: {
  hash: string | null;
  className?: string;
  iconSize?: number;
}) {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  // Seed synchronously from the cache so a remount of an already-resolved
  // hash paints the cover on the first frame instead of flashing the tile.
  const [src, setSrc] = useState<string | null>(() =>
    hash ? (artworkCache.get(keyFor(profileId, hash)) ?? null) : null,
  );
  // Layout effect so a cached hash (or the reset below) updates `src`
  // synchronously before paint, avoiding a one-frame flash of the previous
  // cover when the hash changes.
  // What this component is showing right now. Every resolution stamps the
  // identity it was started for and drops its answer if that is no longer the
  // one on screen — the rows are virtualized, so a component outlives the
  // covers that pass through it.
  const currentKeyRef = useRef<string | null>(null);
  const retriedRef = useRef<string | null>(null);
  useLayoutEffect(() => {
    currentKeyRef.current = hash ? keyFor(profileId, hash) : null;
    // A different cover is a different episode: a tile recycled away from a
    // hash that had failed, and back to it, deserves its retry again.
    retriedRef.current = null;
    if (!hash) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSrc(null);
      return;
    }
    const cached = cacheGet(keyFor(profileId, hash));
    if (cached !== undefined) {
      setSrc(cached);
      return;
    }
    // Uncached new hash: clear the previous cover up front so a changed
    // hash can't keep showing the old artwork while the new one loads.
    setSrc(null);
    let cancelled = false;
    void loadArtwork(profileId, hash).then((url) => {
      if (!cancelled) setSrc(url);
    });
    return () => {
      cancelled = true;
    };
  }, [hash, profileId]);

  // The file can be evicted between resolving its path and painting it — the
  // disk cache has a cap and drops the least recently used. A dead
  // `asset://` would otherwise stay cached and keep failing, so drop it and
  // resolve once more; the second attempt re-downloads.
  //
  // Once per failure, not once per lifetime. The guard exists so a retry that
  // also fails cannot fire `onError` again and spin — against an unreachable
  // server every attempt fails and the tile is the honest answer. A load that
  // succeeds ends that episode, so a later eviction of the same cover is
  // allowed its own retry.
  const handleError = useCallback(() => {
    if (!hash) return;
    const key = keyFor(profileId, hash);
    if (retriedRef.current === key) {
      setSrc(null);
      return;
    }
    retriedRef.current = key;
    forget(profileId, hash);
    setSrc(null);
    void loadArtwork(profileId, hash).then((url) => {
      // Same guard as the effect above, and it belongs here too: this
      // resolution can outlive the cover it was started for.
      if (currentKeyRef.current !== key) return;
      setSrc(url);
    });
  }, [hash, profileId]);

  const handleLoad = useCallback(() => {
    retriedRef.current = null;
  }, []);
  if (!src) {
    return (
      <div
        className={`${className} bg-zinc-200 dark:bg-zinc-700 flex items-center justify-center shrink-0`}
      >
        <ListMusic size={iconSize} className="text-zinc-400" />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      onError={handleError}
      onLoad={handleLoad}
      className={`${className} object-cover shrink-0`}
      loading="lazy"
    />
  );
}
