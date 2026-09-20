import { useArtistImage } from "./useArtistImage";
import { useCoverSlideshow } from "./useCoverSlideshow";
import { usePrefersReducedMotion } from "./usePrefersReducedMotion";
import { isStreamTrack } from "../lib/playerSources";
import type { Track } from "../lib/tauri/track";

export interface SlideshowLayer {
  /** Whether the slideshow owns the cover slot right now. */
  active: boolean;
  /** The photo to crossfade in, or `null` when there is none (yet). */
  artistSrc: string | null;
}

/**
 * Resolve the cover ↔ artist slideshow (issue #466) for one now-playing
 * surface: the per-profile toggle, reduced motion, the track's eligibility
 * and the artist photo, folded into the one boolean `<CoverSlideshow>`
 * takes.
 *
 * Extracted when the mini-player became the third surface to want it
 * (issue #702) — the gate had been copied twice already, and a third copy
 * is how the three drift apart.
 *
 * **`blocked` is the precedence chain above the slideshow.** The
 * documented order is Canvas > motion cover > slideshow > static cover, so
 * a surface that renders those layers passes whether one of them owns the
 * slot; a surface that renders neither (the mini-player) passes nothing.
 *
 * **`artistSrc` is for a surface that already has the photo.** The panel
 * enriches the artist for its "About the artist" block anyway, so it hands
 * the result in rather than making this ask the backend a second time.
 * `undefined` means "no override, resolve it yourself" — `null` is a real
 * answer (that surface looked and found no photo), which is why the two
 * are told apart rather than treated as one falsy value.
 */
export function useSlideshowLayer(
  track: Track | null | undefined,
  options: { blocked?: boolean; artistSrc?: string | null } = {},
): SlideshowLayer {
  const { blocked = false, artistSrc: provided } = options;
  const enabled = useCoverSlideshow().enabled;
  const reducedMotion = usePrefersReducedMotion();
  // Radio (negative sentinel id) and other streamed tracks have no library
  // artist to enrich, so they never get a slideshow.
  const eligible = !!track && !isStreamTrack(track);
  const running = enabled && !reducedMotion && !blocked && eligible;
  // Only enrich when the slideshow could actually run — the toggle is off by
  // default, and a Canvas or motion cover would own the slot anyway — so a
  // profile that never opted in makes no artist call at all.
  const fetched = useArtistImage(
    running && provided === undefined ? track?.artist_id : null,
  );
  const artistSrc = provided === undefined ? fetched : provided;
  return { active: running && !!artistSrc, artistSrc };
}
