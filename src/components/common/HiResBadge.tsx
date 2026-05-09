import { dsdLabel, isHiRes } from "../../lib/hiRes";

/**
 * Hi-Res Audio badge — shown when the source file is delivered at a
 * better-than-CD spec (≥ 24-bit, ≥ 44.1 kHz) OR when the codec is
 * DSD (in which case the badge says "DSD64", "DSD128", etc. instead
 * of "Hi-Res 24-bit").
 *
 * Two visual variants:
 * - `overlay` is intended to sit on top of an album cover (top-left
 *   corner, drop shadow);
 * - `inline` is for sidebar / row contexts where the badge sits next
 *   to text.
 */
interface HiResBadgeProps {
  bitDepth: number | null;
  sampleRate: number | null;
  /** Codec label from the scanner (e.g. "FLAC", "DSD128"). */
  codec?: string | null;
  variant?: "overlay" | "inline";
  /** Override the visible text. Default is "Hi-Res {bitDepth}-bit". */
  label?: string;
}

export function HiResBadge({
  bitDepth,
  sampleRate,
  codec,
  variant = "overlay",
  label,
}: HiResBadgeProps) {
  // DSD wins over the generic Hi-Res check — a DSF/DFF file reports
  // bit_depth=1 but is anything but lossy, and the user expects the
  // rate label (DSD64/128/...) rather than "Hi-Res 1-bit".
  const dsd = dsdLabel(codec);
  const isVisible = dsd !== null || isHiRes(bitDepth, sampleRate);
  if (!isVisible) return null;
  const text = label ?? dsd ?? `Hi-Res ${bitDepth}-bit`;
  if (variant === "inline") {
    return (
      <span className="inline-flex items-center px-2 py-0.5 rounded-full text-[10px] font-bold bg-emerald-500 text-white tracking-wide">
        {text}
      </span>
    );
  }
  return (
    <div className="absolute top-2 left-2 px-2 py-0.5 rounded-full text-[10px] font-bold bg-emerald-500 text-white shadow-md tracking-wide pointer-events-none select-none">
      {text}
    </div>
  );
}
