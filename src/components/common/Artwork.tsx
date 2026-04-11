import { useMemo } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Disc } from "lucide-react";

interface ArtworkProps {
  /**
   * Absolute filesystem path returned by the backend (e.g. `list_tracks`
   * or `list_albums`). `null` renders the placeholder tile.
   */
  path: string | null;
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
 * Render a hash-addressed cover image served via Tauri's `asset://` protocol.
 *
 * `convertFileSrc` turns the absolute filesystem path into a URL the webview
 * can actually load — the scope allowlist in `tauri.conf.json` restricts
 * which paths are authorized. Paths outside `<appdata>/waveflow/profiles/**`
 * will return an opaque error; we don't try to catch it here because it
 * should never happen in practice.
 */
export function Artwork({
  path,
  className = "w-10 h-10",
  iconSize = 18,
  alt,
  rounded = "lg",
}: ArtworkProps) {
  const src = useMemo(() => (path ? convertFileSrc(path) : null), [path]);
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
