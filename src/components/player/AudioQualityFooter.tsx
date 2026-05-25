import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { QueueTrackPayload } from "../../lib/tauri/player";
import { isHiRes } from "../../lib/hiRes";
import { AudioPipelinePopover } from "./AudioPipelinePopover";

interface AudioQualityFooterProps {
  track: QueueTrackPayload | null;
}

// Small open / close delays so brushing the footer doesn't make the
// popover flicker, and so leaving the gap between footer and popover
// for a few frames doesn't immediately dismiss it.
const HOVER_OPEN_DELAY_MS = 120;
const HOVER_CLOSE_DELAY_MS = 200;

/**
 * Thin status strip below the PlayerBar that surfaces the source
 * file's audio specs — sample rate, bitrate, file size on the left
 * and codec / bit depth / sample rate again on the right (the
 * compact "FLAC · 24bit · 44kHz" pill that audiophiles recognise
 * from RustMusic and similar players).
 *
 * On hover (or keyboard focus) opens [`AudioPipelinePopover`] above
 * the strip with the full Source → DSP chips → Output breakdown so
 * the user can see exactly what's going on (bit-perfect? resampling?
 * downmix? EQ active?). The popover unmounts on leave so its data
 * never lingers — the engine state can change between two openings
 * (EQ flipped, device switched) and we want every read to be fresh.
 *
 * Hidden when no track is loaded and gracefully omits any chunk
 * whose underlying value is missing — a 320 kbps MP3 will still
 * show its bitrate even without bit_depth.
 */
export function AudioQualityFooter({ track }: AudioQualityFooterProps) {
  const { t } = useTranslation();
  const [isPopoverOpen, setIsPopoverOpen] = useState(false);
  const openTimerRef = useRef<number | null>(null);
  const closeTimerRef = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (openTimerRef.current != null) window.clearTimeout(openTimerRef.current);
      if (closeTimerRef.current != null)
        window.clearTimeout(closeTimerRef.current);
    },
    [],
  );

  const cancelTimers = () => {
    if (openTimerRef.current != null) {
      window.clearTimeout(openTimerRef.current);
      openTimerRef.current = null;
    }
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  };

  const scheduleOpen = () => {
    if (!track || isPopoverOpen) return;
    cancelTimers();
    openTimerRef.current = window.setTimeout(() => {
      setIsPopoverOpen(true);
    }, HOVER_OPEN_DELAY_MS);
  };

  const scheduleClose = () => {
    cancelTimers();
    closeTimerRef.current = window.setTimeout(() => {
      setIsPopoverOpen(false);
    }, HOVER_CLOSE_DELAY_MS);
  };

  if (!track) {
    return (
      <div className="h-5 px-4 border-t border-zinc-100 dark:border-zinc-800/60" />
    );
  }

  const sampleRateKHz = track.sample_rate
    ? (track.sample_rate / 1000).toFixed(1).replace(/\.0$/, "")
    : null;
  const sizeMB =
    track.file_size > 0 ? Math.round(track.file_size / (1024 * 1024)) : null;

  const leftBits: string[] = [];
  if (sampleRateKHz) leftBits.push(`${sampleRateKHz} kHz`);
  if (track.bitrate) {
    // RustMusic convention — show Mb/s when ≥ 1000 kbps so 24-bit
    // 192 kHz lossless reads "9.2 Mb/s" rather than a wall of digits.
    leftBits.push(
      track.bitrate >= 1000
        ? `${(track.bitrate / 1000).toFixed(track.bitrate >= 10000 ? 1 : 2).replace(/\.?0+$/, "")} Mb/s`
        : `${track.bitrate} kb/s`,
    );
  }
  if (sizeMB != null) leftBits.push(`${sizeMB} Mo`);

  const rightBits: string[] = [];
  if (track.codec) rightBits.push(track.codec);
  if (track.bit_depth) rightBits.push(`${track.bit_depth}bit`);
  if (sampleRateKHz) rightBits.push(`${sampleRateKHz}kHz`);

  const hiRes = isHiRes(track.bit_depth, track.sample_rate);

  return (
    <div
      className="relative h-5 px-4 flex items-center justify-between text-[10px] text-zinc-500 dark:text-zinc-400 border-t border-zinc-100 dark:border-zinc-800/60 bg-white dark:bg-surface-dark-elevated cursor-help"
      onMouseEnter={scheduleOpen}
      onMouseLeave={scheduleClose}
      onFocus={scheduleOpen}
      onBlur={scheduleClose}
      tabIndex={0}
      aria-label={t("playerBar.pipeline.openHint")}
    >
      <span className="tabular-nums truncate">{leftBits.join(" · ")}</span>
      <span className="flex items-center gap-2 tabular-nums">
        {hiRes && (
          <span className="px-1.5 py-0.5 rounded text-[10px] font-bold bg-emerald-500 text-white">
            Hi-Res
          </span>
        )}
        <span className="truncate">{rightBits.join(" · ")}</span>
      </span>
      {isPopoverOpen && (
        <div
          onMouseEnter={cancelTimers}
          onMouseLeave={scheduleClose}
          // The wrapper just sits inside the footer's relative
          // container; the popover itself is `absolute bottom-full
          // right-0 mb-2`, see AudioPipelinePopover.
          className="contents"
        >
          <AudioPipelinePopover track={track} />
        </div>
      )}
    </div>
  );
}
