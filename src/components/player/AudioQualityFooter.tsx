import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AudioWaveform, ChevronUp } from "lucide-react";
import type { QueueTrackPayload } from "../../lib/tauri/player";
import { qualityBadge } from "../../lib/hiRes";
import { fileKind, formatBitrate, formatKHz } from "../../lib/audioQuality";
import { usePlayer } from "../../hooks/usePlayer";
import { useHiResBadgeVisibility } from "../../hooks/useHiResBadgeVisibility";
import { HiResBadge } from "../common/HiResBadge";
import { AudioPipelinePopover } from "./AudioPipelinePopover";

interface AudioQualityFooterProps {
  track: QueueTrackPayload | null;
  /** The analysis caught the track as a probable fake lossless; read
   *  once by the player bar (`useProbableFakeLossless`). */
  probableFakeLossless?: boolean;
}

// Small open / close delays so brushing the footer doesn't make the
// popover flicker, and so leaving the gap between footer and popover
// for a few frames doesn't immediately dismiss it.
const HOVER_OPEN_DELAY_MS = 120;
const HOVER_CLOSE_DELAY_MS = 200;

/**
 * Thin status strip below the PlayerBar that says what the file is: its
 * quality badge (DSD / Hi-Res / Lossless, the same `HiResBadge` the
 * lists carry) or, when there is none to show, the word a listener uses
 * — "Lossy", "Lossless", "Probably fake lossless" — with its figures
 * (codec, bit depth, sample rate, and the bitrate of a lossy file) in a
 * quieter ink beside it. When the engine converts the rate for the
 * output, a second segment says so in words, with the output rate as
 * its figure. The output-mode chip sits on the right.
 *
 * On hover (or keyboard focus) opens [`AudioPipelinePopover`] above
 * the strip: the file, every stage of the engine and the output as
 * cards, each saying whether it changes the sound. The popover
 * unmounts on leave so its data never lingers — the engine state can
 * change between two openings (EQ flipped, device switched) and we
 * want every read to be fresh.
 *
 * Hidden when no track is loaded and gracefully omits any chunk
 * whose underlying value is missing — a 320 kbps MP3 will still
 * show its bitrate even without bit_depth.
 */
export function AudioQualityFooter({
  track,
  probableFakeLossless,
}: AudioQualityFooterProps) {
  const { t, i18n } = useTranslation();
  const { deviceSampleRate, outputMode } = usePlayer();
  const { visible: badgesVisible } = useHiResBadgeVisibility();
  const [isPopoverOpen, setIsPopoverOpen] = useState(false);
  // The strip itself, held in state rather than a ref so the popover,
  // which is portalled and places itself from this element's box, gets
  // it on the render that mounts it.
  const [strip, setStrip] = useState<HTMLDivElement | null>(null);
  const openTimerRef = useRef<number | null>(null);
  const closeTimerRef = useRef<number | null>(null);

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

  useEffect(
    () => () => {
      cancelTimers();
    },
    [],
  );

  // When the current track clears (queue ended, user stopped, profile
  // switch) we must force the popover closed and drop any pending
  // open/close timers. Otherwise `isPopoverOpen` stays true while the
  // component renders the empty-footer fallback, and as soon as a new
  // track arrives the popover would pop back open at the previous
  // position without the user hovering again.
  useEffect(() => {
    if (track == null) {
      cancelTimers();
      // Resetting derived UI state in response to a prop change is the
      // standard exception to the "no setState in effect" rule —
      // there's no user gesture to model and we don't want to push
      // this into a `key` reset that nukes the entire component.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setIsPopoverOpen(false);
    }
  }, [track]);

  const scheduleOpen = () => {
    // Cancel any pending close timer first — re-hovering the trigger
    // while the close timer is ticking but before it fires must keep
    // the popover open. The guard below then short-circuits if there's
    // nothing left to schedule (no track, or already open), but the
    // pending close still needed to die.
    cancelTimers();
    if (!track || isPopoverOpen) return;
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
      <div className="h-6 px-4 border-t border-zinc-100 dark:border-zinc-800/60" />
    );
  }

  // Resampling check works on raw Hz, not the formatted kHz strings.
  // Otherwise a source like 44 056 Hz (rare non-standard CD rip) on a
  // 44 100 Hz device would silently round to "44.1" on both sides and
  // hide the resampling. `track.sample_rate` and `deviceSampleRate`
  // are both already integers from the engine. A DoP stream ships at
  // dsd_rate / 16, which never equals the stored DSD rate, and reaches the
  // DAC untouched: not a conversion, as the pipeline popover already says.
  const isResampling =
    outputMode !== "dop" &&
    track.sample_rate != null &&
    track.sample_rate > 0 &&
    deviceSampleRate != null &&
    deviceSampleRate > 0 &&
    track.sample_rate !== deviceSampleRate;
  const lang = i18n.language;
  const kind = fileKind(track.codec, track.bit_depth);
  const isFakeLossless = kind === "lossless" && probableFakeLossless === true;
  // The badge says what the word would, so the strip shows one or the
  // other: the badge when the file earns one and badges are on, the word
  // otherwise (a lossy file, badges hidden, a probable fake).
  const showsBadge =
    badgesVisible &&
    qualityBadge(
      track.bit_depth,
      track.sample_rate,
      track.codec,
      probableFakeLossless,
    ) != null;
  const word = isFakeLossless
    ? t("playerBar.pipeline.kind.fakeLossless")
    : kind
      ? t(`playerBar.pipeline.kind.${kind}`)
      : null;

  // The file first, its badge or its word, then its figures in a quieter
  // ink. A lossy file's bitrate says how much was kept, so it is shown;
  // a lossless file's says nothing a listener can use.
  const fileFigures = [
    track.codec,
    track.bit_depth
      ? t("playerBar.pipeline.bits", { value: track.bit_depth })
      : null,
    formatKHz(track.sample_rate, lang),
    kind === "lossy" ? formatBitrate(track.bitrate, lang) : null,
  ]
    .filter(Boolean)
    .join(" · ");
  const deviceRate = isResampling ? formatKHz(deviceSampleRate, lang) : null;

  // What the output really engaged as (#597). Shared mode carries no
  // chip: it is the normal case, and a badge on every track would be
  // noise rather than information. The three that say something do —
  // including the one that says the user's choice was refused, which
  // until now only ever reached a log line.
  const outputBadge =
    outputMode === "dop"
      ? { label: t("playerBar.outputMode.dop"), tone: "emerald" as const }
      : outputMode === "exclusive"
        ? {
            label: t("playerBar.outputMode.exclusive"),
            tone: "emerald" as const,
          }
        : outputMode === "exclusive-refused"
          ? {
              label: t("playerBar.outputMode.exclusiveRefused"),
              tone: "amber" as const,
            }
          : null;

  return (
    <div
      ref={setStrip}
      data-audio-quality
      className="relative h-6 px-4 flex items-center justify-between gap-3 text-[11px] text-zinc-500 dark:text-zinc-400 border-t border-zinc-100 dark:border-zinc-800/60 bg-white dark:bg-surface-dark-elevated cursor-help"
      onMouseEnter={scheduleOpen}
      onMouseLeave={scheduleClose}
      onFocus={scheduleOpen}
      onBlur={scheduleClose}
      tabIndex={0}
      aria-label={t("playerBar.pipeline.openHint")}
    >
      <span className="flex min-w-0 items-center gap-3">
        <span className="flex min-w-0 items-center gap-1.5">
          {showsBadge ? (
            <HiResBadge
              bitDepth={track.bit_depth}
              sampleRate={track.sample_rate}
              codec={track.codec}
              probableFakeLossless={probableFakeLossless}
              variant="inline"
            />
          ) : (
            word && (
              <span
                data-quality-word
                data-quality-warn={isFakeLossless || undefined}
                className={`shrink-0 font-semibold ${
                  isFakeLossless
                    ? "text-amber-600 dark:text-amber-400"
                    : "text-zinc-700 dark:text-zinc-200"
                }`}
              >
                {word}
              </span>
            )
          )}
          {fileFigures && (
            <span data-quality-figures className="truncate tabular-nums">
              {fileFigures}
            </span>
          )}
        </span>
        {deviceRate && (
          <span className="flex min-w-0 items-center gap-1.5">
            <AudioWaveform
              size={12}
              className="shrink-0 text-sky-500"
              aria-hidden="true"
            />
            <span
              data-quality-word
              className="shrink-0 font-semibold text-zinc-700 dark:text-zinc-200"
            >
              {t("playerBar.quality.converted")}
            </span>
            <span data-quality-figures className="truncate tabular-nums">
              {deviceRate}
            </span>
          </span>
        )}
      </span>
      <span className="flex shrink-0 items-center gap-2">
        {outputBadge && (
          <span
            className={`px-1.5 py-0.5 rounded text-[10px] font-semibold ${
              outputBadge.tone === "emerald"
                ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                : "bg-amber-500/15 text-amber-600 dark:text-amber-400"
            }`}
          >
            {outputBadge.label}
          </span>
        )}
        <span className="flex items-center gap-0.5 font-medium">
          {t("playerBar.quality.details")}
          <ChevronUp size={12} aria-hidden="true" />
        </span>
      </span>
      {isPopoverOpen && (
        <AudioPipelinePopover
          track={track}
          probableFakeLossless={probableFakeLossless}
          anchor={strip}
          onPointerEnter={cancelTimers}
          onPointerLeave={scheduleClose}
        />
      )}
    </div>
  );
}
