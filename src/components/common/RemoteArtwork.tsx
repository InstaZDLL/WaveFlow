import { useEffect, useState } from "react";
import { ListMusic } from "lucide-react";
import { remoteArtwork } from "../../lib/tauri/remoteServer";

/**
 * A remote track's cover, fetched by hash as a `data:` URL. The artwork
 * endpoint is Bearer-only, so a bare `<img src>` pointed at it would 401 —
 * we fetch it once and cache the result in state. Falls back to a neutral
 * tile while it resolves or when there is no hash.
 *
 * Shared by the remote playlist view and the remote queue panel (RFC-005).
 */
export function RemoteArtwork({
  hash,
  className = "w-9 h-9",
  iconSize = 14,
}: {
  hash: string | null;
  className?: string;
  iconSize?: number;
}) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    if (!hash) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSrc(null);
      return;
    }
    let cancelled = false;
    remoteArtwork(hash)
      .then((url) => {
        if (!cancelled) setSrc(url);
      })
      .catch(() => {
        if (!cancelled) setSrc(null);
      });
    return () => {
      cancelled = true;
    };
  }, [hash]);
  if (!src) {
    return (
      <div
        className={`${className} rounded bg-zinc-200 dark:bg-zinc-700 flex items-center justify-center shrink-0`}
      >
        <ListMusic size={iconSize} className="text-zinc-400" />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      className={`${className} rounded object-cover shrink-0`}
      loading="lazy"
    />
  );
}
