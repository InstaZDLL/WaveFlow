/**
 * Full-bleed backdrop painted behind the artist detail header — issue
 * #482, the Spotify-style artist banner.
 *
 * Two tiers, decided by the caller and reported through `isFanart`:
 *
 * 1. **Real wide fanart** (TheAudioDB, cached in `metadata_artwork/`) —
 *    shown nearly crisp. Only a whisper of blur, enough to keep JPEG
 *    artefacts from crawling under the white header copy.
 * 2. **The square artist photo** (Deezer or a local `artist.jpg`) —
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
interface ArtistHeroBackdropProps {
  /** Resolved image URL (asset:// or remote). `null` renders nothing. */
  src: string | null;
  /** `true` for real wide fanart, `false` for the square-photo fallback. */
  isFanart: boolean;
}

export function ArtistHeroBackdrop({ src, isFanart }: ArtistHeroBackdropProps) {
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
        className="absolute inset-0 bg-center bg-cover"
        style={{
          backgroundImage: `url("${src}")`,
          filter: isFanart
            ? "blur(2px) saturate(115%)"
            : "blur(56px) saturate(190%)",
          // Scale past the edges so neither blur tier bleeds the
          // container's background in at the borders.
          transform: isFanart ? "scale(1.06)" : "scale(1.35)",
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
