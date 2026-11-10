import { qualityBadge } from "../../lib/hiRes";
import { useHiResBadgeVisibility } from "../../hooks/useHiResBadgeVisibility";

/**
 * Quality badge — three tiers, best first ([`qualityBadge`]):
 *
 * - **DSD** — the codec is DSD, and the badge says "DSD64", "DSD128"…;
 * - **Hi-Res** — the file is delivered at a better-than-CD spec
 *   (≥ 24-bit, ≥ 44.1 kHz): "Hi-Res 24-bit";
 * - **Lossless** — a lossless file at CD quality (16-bit FLAC, ALAC, WAV,
 *   AIFF, APE, WavPack): "Lossless". Drawn quieter than the two above,
 *   so the better tier still reads as better. Withheld when the caller
 *   knows the analysis found a probable fake lossless
 *   (`probableFakeLossless`), and when no codec is passed (an album
 *   tile only knows its best depth and rate).
 *
 * The labels stay as they are in every language, like the "Hi-Res" mark
 * itself.
 *
 * Three visual variants:
 * - `overlay` (default) sits on top of an album cover (top-left
 *   corner, drop shadow);
 * - `inline` is for sidebar / row contexts where the badge sits next
 *   to text;
 * - `text` is the minimal green text label used under the artist name
 *   in the player bar — no background pill so it nests cleanly inside
 *   dense metadata.
 *
 * Globally hidden by the per-profile
 * `profile_setting['ui.show_hi_res_badge']` toggle — flipping that off
 * returns `null` everywhere this component is mounted.
 */
interface HiResBadgeProps {
  bitDepth: number | null;
  sampleRate: number | null;
  /** Codec label from the scanner (e.g. "FLAC", "DSD128"). */
  codec?: string | null;
  /** The track's analysis found a lossless file whose highs stop at a
   *  lossy encoder's wall. Pass it where the analysis has been read. */
  probableFakeLossless?: boolean;
  variant?: "overlay" | "inline" | "text";
  /** Override the visible text of the Hi-Res tier. Default is
   *  "Hi-Res {bitDepth}-bit". */
  label?: string;
}

export function HiResBadge({
  bitDepth,
  sampleRate,
  codec,
  probableFakeLossless,
  variant = "overlay",
  label,
}: HiResBadgeProps) {
  const { visible } = useHiResBadgeVisibility();
  const badge = qualityBadge(bitDepth, sampleRate, codec, probableFakeLossless);
  if (!badge || !visible) return null;
  const quiet = badge.kind === "lossless";
  const text =
    badge.kind === "dsd"
      ? (label ?? badge.label)
      : badge.kind === "hiRes"
        ? (label ?? `Hi-Res ${bitDepth}-bit`)
        : "Lossless";
  if (variant === "text") {
    return (
      <span
        data-quality-badge={badge.kind}
        className={`text-[10px] font-bold uppercase tracking-wider ${
          quiet
            ? "text-zinc-500 dark:text-zinc-400"
            : "text-emerald-500 dark:text-emerald-400"
        }`}
      >
        {text}
      </span>
    );
  }
  if (variant === "inline") {
    return (
      <span
        data-quality-badge={badge.kind}
        className={`inline-flex shrink-0 items-center px-2 py-0.5 rounded-full text-[10px] font-bold tracking-wide ${
          quiet
            ? "bg-transparent text-emerald-700 ring-1 ring-inset ring-emerald-500/60 dark:text-emerald-300"
            : "bg-emerald-500 text-white"
        }`}
      >
        {text}
      </span>
    );
  }
  return (
    <div
      data-quality-badge={badge.kind}
      className={`absolute top-2 start-2 px-2 py-0.5 rounded-full text-[10px] font-bold shadow-md tracking-wide pointer-events-none select-none ${
        quiet ? "bg-white/90 text-zinc-900" : "bg-emerald-500 text-white"
      }`}
    >
      {text}
    </div>
  );
}
