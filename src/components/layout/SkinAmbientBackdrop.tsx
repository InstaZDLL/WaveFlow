import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { usePlayer } from "../../hooks/usePlayer";
import { useSkin } from "../../hooks/useSkin";

/**
 * The room — the playing record's cover, blurred to a colour field over
 * the whole window, behind everything. Mounted only when the active
 * skin's preset asks for it (`fluid.atmosphere: "room"`, Lounge); every
 * other skin returns `null` and pays nothing.
 *
 * The blur is computed once per record, not once per frame: the cover is
 * drawn into a small canvas (160 px wide) through the 2D context's own
 * `filter`, and the canvas is stretched to the window. Upscaling a field
 * that is already blurred loses nothing, and the compositor then moves a
 * static texture instead of re-running a 140 px blur over the whole
 * window under every frame of a scroll (the 1.x Lounge did, as a CSS
 * filter on a full-window element). An engine without `ctx.filter`
 * draws the cover unfiltered and the CSS filter does the blur instead —
 * the old cost, on the old engines only.
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
        <RoomLayer
          key={layer.src}
          image={layer.image}
          animation={
            i > 0 && !reduceMotion
              ? `fe-fade-in ${skin.motion.duration * 2}s ${skin.motion.ease} both`
              : undefined
          }
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
    const image = new Image();
    image.decoding = "async";
    image.onload = () => {
      if (cancelled) return;
      setLayers((prev) => [
        ...prev.filter((l) => l.src !== src).slice(-1),
        { src, image },
      ]);
    };
    image.src = src;
    return () => {
      cancelled = true;
    };
  }, [src]);
  return layers;
}

/** Width of the canvas the cover is blurred into; the height follows the
 *  window's aspect. Small on purpose: the field has no detail to keep. */
const ROOM_WIDTH = 160;

let canvasFilter: boolean | null = null;
function supportsCanvasFilter(): boolean {
  if (canvasFilter == null) {
    const ctx = document.createElement("canvas").getContext("2d");
    canvasFilter = ctx != null && "filter" in ctx;
  }
  return canvasFilter;
}

function RoomLayer({
  image,
  animation,
}: {
  image: HTMLImageElement;
  animation: string | undefined;
}) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  const filtered = supportsCanvasFilter();

  useLayoutEffect(() => {
    const canvas = ref.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx || image.naturalWidth === 0) return;
    const w = ROOM_WIDTH;
    const h = Math.max(
      1,
      Math.round((w * window.innerHeight) / window.innerWidth),
    );
    canvas.width = w;
    canvas.height = h;
    // Cover-fit, then 1.6× past the edges, as the 1.x backdrop did: the
    // blur pulls the middle of the sleeve out to the window's corners.
    const scale =
      Math.max(w / image.naturalWidth, h / image.naturalHeight) * 1.6;
    const dw = image.naturalWidth * scale;
    const dh = image.naturalHeight * scale;
    // 140 px on a 1600 px window, in the canvas's own pixels; saturation
    // pumps the sleeve's hues so the room takes its colour, not its grey,
    // and the darkening keeps a white sleeve from lighting it white.
    if (filtered)
      ctx.filter = `blur(${Math.round(w * 0.085)}px) saturate(2.6) brightness(0.62)`;
    ctx.drawImage(image, (w - dw) / 2, (h - dh) / 2, dw, dh);
  }, [image, filtered]);

  return (
    <canvas
      ref={ref}
      className="absolute inset-0 h-full w-full"
      style={{
        filter: filtered
          ? undefined
          : "blur(140px) saturate(260%) brightness(0.62)",
        transform: filtered ? undefined : "scale(1.1)",
        animation,
      }}
    />
  );
}
