import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FileAudio, Cpu, Speaker, Sparkles } from "lucide-react";
import type { QueueTrackPayload } from "../../lib/tauri/player";
import {
  playerGetAudioSettings,
  playerGetState,
} from "../../lib/tauri/player";
import { playerGetEq } from "../../lib/tauri/eq";
import { usePlayer } from "../../hooks/usePlayer";

interface AudioPipelinePopoverProps {
  track: QueueTrackPayload;
  /** Mounted only while open — the caller toggles it on pointerenter
   *  / pointerleave with a short delay so brushing the footer doesn't
   *  flicker the popover open. */
  onClose?: () => void;
}

interface PipelineSnapshot {
  outputSampleRate: number;
  outputChannels: number;
  eqEnabled: boolean;
  normalize: boolean;
  replaygain: boolean;
  mono: boolean;
}

/**
 * Maps a channel count (1-8) onto the layout strings audiophile
 * players use (Mono / Stereo / 3.0 / 4.0 / 5.0 / 5.1 / 6.1 / 7.1).
 * Unknown counts fall back to `${n}ch` so we never silently drop the
 * information.
 */
function formatChannelLayout(channels: number | null | undefined): string {
  if (channels == null || channels <= 0) return "—";
  switch (channels) {
    case 1:
      return "Mono";
    case 2:
      return "Stereo";
    case 3:
      return "3.0";
    case 4:
      return "4.0";
    case 5:
      return "5.0";
    case 6:
      return "5.1";
    case 7:
      return "6.1";
    case 8:
      return "7.1";
    default:
      return `${channels}ch`;
  }
}

/** Bitrate as kb/s under 1000, Mb/s with one decimal otherwise. */
function formatBitrate(kbps: number | null | undefined): string | null {
  if (kbps == null || kbps <= 0) return null;
  if (kbps >= 1000) {
    const mbps = kbps / 1000;
    return `${mbps.toFixed(mbps >= 10 ? 1 : 2).replace(/\.?0+$/, "")} Mb/s`;
  }
  return `${kbps} kb/s`;
}

function formatSampleRate(hz: number | null | undefined): string | null {
  if (hz == null || hz <= 0) return null;
  const khz = hz / 1000;
  // Drop a trailing ".0" so 48000 reads "48 kHz" rather than "48.0 kHz",
  // but keep the decimal for 44.1 / 88.2 / 176.4 family.
  return `${khz.toFixed(1).replace(/\.0$/, "")} kHz`;
}

/**
 * Hover popover that surfaces the full audio pipeline (Source →
 * Pipeline DSP chips → Output) for the currently playing track. Lives
 * above [`AudioQualityFooter`] which is its only trigger today.
 *
 * Strategy on data freshness: hydrates the output-side + DSP flags on
 * mount via `playerGetState` / `playerGetAudioSettings` / `playerGetEq`
 * so the popover always reflects the real engine state rather than
 * stale React state (e.g. EQ may have been flipped from the dedicated
 * popover seconds ago). Cheap calls — atomic loads on the Rust side —
 * so we don't bother caching across hover sessions.
 */
export function AudioPipelinePopover({ track }: AudioPipelinePopoverProps) {
  const { t } = useTranslation();
  const { playbackSpeed } = usePlayer();
  const [snap, setSnap] = useState<PipelineSnapshot | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [stateSnap, audioSettings, eqSnap] = await Promise.all([
          playerGetState(),
          playerGetAudioSettings(),
          playerGetEq(),
        ]);
        if (cancelled) return;
        setSnap({
          outputSampleRate: stateSnap.sample_rate,
          outputChannels: stateSnap.channels,
          eqEnabled: eqSnap.enabled,
          normalize: audioSettings.normalize,
          replaygain: audioSettings.replaygain,
          mono: audioSettings.mono,
        });
      } catch (err) {
        console.error("[AudioPipelinePopover] hydrate failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const sourceCodec = track.codec ?? "—";
  const sourceRateLabel = formatSampleRate(track.sample_rate);
  const sourceBitDepth = track.bit_depth ? `${track.bit_depth} bit` : null;
  const sourceBitrateLabel = formatBitrate(track.bitrate);
  const sourceChannelsLabel = formatChannelLayout(track.channels);

  const outputRateLabel = snap ? formatSampleRate(snap.outputSampleRate) : null;
  const outputChannelsLabel = snap
    ? formatChannelLayout(snap.outputChannels)
    : null;

  // Pipeline-effect detection. Anything in this set means the stream
  // isn't bit-perfect — used to decide whether to surface the green
  // "Bit-perfect" pill at the bottom.
  const isDsd = (track.codec ?? "").toUpperCase().includes("DSD");
  const isResampling =
    snap != null &&
    track.sample_rate != null &&
    snap.outputSampleRate > 0 &&
    snap.outputSampleRate !== track.sample_rate;
  const isDownmixing =
    snap != null &&
    track.channels != null &&
    snap.outputChannels > 0 &&
    snap.outputChannels < track.channels;
  const isSpeedShifted = Math.abs(playbackSpeed - 1.0) > 0.001;
  const isEq = snap?.eqEnabled ?? false;
  const isNormalize = snap?.normalize ?? false;
  const isReplayGain = snap?.replaygain ?? false;
  const isMono = snap?.mono ?? false;
  const isBitPerfect =
    snap != null &&
    !isDsd &&
    !isResampling &&
    !isDownmixing &&
    !isSpeedShifted &&
    !isEq &&
    !isNormalize &&
    !isReplayGain &&
    !isMono;

  const chips: Array<{ key: string; label: string; tone: "dsp" | "convert" }> =
    [];
  if (isDsd)
    chips.push({
      key: "dsd",
      label: t("playerBar.pipeline.chip.dsdToPcm"),
      tone: "convert",
    });
  if (isResampling)
    chips.push({
      key: "resample",
      label: t("playerBar.pipeline.chip.resample"),
      tone: "convert",
    });
  if (isDownmixing)
    chips.push({
      key: "downmix",
      label: t("playerBar.pipeline.chip.downmix"),
      tone: "convert",
    });
  if (isEq)
    chips.push({
      key: "eq",
      label: t("playerBar.pipeline.chip.eq"),
      tone: "dsp",
    });
  if (isReplayGain)
    chips.push({
      key: "rg",
      label: t("playerBar.pipeline.chip.replayGain"),
      tone: "dsp",
    });
  if (isNormalize)
    chips.push({
      key: "norm",
      label: t("playerBar.pipeline.chip.normalize"),
      tone: "dsp",
    });
  if (isMono)
    chips.push({
      key: "mono",
      label: t("playerBar.pipeline.chip.mono"),
      tone: "dsp",
    });
  if (isSpeedShifted)
    chips.push({
      key: "speed",
      label: t("playerBar.pipeline.chip.speed", {
        value: playbackSpeed.toFixed(2).replace(/\.?0+$/, ""),
      }),
      tone: "dsp",
    });

  return (
    // Hover-triggered informational popover — not a dialog. Skipping
    // `role="dialog"` (and the associated `useModalA11y` focus trap)
    // by design: the popover holds no actionable controls, dismisses
    // automatically on hover-leave, and trapping focus inside it
    // would feel broken when the user only meant to glance at the
    // specs. `role="group"` keeps the heading + chip structure
    // grouped for assistive tech without claiming dialog semantics.
    <div
      role="group"
      aria-label={t("playerBar.pipeline.title")}
      className="absolute bottom-full right-4 mb-3 w-80 p-4 rounded-xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-xl z-50 text-left"
    >
      <div className="text-xs font-bold uppercase tracking-widest text-zinc-400 mb-3">
        {t("playerBar.pipeline.title")}
      </div>

      {/* Source */}
      <div className="flex gap-3 items-start">
        <FileAudio
          size={16}
          className="mt-0.5 shrink-0 text-zinc-400"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <div className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500 dark:text-zinc-400">
            {t("playerBar.pipeline.source")}
          </div>
          <div className="text-sm text-zinc-800 dark:text-zinc-100 truncate">
            {sourceCodec}
          </div>
          <div className="text-xs text-zinc-500 dark:text-zinc-400 truncate">
            {[
              sourceRateLabel,
              sourceBitDepth,
              sourceBitrateLabel,
              sourceChannelsLabel,
            ]
              .filter(Boolean)
              .join(" · ")}
          </div>
        </div>
      </div>

      {/* Pipeline DSP chips */}
      <div className="flex gap-3 items-start mt-3">
        <Cpu
          size={16}
          className="mt-0.5 shrink-0 text-zinc-400"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <div className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500 dark:text-zinc-400">
            {t("playerBar.pipeline.processing")}
          </div>
          {chips.length === 0 ? (
            <div className="text-xs text-zinc-500 dark:text-zinc-400 italic mt-0.5">
              {t("playerBar.pipeline.noProcessing")}
            </div>
          ) : (
            <div className="flex flex-wrap gap-1 mt-1">
              {chips.map((chip) => (
                <span
                  key={chip.key}
                  className={`px-1.5 py-0.5 rounded text-[10px] font-semibold ${
                    chip.tone === "convert"
                      ? "bg-amber-500/15 text-amber-700 dark:text-amber-400 border border-amber-500/30"
                      : "bg-sky-500/15 text-sky-700 dark:text-sky-400 border border-sky-500/30"
                  }`}
                >
                  {chip.label}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Output */}
      <div className="flex gap-3 items-start mt-3">
        <Speaker
          size={16}
          className="mt-0.5 shrink-0 text-zinc-400"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <div className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500 dark:text-zinc-400">
            {t("playerBar.pipeline.output")}
          </div>
          <div className="text-sm text-zinc-800 dark:text-zinc-100 truncate">
            {outputRateLabel ?? t("playerBar.pipeline.loading")}
          </div>
          {outputChannelsLabel && (
            <div className="text-xs text-zinc-500 dark:text-zinc-400 truncate">
              {outputChannelsLabel}
            </div>
          )}
        </div>
      </div>

      {/* Bit-perfect pill — only shown when the pipeline is fully
          transparent. Sits at the bottom so the visual cue is the
          last thing the audiophile reads. */}
      {isBitPerfect && (
        <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center gap-2">
          <Sparkles
            size={14}
            className="text-emerald-500"
            aria-hidden="true"
          />
          <span className="text-xs font-semibold text-emerald-600 dark:text-emerald-400">
            {t("playerBar.pipeline.bitPerfect")}
          </span>
        </div>
      )}
    </div>
  );
}
