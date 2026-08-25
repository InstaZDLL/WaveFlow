import { useLayoutEffect, useState } from "react";
import { ListMusic } from "lucide-react";
import { remoteArtwork } from "../../lib/tauri/remoteServer";
import { resolveArtwork } from "../../lib/tauri/artwork";

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
 */
/** Cap the resolved-artwork cache so a long session browsing many remote
 *  tracks can't grow it without bound. */
const ARTWORK_CACHE_CAPACITY = 256;
const artworkCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string | null>>();

/** LRU read: `Map` preserves insertion order, so re-inserting on a hit
 *  marks the entry most-recently-used. */
function cacheGet(hash: string): string | undefined {
  const url = artworkCache.get(hash);
  if (url !== undefined) {
    artworkCache.delete(hash);
    artworkCache.set(hash, url);
  }
  return url;
}

function cacheSet(hash: string, url: string) {
  artworkCache.set(hash, url);
  if (artworkCache.size > ARTWORK_CACHE_CAPACITY) {
    // Evict the least-recently-used entry (the oldest insertion).
    const oldest = artworkCache.keys().next().value;
    if (oldest !== undefined) artworkCache.delete(oldest);
  }
}

function loadArtwork(hash: string): Promise<string | null> {
  const cached = cacheGet(hash);
  if (cached !== undefined) return Promise.resolve(cached);
  const pending = inFlight.get(hash);
  if (pending) return pending;
  const promise = remoteArtwork(hash)
    .then((path) => {
      // The backend answers with a local path; the asset protocol serves it
      // exactly like a scanned cover.
      const url = resolveArtwork({ full: path }, "full");
      if (url) cacheSet(hash, url);
      inFlight.delete(hash);
      return url;
    })
    .catch(() => {
      inFlight.delete(hash);
      return null;
    });
  inFlight.set(hash, promise);
  return promise;
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
  // Seed synchronously from the cache so a remount of an already-resolved
  // hash paints the cover on the first frame instead of flashing the tile.
  const [src, setSrc] = useState<string | null>(() =>
    hash ? (artworkCache.get(hash) ?? null) : null,
  );
  // Layout effect so a cached hash (or the reset below) updates `src`
  // synchronously before paint, avoiding a one-frame flash of the previous
  // cover when the hash changes.
  useLayoutEffect(() => {
    if (!hash) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSrc(null);
      return;
    }
    const cached = cacheGet(hash);
    if (cached !== undefined) {
      setSrc(cached);
      return;
    }
    // Uncached new hash: clear the previous cover up front so a changed
    // hash can't keep showing the old artwork while the new one loads.
    setSrc(null);
    let cancelled = false;
    void loadArtwork(hash).then((url) => {
      if (!cancelled) setSrc(url);
    });
    return () => {
      cancelled = true;
    };
  }, [hash]);
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
      className={`${className} object-cover shrink-0`}
      loading="lazy"
    />
  );
}
