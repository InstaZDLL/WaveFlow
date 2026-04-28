import type { QueueTrackPayload } from "../../lib/tauri/player";
import { isHiRes } from "../../lib/hiRes";

interface AudioQualityFooterProps {
  track: QueueTrackPayload | null;
}

/**
 * Thin status strip below the PlayerBar that surfaces the source
 * file's audio specs — sample rate, bitrate, file size on the left
 * and codec / bit depth / sample rate again on the right (the
 * compact "FLAC · 24bit · 44kHz" pill that audiophiles recognise
 * from RustMusic and similar players).
 *
 * Hidden when no track is loaded and gracefully omits any chunk
 * whose underlying value is missing — a 320 kbps MP3 will still
 * show its bitrate even without bit_depth.
 */
export function AudioQualityFooter({ track }: AudioQualityFooterProps) {
  if (!track) {
    return <div className="h-6 px-6 border-t border-zinc-100 dark:border-zinc-800/60" />;
  }

  const sampleRateKHz = track.sample_rate
    ? (track.sample_rate / 1000).toFixed(1).replace(/\.0$/, "")
    : null;
  const sizeMB =
    track.file_size > 0 ? Math.round(track.file_size / (1024 * 1024)) : null;

  const leftBits: string[] = [];
  if (sampleRateKHz) leftBits.push(`${sampleRateKHz} kHz`);
  if (track.bitrate) leftBits.push(`${track.bitrate} kb/s`);
  if (sizeMB != null) leftBits.push(`${sizeMB} Mo`);

  const rightBits: string[] = [];
  if (track.codec) rightBits.push(track.codec);
  if (track.bit_depth) rightBits.push(`${track.bit_depth}bit`);
  if (sampleRateKHz) rightBits.push(`${sampleRateKHz}kHz`);

  const hiRes = isHiRes(track.bit_depth, track.sample_rate);

  return (
    <div className="h-6 px-6 flex items-center justify-between text-[11px] text-zinc-500 dark:text-zinc-400 border-t border-zinc-100 dark:border-zinc-800/60 bg-[#FAFAFA] dark:bg-surface-dark-elevated">
      <span className="tabular-nums truncate">{leftBits.join(" · ")}</span>
      <span className="flex items-center gap-2 tabular-nums">
        {hiRes && (
          <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-emerald-500 text-white">
            Hi-Res
          </span>
        )}
        <span className="truncate">{rightBits.join(" · ")}</span>
      </span>
    </div>
  );
}
