import { useMemo } from "react";
import { Disc } from "lucide-react";
import { resolveArtwork, type ArtworkSize } from "../../lib/tauri/artwork";

interface ArtworkProps {
  /**
   * Absolute filesystem path returned by the backend (e.g. `list_tracks`
   * or `list_albums`). `null` renders the placeholder tile.
   */
  path: string | null;
  /** Pre-resized 64×64 variant from the thumbnails pipeline. */
  path1x?: string | null;
  /** Pre-resized 128×128 variant from the thumbnails pipeline. */
  path2x?: string | null;
  /**
   * Optional remote URL fallback (e.g. Deezer CDN) used only when no
   * local variant is available — typical of an artist whose enrichment
   * just landed but whose download hasn't completed yet.
   */
  remoteUrl?: string | null;
  /** Display size hint. Defaults to `2x` (grid tile). */
  size?: ArtworkSize;
  /** Tailwind sizing classes applied to the wrapper. */
  className?: string;
  /** Size of the fallback icon, in pixels. */
  iconSize?: number;
  /**
   * Accessible label for screen readers. Pass the album title or track
   * title so the image is announced meaningfully.
   */
  alt?: string;
  /** Border radius preset. Defaults to `lg` for in-row thumbnails. */
  rounded?: "md" | "lg" | "xl" | "2xl";
}

/**
 * Render a hash-addressed cover image served via Tauri's `asset://`
 * protocol, picking the closest pre-resized variant for the requested
 * display `size`.
 */
export function Artwork({
  path,
  path1x,
  path2x,
  remoteUrl,
  size = "2x",
  className = "w-10 h-10",
  iconSize = 18,
  alt,
  rounded = "lg",
}: ArtworkProps) {
  const src = useMemo(
    () =>
      resolveArtwork(
        {
          full: path,
          x1: path1x ?? null,
          x2: path2x ?? null,
          remoteUrl: remoteUrl ?? null,
        },
        size,
      ),
    [path, path1x, path2x, remoteUrl, size],
  );
  const radiusClass = {
    md: "rounded-md",
    lg: "rounded-lg",
    xl: "rounded-xl",
    "2xl": "rounded-2xl",
  }[rounded];

  if (!src) {
    return (
      <div
        className={`${className} ${radiusClass} bg-linear-to-br from-emerald-100 to-emerald-200 dark:from-emerald-900/40 dark:to-emerald-800/30 border border-emerald-200/60 dark:border-emerald-800/40 flex items-center justify-center overflow-hidden shrink-0`}
        aria-hidden={alt ? undefined : true}
        aria-label={alt}
        role={alt ? "img" : undefined}
      >
        <Disc
          size={iconSize}
          className="text-emerald-500/70 dark:text-emerald-400/60"
        />
      </div>
    );
  }

  return (
    <img
      src={src}
      alt={alt ?? ""}
      loading="lazy"
      decoding="async"
      draggable={false}
      className={`${className} ${radiusClass} object-cover shrink-0 bg-zinc-100 dark:bg-zinc-800`}
    />
  );
}
