import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { drawBlurred, loadBlurSource } from "../../lib/precomputedBlur";

/**
 * A cover blurred once, not once per frame.
 *
 * A CSS `filter: blur()` on an image is recomputed every time the region
 * it sits in repaints, and anything animating nearby (a progress line, a
 * wave, a pulsing dot) repaints it every frame. With a GPU that is cheap;
 * without one — WebKitGTK's software renderer, the path the renderer
 * fallback (#595) and virtual machines take — it costs the whole frame:
 * the Fluid Home drew one frame every 620 ms on it, and 48 a second with
 * its CSS filters off (`scripts/bench/webkitgtk`).
 *
 * Here the image is blurred into a small canvas (at most 192 px wide,
 * `lib/precomputedBlur.ts`, which works without `ctx.filter` — WebKitGTK
 * has none) sized against the box it fills, and the canvas is stretched
 * back over that box. A field that is already blurred loses nothing to
 * the upscale, and what repaints afterwards is a static bitmap. The blur
 * is redrawn when the box changes size, since the radius is measured
 * against it.
 *
 * The values are the ones the CSS filter said: `blur` in CSS pixels of
 * the box, `scale` the zoom past the box's edges the transform gave.
 */
interface PrecomputedBlurProps {
  /** The image to blur. A small thumbnail is enough: the field has no
   *  detail to keep, and it decodes faster. */
  src?: string | null;
  /** An image already loaded (by `loadBlurSource`), in place of `src`. */
  image?: HTMLImageElement | null;
  /** Blur radius, in CSS pixels of the box. */
  blur: number;
  saturate?: number;
  brightness?: number;
  /** Cover-fit, then zoomed this much past the box's edges. */
  scale?: number;
  className?: string;
  style?: CSSProperties;
}

/** The loaded image for `src`, or null until it has loaded (or failed). */
function useLoaded(
  src: string | null | undefined,
  given: HTMLImageElement | null | undefined,
) {
  const [loaded, setLoaded] = useState<{
    src: string;
    image: HTMLImageElement;
  } | null>(null);
  useEffect(() => {
    if (given || !src) return;
    let cancelled = false;
    loadBlurSource(src)
      .then((image) => {
        if (!cancelled) setLoaded({ src, image });
      })
      .catch(() => {
        // No light rather than a broken one.
      });
    return () => {
      cancelled = true;
    };
  }, [src, given]);
  if (given) return given;
  return loaded && loaded.src === src ? loaded.image : null;
}

export function PrecomputedBlur({
  src,
  image,
  blur,
  saturate = 1,
  brightness = 1,
  scale = 1,
  className,
  style,
}: PrecomputedBlurProps) {
  const ref = useRef<HTMLCanvasElement | null>(null);
  const loaded = useLoaded(src, image);

  useLayoutEffect(() => {
    const canvas = ref.current;
    if (!canvas || !loaded || loaded.naturalWidth === 0) return;
    let drawnFor = "";
    const draw = () => {
      const box = canvas.getBoundingClientRect();
      if (box.width < 1 || box.height < 1) return;
      const key = `${Math.round(box.width)}x${Math.round(box.height)}`;
      if (key === drawnFor) return;
      drawnFor = key;
      drawBlurred(canvas, loaded, box.width, box.height, {
        blur,
        saturate,
        brightness,
        scale,
      });
    };
    draw();
    // Redrawn when the box changes shape, at most once a frame.
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(draw);
    });
    observer.observe(canvas);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [loaded, blur, saturate, brightness, scale]);

  return (
    <canvas
      ref={ref}
      aria-hidden="true"
      className={className}
      style={{ display: "block", width: "100%", height: "100%", ...style }}
    />
  );
}
