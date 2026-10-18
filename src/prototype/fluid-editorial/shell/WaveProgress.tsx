import {
  useCallback,
  useId,
  useRef,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { duration } from "../format";
import { useFe } from "../useFe";

const STEP_MS = 5_000;
const PAGE_MS = 30_000;

/**
 * The player's progress indicator and the page's recurring motif in one
 * place: the played portion is a gentle wave, the rest a hairline. The
 * wave is a single path clipped to the played width, so a seek costs one
 * attribute, and the amplitude is 2 px — read it as a line, not a chart.
 *
 * Keyboard: same contract as the production `ProgressBar` (arrows 5 s,
 * Page 30 s, Home/End).
 */
export function WaveProgress() {
  const { current, positionMs, seek, copy } = useFe();
  const ref = useRef<HTMLDivElement | null>(null);
  const clipId = useId();
  const total = current.durationMs;
  const pct = total > 0 ? Math.min(100, (positionMs / total) * 100) : 0;

  const fromPointer = useCallback(
    (clientX: number) => {
      const el = ref.current;
      if (!el) return 0;
      const r = el.getBoundingClientRect();
      return Math.round(
        Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * total,
      );
    },
    [total],
  );

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) =>
    seek(fromPointer(e.clientX));

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    let next: number | null = null;
    switch (e.key) {
      case "ArrowLeft":
      case "ArrowDown":
        next = positionMs - STEP_MS;
        break;
      case "ArrowRight":
      case "ArrowUp":
        next = positionMs + STEP_MS;
        break;
      case "PageDown":
        next = positionMs - PAGE_MS;
        break;
      case "PageUp":
        next = positionMs + PAGE_MS;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = total;
        break;
    }
    if (next == null) return;
    e.preventDefault();
    seek(Math.min(total, Math.max(0, next)));
  };

  // A wave of wavelength 24 units across a 1000-unit-wide box; the SVG
  // stretches to the bar, the amplitude is pinned by the fixed height.
  const wave = (() => {
    const parts: string[] = ["M0 10"];
    const half = 12;
    for (let x = 0; x < 1000; x += half * 2) {
      parts.push(`q ${half / 2} -4 ${half} 0 t ${half} 0`);
    }
    return parts.join(" ");
  })();

  return (
    <div className="relative h-5 w-full px-16">
      <span className="fe-ink-3 fe-mono absolute left-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
        {duration(positionMs)}
      </span>
      <div
        ref={ref}
        className="fe-progress h-full"
        role="slider"
        tabIndex={0}
        aria-label={copy.player.seek}
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={Math.round(positionMs)}
        aria-valuetext={`${duration(positionMs)} / ${duration(total)}`}
        onPointerDown={onPointerDown}
        onKeyDown={onKeyDown}
      >
        <svg
          viewBox="0 0 1000 20"
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          <defs>
            <clipPath id={clipId}>
              <rect x="0" y="0" width={pct * 10} height="20" />
            </clipPath>
          </defs>
          <line
            x1="0"
            y1="10"
            x2="1000"
            y2="10"
            stroke="rgb(255 255 255 / 0.16)"
            strokeWidth="1.5"
            vectorEffect="non-scaling-stroke"
          />
          <path
            d={wave}
            fill="none"
            stroke="var(--fe-accent)"
            strokeWidth="1.8"
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
            clipPath={`url(#${clipId})`}
          />
          <circle
            className="fe-progress__thumb"
            cx={pct * 10}
            cy="10"
            r="5"
            fill="var(--fe-ink)"
            vectorEffect="non-scaling-stroke"
            style={{
              transform: "scale(0.6, 1)",
              transformOrigin: `${pct * 10}px 10px`,
            }}
          />
        </svg>
      </div>
      <span className="fe-ink-3 fe-mono absolute right-5 top-1/2 -translate-y-1/2 text-[0.75rem]">
        {duration(total)}
      </span>
    </div>
  );
}
