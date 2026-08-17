import { useEffect, useState } from "react";
import { ListMusic } from "lucide-react";
import { remoteArtwork } from "../../lib/tauri/remoteServer";

/**
 * Process-wide cache of resolved `data:` URLs, keyed by artwork hash, plus
 * a registry of in-flight fetches. The artwork is content-addressed and
 * immutable, so a hash resolves to the same bytes forever — every remount
 * (scrolling a virtualized list, reopening a view) then reuses the cached
 * URL, and two components mounting the same hash at once share one fetch
 * instead of racing two.
 */
const artworkCache = new Map<string, string>();
const inFlight = new Map<string, Promise<string | null>>();

function loadArtwork(hash: string): Promise<string | null> {
  const cached = artworkCache.get(hash);
  if (cached) return Promise.resolve(cached);
  const pending = inFlight.get(hash);
  if (pending) return pending;
  const promise = remoteArtwork(hash)
    .then((url) => {
      artworkCache.set(hash, url);
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
 * A remote track's cover, fetched by hash as a `data:` URL. The artwork
 * endpoint is Bearer-only, so a bare `<img src>` pointed at it would 401 —
 * we fetch it once, cache the result process-wide (see {@link loadArtwork})
 * and reuse it across remounts. Falls back to a neutral tile while it
 * resolves or when there is no hash.
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
  useEffect(() => {
    if (!hash) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSrc(null);
      return;
    }
    const cached = artworkCache.get(hash);
    if (cached) {
      setSrc(cached);
      return;
    }
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
