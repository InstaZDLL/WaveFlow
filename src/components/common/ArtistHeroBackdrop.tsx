/**
 * Full-bleed backdrop painted behind the artist detail header — issue
 * #482, the Spotify-style artist banner.
 *
 * Three tiers, decided by the caller and reported through `tier`:
 *
 * 1. **A banner the user chose** (a TheAudioDB fanart picked in the
 *    banner picker, or a local file) — shown **as it is**: no blur, no
 *    saturation boost, no scale. A deliberate pick, often at full
 *    resolution, is the one image here that must not be touched; the
 *    softening below is for sources nobody looked at (#1094). Cropped
 *    from the top third rather than the middle, so a shrinking banner
 *    loses sky before it loses a face (issue #693).
 * 2. **TheAudioDB's automatic fanart** (cached in `metadata_artwork/`,
 *    nobody chose it) — a 1px blur, only to keep JPEG artefacts from
 *    crawling under the white header copy. The blur was 2px until a
 *    chosen banner turned out to be softened by it too; at 200 % display
 *    scaling 2 CSS px are 4 device pixels. Same top-third crop.
 * 3. **The square artist photo** (Deezer or a local `artist.jpg`) —
 *    heavily blurred + upscaled, the same colour-field treatment
 *    [`SkinAmbientBackdrop`](../layout/SkinAmbientBackdrop.tsx) uses. A
 *    1:1 image stretched across a 3:1 banner would be unreadable
 *    otherwise, and this tier is the universal fallback: it works
 *    offline and needs no network metadata at all.
 *
 * Legibility is not left to the theme: the image always carries a dark
 * scrim so the white header text reads in light *and* dark themes —
 * Spotify's header is dark-on-image in every mode. The bottom edge
 * fades out through a mask instead of a hard-coded colour stop, so the
 * hero dissolves into whatever the current theme/skin paints behind it.
 */
/**
 * Escape a URL for interpolation inside a double-quoted CSS `url("…")`.
 * Unlike the local artwork paths other backdrops paint, a hero source can
 * be a **remote URL straight out of TheAudioDB** — third-party data that
 * must not be able to close the string and inject CSS. Inside a quoted
 * string only the backslash, the closing quote and raw newlines matter.
 */
function cssUrl(src: string): string {
  return src.replace(/[\\"]/g, "\\$&").replace(/[\n\r\f]/g, "");
}

/** Where the hero image comes from, which decides how it is treated. */
export type ArtistHeroTier = "chosen" | "fanart" | "photo";

interface ArtistHeroBackdropProps {
  /** Resolved image URL (asset:// or remote). `null` renders nothing. */
  src: string | null;
  /**
   * `chosen`: a wide banner the user picked — untouched. `fanart`:
   * TheAudioDB's automatic wide image — lightly blurred. `photo`: the
   * square-photo fallback — heavily blurred.
   */
  tier: ArtistHeroTier;
}

export function ArtistHeroBackdrop({ src, tier }: ArtistHeroBackdropProps) {
  if (!src) return null;

  // Respect the OS-level reduced-motion preference (WCAG 2.3.3) for the
  // cross-fade only — the image itself is static, so there's nothing to
  // hide from a motion-sensitive user. Sampled synchronously per render,
  // same trade-off as SkinAmbientBackdrop; `matchMedia` is missing in
  // non-browser hosts, hence the `typeof` guard.
  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // Fade the bottom edge out instead of cutting it — the hero then
  // blends into the page background whatever the theme paints there.
  const wide = tier !== "photo";
  const fadeMask = "linear-gradient(to bottom, black 68%, transparent 100%)";

  return (
    <div
      aria-hidden="true"
      className="absolute inset-0 overflow-hidden pointer-events-none"
      style={{ maskImage: fadeMask, WebkitMaskImage: fadeMask }}
    >
      <div
        // Keyed by URL so a source swap (cache hit → enrichment result)
        // mounts a fresh element and re-runs the fade-in keyframe —
        // React's reconciliation does the cross-fade for free.
        key={src}
        className="absolute inset-0 bg-cover"
        style={{
          backgroundImage: `url("${cssUrl(src)}")`,
          // Anchored high rather than centred (#693). `bg-cover` fills a
          // very wide, fairly short banner, so the image is cropped top
          // and bottom — and centring crops *equally* from both, which
          // takes the head first, because faces sit in the upper third
          // of nearly every artist photo. The deeper the crop (a wider
          // window makes the banner relatively shorter), the worse it
          // got: the framing visibly shifted as the window resized.
          // Keeping the horizon at 30 % loses sky and floor first.
          // Not applied to the blurred square photo, which has no
          // subject left to protect and whose edges are the only thing
          // the position would move.
          backgroundPosition: wide ? "center 30%" : "center",
          filter:
            tier === "chosen"
              ? undefined
              : tier === "fanart"
                ? "blur(1px) saturate(115%)"
                : "blur(56px) saturate(190%)",
          // Scale past the edges so a blur does not bleed the container's
          // background in at the borders. Only the blur needs it: the
          // chosen banner has none, and a scale would resample it, which
          // is the softness #1094 was about. 1px of blur spreads about two
          // device-independent pixels, hence 2 % (1 % a side, 3px on a
          // 300px-tall hero) for the fanart.
          transform:
            tier === "chosen"
              ? undefined
              : tier === "fanart"
                ? "scale(1.02)"
                : "scale(1.35)",
          animation: reduceMotion
            ? undefined
            : "artistHeroFadeIn 0.45s ease-out forwards",
        }}
      />
      {/* Dark scrim — theme-independent on purpose (see the header copy
          which is forced white to match). Heavier at the bottom, where
          the name and stats sit. */}
      <div className="absolute inset-0 bg-linear-to-t from-black/85 via-black/60 to-black/35" />
    </div>
  );
}
