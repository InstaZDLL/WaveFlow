import { isHiRes } from "../../lib/hiRes";

/**
 * Hi-Res Audio badge — shown when the source file is delivered at a
 * better-than-CD spec (≥ 24-bit, ≥ 44.1 kHz).
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
  variant?: "overlay" | "inline";
  /** Override the visible text. Default is "Hi-Res 24-bit". */
  label?: string;
}

export function HiResBadge({
  bitDepth,
  sampleRate,
  variant = "overlay",
  label,
}: HiResBadgeProps) {
  if (!isHiRes(bitDepth, sampleRate)) return null;
  const text = label ?? `Hi-Res ${bitDepth}-bit`;
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
