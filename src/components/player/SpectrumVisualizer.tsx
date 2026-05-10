import { useEffect, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface SpectrumPayload {
  bands: number[];
}

interface SpectrumVisualizerProps {
  /** Tailwind sizing classes for the canvas wrapper. */
  className?: string;
  /** Bar fill colour (any CSS colour the canvas accepts). */
  color?: string;
  /**
   * When true, draws a light-on-dark variant suited to the immersive
   * overlay backdrop (white bars at moderate opacity). When false,
   * uses the regular `color` prop.
   */
  glow?: boolean;
}

/**
 * Real-time spectrum visualizer. Subscribes to the backend
 * `player:spectrum` event (emitted at ~30 Hz from the decoder
 * thread, see `audio/spectrum.rs`) and renders log-spaced bars on a
 * `<canvas>` driven by `requestAnimationFrame`. Bands smoothly decay
 * between frames so the visual feels fluid even though the source
 * cadence is below the screen refresh rate.
 *
 * The component is always safe to mount: the backend short-circuits
 * the FFT entirely when the visualizer toggle is off, and this
 * component just renders an empty canvas in that case.
 */
export function SpectrumVisualizer({
  className = "w-full h-24",
  color = "#10b981",
  glow = false,
}: SpectrumVisualizerProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // Latest bands received from the backend. Mutable ref so the
  // animation loop reads the freshest values without re-rendering.
  const targetRef = useRef<number[] | null>(null);
  // Smoothed bar heights actually drawn this frame. Decays toward
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

    // Match the canvas backing store to the DPR + actual rendered
    // size so bars stay crisp on Retina / 4K displays.
    const fitCanvas = () => {
      const dpr = window.devicePixelRatio || 1;
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

    const tick = () => {
      const target = targetRef.current;
      if (target && target.length > 0) {
        if (
          !drawnRef.current ||
          drawnRef.current.length !== target.length
        ) {
          drawnRef.current = new Array(target.length).fill(0);
        }
        const drawn = drawnRef.current;
        // Asymmetric smoothing: jump up fast (so transients pop),
        // decay down slowly (so bars don't look glitchy).
        for (let i = 0; i < drawn.length; i++) {
          const t = target[i] ?? 0;
          drawn[i] = drawn[i] < t ? drawn[i] + (t - drawn[i]) * 0.55 : drawn[i] + (t - drawn[i]) * 0.18;
        }
      } else if (drawnRef.current) {
        // No incoming bands → fade to zero so the bars don't freeze
        // mid-pose when playback pauses.
        const drawn = drawnRef.current;
        let any = false;
        for (let i = 0; i < drawn.length; i++) {
          drawn[i] *= 0.85;
          if (drawn[i] > 0.001) any = true;
        }
        if (!any) drawnRef.current = null;
      }

      const drawn = drawnRef.current;
      const w = canvas.width;
      const h = canvas.height;
      ctx.clearRect(0, 0, w, h);

      if (drawn && drawn.length > 0) {
        const barCount = drawn.length;
        // Small inset so the gap between bars is visible at the edges.
        const gap = Math.max(1, Math.floor(w / barCount / 4));
        const barWidth = Math.max(1, (w - gap * (barCount - 1)) / barCount);

        ctx.fillStyle = glow ? "rgba(255,255,255,0.85)" : color;
        for (let i = 0; i < barCount; i++) {
          const value = Math.max(0, Math.min(1, drawn[i]));
          const barHeight = value * h;
          const x = i * (barWidth + gap);
          const y = h - barHeight;
          ctx.fillRect(x, y, barWidth, barHeight);
        }
      }

      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  }, [color, glow]);

  return (
    <canvas
      ref={canvasRef}
      className={className}
      // Hide from the a11y tree — purely decorative.
      aria-hidden="true"
    />
  );
}
