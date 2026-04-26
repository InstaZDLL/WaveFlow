import { useId, useMemo, useState } from "react";

interface BarDatum {
  /** Stable key for React + tooltip lookup. */
  key: string;
  /** Label shown on the X axis. */
  label: string;
  /** Bar height value (must be >= 0). */
  value: number;
  /** Optional tooltip override; defaults to `label: value`. */
  tooltip?: string;
}

interface BarChartProps {
  data: BarDatum[];
  /** Pixel height of the chart (excluding labels). */
  height?: number;
  /** When true, only every Nth label is rendered to avoid overcrowding. */
  thinLabels?: boolean;
  /** Empty-state text shown when `data` is empty or all-zero. */
  emptyText: string;
  ariaLabel: string;
}

/**
 * Minimal SVG bar chart — no external chart library.
 *
 * Bars are sized by ratio against the dataset max; tooltip shows on hover
 * via a positioned div. Renders responsively via `viewBox` so it scales
 * with the parent container.
 */
export function BarChart({
  data,
  height = 160,
  thinLabels,
  emptyText,
  ariaLabel,
}: BarChartProps) {
  const titleId = useId();
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);

  const max = useMemo(
    () => data.reduce((m, d) => Math.max(m, d.value), 0),
    [data],
  );

  if (data.length === 0 || max === 0) {
    return (
      <div
        className="flex items-center justify-center text-sm text-zinc-500 dark:text-zinc-400"
        style={{ height }}
      >
        {emptyText}
      </div>
    );
  }

  const labelStep = thinLabels
    ? Math.max(1, Math.ceil(data.length / 8))
    : 1;
  const barWidthPct = 100 / data.length;
  const innerBarWidthPct = barWidthPct * 0.7;
  const barOffsetPct = barWidthPct * 0.15;

  return (
    <div className="relative">
      <svg
        viewBox={`0 0 100 ${height}`}
        preserveAspectRatio="none"
        className="w-full"
        style={{ height }}
        role="img"
        aria-labelledby={titleId}
      >
        <title id={titleId}>{ariaLabel}</title>
        {data.map((d, i) => {
          const h = (d.value / max) * (height - 4);
          const x = i * barWidthPct + barOffsetPct;
          const y = height - h;
          const active = hoverIdx === i;
          return (
            <rect
              key={d.key}
              x={x}
              y={y}
              width={innerBarWidthPct}
              height={h}
              rx={0.5}
              className={
                active
                  ? "fill-emerald-500"
                  : "fill-emerald-500/70 hover:fill-emerald-500"
              }
              onMouseEnter={() => setHoverIdx(i)}
              onMouseLeave={() => setHoverIdx(null)}
            >
              <title>{d.tooltip ?? `${d.label}: ${d.value}`}</title>
            </rect>
          );
        })}
      </svg>
      <div className="flex mt-2 text-[10px] text-zinc-500 dark:text-zinc-400 tabular-nums">
        {data.map((d, i) => (
          <div
            key={d.key}
            className="text-center truncate"
            style={{ width: `${barWidthPct}%` }}
          >
            {i % labelStep === 0 ? d.label : ""}
          </div>
        ))}
      </div>
    </div>
  );
}
