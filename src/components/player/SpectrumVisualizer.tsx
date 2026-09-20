import { useEffect, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import type { VisualizerStyleId } from "../../hooks/useVisualizerStyle";

interface SpectrumPayload {
  bands: number[];
}

interface SpectrumVisualizerProps {
  /** Tailwind sizing classes for the canvas wrapper. */
  className?: string;
  /**
   * Fill colour (any CSS colour the canvas accepts). When omitted, falls
   * back to the light backdrop variant (see `glow`). Ignored when `rainbow`.
   */
  color?: string;
  /**
   * When true, tints the drawing by a left-to-right hue sweep instead of a
   * solid fill — overrides `color`. Issue #468.
   */
  rainbow?: boolean;
  /**
   * When true and no explicit `color` is given, draws the light-on-dark
   * variant suited to the immersive overlay backdrop (white at moderate
   * opacity). An explicit `color`/`rainbow` always wins.
   */
  glow?: boolean;
  /**
   * How the bands are drawn (issue #699): a smooth filled curve (`wave`),
   * that curve mirrored around a centre line (`mirror`), or the original
   * rectangles (`bars`).
   */
  styleId?: VisualizerStyleId;
}

/** Curve thickness in CSS pixels, scaled by the device pixel ratio. */
const CURVE_WIDTH = 1.5;
/** How opaque the area under the curve is in high contrast, where the
 *  fade-to-transparent gradient is replaced by a flat wash. */
const HIGH_CONTRAST_AREA_ALPHA = 0.4;
/** Same, for the rainbow sweep, which has no vertical fade of its own. */
const RAINBOW_AREA_ALPHA = 0.35;

/**
 * Real-time spectrum visualizer. Subscribes to the backend
 * `player:spectrum` event (emitted at ~30 Hz from the decoder thread, see
 * `audio/spectrum.rs`) and renders the 48 log-spaced bands on a `<canvas>`
 * driven by `requestAnimationFrame`. Bands smoothly decay between frames so
 * the visual feels fluid even though the source cadence is below the screen
 * refresh rate.
 *
 * Three styles (issue #699). The data was already good enough for curves —
 * what changed is how the 48 values are drawn, not the analysis:
 *
 * - `wave` draws one Catmull-Rom curve through the band tops, emitted as
 *   cubic Béziers, filled underneath with a gradient that fades out;
 * - `mirror` reflects that curve around the middle, as one closed shape;
 * - `bars` keeps the rectangles, with rounded caps.
 *
 * **The frame loop allocates nothing.** A gradient is bound to the
 * coordinates it was built with, so it is rebuilt when the canvas is
 * resized and cached otherwise; the curve is emitted straight into one path
 * by index rather than through an array of points; and the mirror's lower
 * edge is drawn by flipping a sign the helpers read, rather than by handing
 * them a fresh closure sixty times a second.
 *
 * The component is always safe to mount: the backend short-circuits the FFT
 * entirely when the visualizer toggle is off, and this just renders an
 * empty canvas in that case.
 */
export function SpectrumVisualizer({
  className = "w-full h-24",
  color,
  rainbow = false,
  glow = false,
  styleId = "wave",
}: SpectrumVisualizerProps) {
  // An explicit colour wins; otherwise fall back to the immersive light
  // variant (`glow`) or the historical emerald default.
  const fill = color ?? (glow ? "rgba(255,255,255,0.85)" : "#10b981");
  const reducedMotion = usePrefersReducedMotion();
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // Latest bands received from the backend. Mutable ref so the
  // animation loop reads the freshest values without re-rendering.
  const targetRef = useRef<number[] | null>(null);
  // Smoothed heights actually drawn this frame. Decays toward
  // `targetRef` each tick → fluid animation.
  const drawnRef = useRef<number[] | null>(null);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen<SpectrumPayload>("player:spectrum", (event) => {
      targetRef.current = event.payload.bands;
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch((err) => console.error("[SpectrumVisualizer] listen failed", err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let raf = 0;
    let dpr = window.devicePixelRatio || 1;

    // Match the canvas backing store to the DPR + actual rendered
    // size so the drawing stays crisp on Retina / 4K displays.
    const fitCanvas = () => {
      dpr = window.devicePixelRatio || 1;
      const rect = canvas.getBoundingClientRect();
      const width = Math.max(1, Math.floor(rect.width * dpr));
      const height = Math.max(1, Math.floor(rect.height * dpr));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
    };

    const ro = new ResizeObserver(fitCanvas);
    ro.observe(canvas);
    fitCanvas();

    // ── Paints, rebuilt only when their inputs change ─────────────────
    let paintKey = "";
    let strokePaint: string | CanvasGradient = fill;
    let areaPaint: string | CanvasGradient = fill;

    const buildPaints = (w: number, h: number, highContrast: boolean) => {
      const key = `${w}x${h}|${fill}|${rainbow}|${highContrast}|${styleId}`;
      if (key === paintKey) return;
      paintKey = key;

      if (rainbow) {
        // The per-bar hue sweep of #468, as one horizontal gradient: a
        // curve has no bars to tint one by one. 0..300° (red → violet),
        // stopping short of 360 so the right edge doesn't wrap back to the
        // left edge's red.
        const sweep = ctx.createLinearGradient(0, 0, w, 0);
        for (let i = 0; i <= 6; i++) {
          sweep.addColorStop(i / 6, `hsl(${(i / 6) * 300}, 85%, 60%)`);
        }
        strokePaint = sweep;
      } else {
        strokePaint = fill;
      }

      // High contrast keeps the shape solid: a fill fading to transparent
      // is exactly the low-contrast edge that mode exists to remove, so the
      // area becomes a flat wash instead — painted through `globalAlpha`,
      // which works for any CSS colour the caller passes, where rewriting
      // one into `rgba()` would not. The rainbow sweep is flat for the same
      // reason: the canvas cannot compose two gradients into one paint.
      if (highContrast || rainbow || styleId === "bars") {
        areaPaint = strokePaint;
        return;
      }
      const area = ctx.createLinearGradient(0, 0, 0, h);
      area.addColorStop(0, fill);
      area.addColorStop(1, "transparent");
      areaPaint = area;
    };

    // ── Curve emission ────────────────────────────────────────────────
    // Catmull-Rom through the band values, converted to cubic Béziers: the
    // tangent at each point is a sixth of the segment between its
    // neighbours (the standard uniform form).
    //
    // `passBaseline` / `passSign` / `passAmp` are written before each pass
    // rather than handed to the helpers, because the mirror draws the same
    // curve twice per frame and a closure per pass would be an allocation
    // per frame for nothing.
    let passBaseline = 0;
    let passSign = -1;
    let passAmp = 0;

    const yAt = (i: number) => {
      const drawn = drawnRef.current;
      const v = drawn ? (drawn[i] ?? 0) : 0;
      const clamped = v < 0 ? 0 : v > 1 ? 1 : v;
      return passBaseline + passSign * clamped * passAmp;
    };

    const emitCurve = (n: number, dx: number, from: number, to: number) => {
      const step = to > from ? 1 : -1;
      const last = n - 1;
      const clampIdx = (i: number) => (i < 0 ? 0 : i > last ? last : i);
      for (let i = from; i !== to; i += step) {
        const j = i + step;
        const prev = clampIdx(i - step);
        const next = clampIdx(j + step);
        const x1 = i * dx;
        const y1 = yAt(i);
        const x2 = j * dx;
        const y2 = yAt(j);
        const x0 = prev * dx;
        const y0 = yAt(prev);
        const x3 = next * dx;
        const y3 = yAt(next);
        ctx.bezierCurveTo(
          x1 + (x2 - x0) / 6,
          y1 + (y2 - y0) / 6,
          x2 - (x3 - x1) / 6,
          y2 - (y3 - y1) / 6,
          x2,
          y2,
        );
      }
    };

    const tick = () => {
      // Asymmetric smoothing: jump up fast (so transients pop), decay down
      // slowly (so it doesn't look glitchy). Under reduced motion both
      // halves are damped — the ask there is for calm, and a curve that
      // eases into place is calmer than one that stops moving.
      const attack = reducedMotion ? 0.22 : 0.55;
      const release = reducedMotion ? 0.08 : 0.18;

      const target = targetRef.current;
      if (target && target.length > 0) {
        if (!drawnRef.current || drawnRef.current.length !== target.length) {
          drawnRef.current = new Array(target.length).fill(0);
        }
        const drawn = drawnRef.current;
        for (let i = 0; i < drawn.length; i++) {
          const t = target[i] ?? 0;
          drawn[i] =
            drawn[i] < t
              ? drawn[i] + (t - drawn[i]) * attack
              : drawn[i] + (t - drawn[i]) * release;
        }
      } else if (drawnRef.current) {
        // No incoming bands → settle to zero so the shape doesn't freeze
        // mid-pose when playback pauses. A curve lands on a flat line.
        const drawn = drawnRef.current;
        const decay = reducedMotion ? 0.92 : 0.85;
        let any = false;
        for (let i = 0; i < drawn.length; i++) {
          drawn[i] *= decay;
          if (drawn[i] > 0.001) any = true;
        }
        if (!any) drawnRef.current = null;
      }

      const drawn = drawnRef.current;
      const w = canvas.width;
      const h = canvas.height;
      ctx.clearRect(0, 0, w, h);

      if (drawn && drawn.length > 0) {
        // Read off the root rather than through the contrast hook: the
        // attribute is what actually decides how the rest of the window is
        // painted (#596), it is stamped before the first paint, and a
        // decorative canvas has no business holding a profile setting open.
        const highContrast =
          document.documentElement.getAttribute("data-contrast") === "high";
        buildPaints(w, h, highContrast);

        const n = drawn.length;
        if (styleId === "bars") {
          // Small inset so the gap between bars is visible at the edges.
          const gap = Math.max(1, Math.floor(w / n / 4));
          const barWidth = Math.max(1, (w - gap * (n - 1)) / n);
          // Rounded caps, clamped to the bar's own half-width so a thin bar
          // doesn't turn into a lozenge.
          const radius = Math.min(barWidth / 2, 3 * dpr);
          const denom = Math.max(1, n - 1);
          if (!rainbow) ctx.fillStyle = strokePaint;
          for (let i = 0; i < n; i++) {
            if (rainbow) {
              ctx.fillStyle = `hsl(${(i / denom) * 300}, 85%, 60%)`;
            }
            const value = Math.max(0, Math.min(1, drawn[i]));
            const barHeight = value * h;
            if (barHeight <= 0) continue;
            const x = i * (barWidth + gap);
            const y = h - barHeight;
            ctx.beginPath();
            ctx.roundRect(x, y, barWidth, barHeight, [radius, radius, 0, 0]);
            ctx.fill();
          }
        } else {
          const dx = n > 1 ? w / (n - 1) : w;
          const lineWidth = CURVE_WIDTH * dpr * (highContrast ? 2 : 1);
          // Half a stroke sits outside its path, so keep the extremes that
          // far inside the canvas or a band at full scale gets shaved off.
          const inset = lineWidth / 2;

          ctx.lineWidth = lineWidth;
          ctx.lineJoin = "round";
          ctx.lineCap = "round";
          ctx.strokeStyle = strokePaint;
          passSign = -1;

          if (styleId === "mirror") {
            passBaseline = h / 2;
            passAmp = h / 2 - inset;
            ctx.beginPath();
            ctx.moveTo(0, yAt(0));
            emitCurve(n, dx, 0, n - 1);
            // Down the right edge, back along the reflection, closed: one
            // shape, so the fill has no seam down the middle.
            passSign = 1;
            ctx.lineTo((n - 1) * dx, yAt(n - 1));
            emitCurve(n, dx, n - 1, 0);
            ctx.closePath();
            ctx.stroke();
          } else {
            passBaseline = h - inset;
            passAmp = h - lineWidth;
            ctx.beginPath();
            ctx.moveTo(0, yAt(0));
            emitCurve(n, dx, 0, n - 1);
            // Stroke the curve alone, THEN close it down to the baseline
            // and fill — closing first would draw a line along the bottom
            // edge of the canvas.
            ctx.stroke();
            ctx.lineTo(w, h);
            ctx.lineTo(0, h);
            ctx.closePath();
          }

          ctx.fillStyle = areaPaint;
          if (highContrast) ctx.globalAlpha = HIGH_CONTRAST_AREA_ALPHA;
          else if (rainbow) ctx.globalAlpha = RAINBOW_AREA_ALPHA;
          ctx.fill();
          ctx.globalAlpha = 1;
        }
      }

      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, [fill, rainbow, glow, styleId, reducedMotion]);

  return (
    <canvas
      ref={canvasRef}
      className={className}
      // Hide from the a11y tree — purely decorative.
      aria-hidden="true"
    />
  );
}
