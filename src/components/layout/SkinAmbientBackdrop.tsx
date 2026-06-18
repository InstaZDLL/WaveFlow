import { convertFileSrc } from "@tauri-apps/api/core";
import { usePlayer } from "../../hooks/usePlayer";
import { useSkin } from "../../hooks/useSkin";

/**
 * Fullscreen ambient backdrop — reads the currently playing
 * track's cover art and paints it as a blurred, dimmed
 * background behind the entire app layout. Only renders when
 * the active skin opts into the ambient layer (Lounge); other
 * skins return `null` so they pay zero render cost.
 *
 * Cross-fade strategy: each cover URL renders inside an
 * element that's `key`ed by URL, so React mounts a new element
 * (and unmounts the old) on every cover change. The fresh
 * element runs a CSS keyframe (`loungeBackdropFadeIn` in
 * `app.css`) to fade up over the active skin's motion
 * duration. No state machine needed — React's reconciliation
 * does the cross-fade for free.
 *
 * Mounted as a sibling at the AppLayout root with
 * `position: fixed` + `z-index: -10` so it tracks the viewport
 * and never re-flows; the rest of the UI floats above without
 * needing to re-paint the backdrop.
 */
export function SkinAmbientBackdrop() {
  const { skin } = useSkin();
  const { currentTrack } = usePlayer();

  const enabled = skin.id === "lounge";
  const artworkPath = currentTrack?.artwork_path ?? null;
  // Tauri's asset protocol — the path lives under the
  // per-profile data dir and isn't a regular http URL.
  const resolved = artworkPath ? convertFileSrc(artworkPath) : null;

  if (!enabled) return null;

  // Respect the OS-level reduced-motion preference (WCAG 2.3.3).
  // The check is synchronous + non-reactive: the media query is
  // sampled once per render, and switching the preference is
  // rare enough that we don't need a `useEffect` + listener
  // dance for the next cover swap to pick it up. `matchMedia`
  // is missing in SSR/non-browser hosts (tests) — guard with
  // `typeof`.
  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  return (
    <div
      aria-hidden="true"
      className="fixed inset-0 -z-10 overflow-hidden pointer-events-none bg-black"
    >
      {resolved && (
        <div
          key={resolved}
          className="absolute inset-0 bg-center bg-cover"
          style={{
            backgroundImage: `url("${resolved}")`,
            // Vivid: brightness almost full + heavy saturation
            // boost so the album's dominant colour (warm reds
            // on the current track, etc.) reads as the
            // ambient mood. Earlier `brightness(0.6)` washed
            // every cover out to dim gray.
            filter: "blur(72px) saturate(320%) brightness(1.2)",
            transform: "scale(1.6)",
            // Skip the cross-fade keyframe when the user opted
            // out of motion — the cover swap becomes an instant
            // cut, which is the conventional reduced-motion
            // affordance.
            animation: reduceMotion
              ? undefined
              : `loungeBackdropFadeIn ${skin.motion.duration}s ${skin.motion.ease} forwards`,
          }}
        />
      )}
      {/* Featherweight vignette — only darkens the very top +
          bottom edges where the chrome (header + footer) sit,
          so their text stays legible. The body section in the
          middle stays untouched so the cover's colour can
          dominate. */}
      <div className="absolute inset-0 bg-linear-to-b from-black/5 via-transparent to-black/10" />
    </div>
  );
}
