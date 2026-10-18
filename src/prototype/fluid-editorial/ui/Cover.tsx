import { useState } from "react";
import { fallbackGradient } from "../color";

interface CoverProps {
  src: string | null;
  palette: string[] | null;
  alt: string;
  className?: string;
  /** Marks the image as purely decorative (a caption already names it). */
  decorative?: boolean;
}

/**
 * A cover that degrades to its own palette. The generated covers are
 * not in the repository, so on a machine without them every tile still
 * carries the colour of the record it stands for.
 */
export function Cover({
  src,
  palette,
  alt,
  className = "",
  decorative,
}: CoverProps) {
  // Remembered per source: a tile that is recycled with a new `src`
  // gets to try again instead of staying on the gradient.
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const show = src && failedSrc !== src;
  return (
    <div
      className={`fe-tile ${className}`}
      style={show ? undefined : { backgroundImage: fallbackGradient(palette) }}
      role={!show && !decorative ? "img" : undefined}
      aria-label={!show && !decorative ? alt : undefined}
    >
      {show && (
        <img
          src={src}
          alt={decorative ? "" : alt}
          loading="lazy"
          decoding="async"
          onError={() => setFailedSrc(src)}
        />
      )}
    </div>
  );
}

interface MosaicProps {
  covers: string[];
  palette?: string[] | null;
  className?: string;
}

/** Four covers in a 2×2 — how the app already draws a smart playlist. */
export function Mosaic({ covers, palette, className = "" }: MosaicProps) {
  const cells = [0, 1, 2, 3].map(
    (i) => covers[i] ?? covers[i % Math.max(covers.length, 1)] ?? null,
  );
  return (
    <div
      className={`fe-tile grid grid-cols-2 grid-rows-2 ${className}`}
      aria-hidden="true"
    >
      {cells.map((c, i) =>
        c ? (
          <img key={i} src={c} alt="" loading="lazy" decoding="async" />
        ) : (
          <div key={i} style={{ backgroundImage: fallbackGradient(palette) }} />
        ),
      )}
    </div>
  );
}
