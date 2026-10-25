import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { PrecomputedBlur } from "../common/PrecomputedBlur";
import { loadBlurSource } from "../../lib/precomputedBlur";
import { usePlayer } from "../../hooks/usePlayer";
import { useSkin } from "../../hooks/useSkin";

/**
 * The room — the playing record's cover, blurred to a colour field over
 * the whole window, behind everything. Mounted only when the active
 * skin's preset asks for it (`fluid.atmosphere: "room"`, Lounge); every
 * other skin returns `null` and pays nothing.
 *
 * The blur is computed once per record, not once per frame
 * (`PrecomputedBlur`): the compositor moves a static texture instead of
 * re-running a 140 px blur over the whole window under every frame of a
 * scroll, as the 1.x Lounge did with a CSS filter on a full-window
 * element.
 *
 * Cross-fade: each record's canvas is `key`ed by its URL and fades in
 * over the previous one, which stays underneath until the next record
 * replaces it; under reduced motion the swap is a cut. The layer is
 * `position: fixed` behind the app (`-z-10`), so nothing re-flows it.
 * The tint over it — what keeps a pale sleeve from lighting the room
 * white — is the skin's (`.skin-room::after` in `lounge.css`).
 *
 * Off under the WebKitGTK safeguard: the skin's static gradient stands
 * in for the room there.
 */
export function SkinAmbientBackdrop() {
  const { skin } = useSkin();
  const { currentTrack } = usePlayer();

  const enabled =
    skin.layout === "fluid" &&
    skin.fluid?.atmosphere === "room" &&
    !document.documentElement.hasAttribute("data-webkitgtk-safe");

  const artworkPath = enabled ? (currentTrack?.artwork_path ?? null) : null;
  // Tauri's asset protocol — the path lives under the per-profile data
  // dir and isn't a regular http URL.
  const src = artworkPath ? convertFileSrc(artworkPath) : null;
  const layers = useRoomLayers(src);

  if (!enabled) return null;

  // Respect the OS-level reduced-motion preference (WCAG 2.3.3), sampled
  // once per render: switching the preference is rare enough that the
  // next cover swap picking it up is fine. `matchMedia` is missing in
  // non-browser hosts (tests) — guard with `typeof`.
  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const visible = src ? layers : [];

  return (
    <div
      aria-hidden="true"
      className="skin-room fixed inset-0 -z-10 overflow-hidden pointer-events-none"
      data-lit={visible.length > 0 || undefined}
    >
      {visible.map((layer, i) => (
        // 136 px on a 1600 px window; saturation pumps the sleeve's hues so
        // the room takes its colour, not its grey, and the darkening keeps
        // a white sleeve from lighting it white. Cover-fit, then 1.6× past
        // the edges: the blur pulls the middle of the sleeve out to the
        // window's corners.
        <PrecomputedBlur
          key={layer.src}
          image={layer.image}
          blur={136}
          saturate={2.6}
          brightness={0.62}
          scale={1.6}
          className="absolute inset-0"
          style={{
            animation:
              i > 0 && !reduceMotion
                ? `fe-fade-in ${skin.motion.duration * 2}s ${skin.motion.ease} both`
                : undefined,
          }}
        />
      ))}
    </div>
  );
}

interface RoomImage {
  src: string;
  image: HTMLImageElement;
}

/** The decoded covers on screen: the current one last, the one it fades
 *  over before it. A cover that fails to load leaves the previous light;
 *  a track with no cover clears it, so an older sleeve never comes back
 *  under the next one while that one loads. */
function useRoomLayers(src: string | null): RoomImage[] {
  const [layers, setLayers] = useState<RoomImage[]>([]);
  // Reset during render rather than in an effect: the cleared room is
  // what this very render paints.
  const [lastSrc, setLastSrc] = useState(src);
  if (src !== lastSrc) {
    setLastSrc(src);
    if (!src) setLayers([]);
  }
  useEffect(() => {
    if (!src) return;
    let cancelled = false;
    loadBlurSource(src)
      .then((image) => {
        if (cancelled) return;
        setLayers((prev) => [
          ...prev.filter((l) => l.src !== src).slice(-1),
          { src, image },
        ]);
      })
      .catch(() => {
        // A cover that fails to load leaves the previous light.
      });
    return () => {
      cancelled = true;
    };
  }, [src]);
  return layers;
}
