import { useEffect, useState } from "react";

const ROUND: Record<"md" | "lg" | "xl" | "2xl", string> = {
  md: "rounded-md",
  lg: "rounded-lg",
  xl: "rounded-xl",
  "2xl": "rounded-2xl",
};

/** How long each image holds before crossfading to the other. */
const DEFAULT_INTERVAL_MS = 20_000;

/**
 * Cover ↔ artist crossfade slideshow (issue #466). Drop it as a sibling of
 * an `<Artwork>` inside a `relative` container: when enabled and an artist
 * photo exists, the artist image fades in over the static cover, holds, then
 * fades back out — a gentle living backdrop built from images already in the
 * library, no plugin.
 *
 * A single artist overlay (rather than two stacked images) IS the crossfade:
 * the cover shows through whenever the overlay's opacity is 0, so the CSS
 * opacity transition reads as cover → artist → cover. It starts on the cover
 * (overlay hidden) so the album art is always the first thing seen.
 *
 * The surface owns the gate (`enabled` already folds in the global toggle,
 * `prefers-reduced-motion`, and the Canvas/motion-cover precedence), so this
 * only renders the animation itself. Decorative (`aria-hidden`) — the
 * accessible name lives on the `<Artwork>` underneath. A load error silently
 * falls back to the static cover.
 */
export function CoverSlideshow({
  artistSrc,
  enabled,
  rounded = "2xl",
  className,
  intervalMs = DEFAULT_INTERVAL_MS,
}: {
  /** Resolved artist photo src (from `useArtistImage` / panel enrichment). */
  artistSrc: string | null;
  /** Global toggle AND reduced-motion AND Canvas/motion precedence, resolved
   *  by the surface. When false the slideshow renders nothing. */
  enabled: boolean;
  rounded?: "md" | "lg" | "xl" | "2xl";
  className?: string;
  intervalMs?: number;
}) {
  if (!enabled || !artistSrc) return null;
  // Key on the src so a new artist restarts from the cover.
  return (
    <SlideshowLayer
      key={artistSrc}
      src={artistSrc}
      rounded={rounded}
      className={className}
      intervalMs={intervalMs}
    />
  );
}

function SlideshowLayer({
  src,
  rounded,
  className,
  intervalMs,
}: {
  src: string;
  rounded: "md" | "lg" | "xl" | "2xl";
  className?: string;
  intervalMs: number;
}) {
  const [showArtist, setShowArtist] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const id = setInterval(() => setShowArtist((v) => !v), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);

  if (failed) return null;

  return (
    <img
      src={src}
      alt=""
      aria-hidden="true"
      onError={() => setFailed(true)}
      className={`pointer-events-none absolute inset-0 w-full h-full object-cover ${ROUND[rounded]} transition-opacity duration-1000 ${showArtist ? "opacity-100" : "opacity-0"} ${className ?? ""}`}
    />
  );
}
