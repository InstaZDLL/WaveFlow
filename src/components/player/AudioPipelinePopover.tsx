import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FileAudio, Cpu, Speaker, Sparkles } from "lucide-react";
import type { QueueTrackPayload } from "../../lib/tauri/player";
import { playerGetAudioSettings, playerGetState } from "../../lib/tauri/player";
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
  /** Native DSD via DoP actually engaged for the current track (#495). */
  dopActive: boolean;
  /** The output really owns the device (WASAPI Exclusive today). */
  exclusiveActive: boolean;
  /** Which track was playing when this was read. See `snap` below. */
  forTrackId: number;
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
 * Strategy on data freshness: reads the output-side + DSP flags from
 * the engine via `playerGetState` / `playerGetAudioSettings` /
 * `playerGetEq` rather than trusting React state (the EQ may have been
 * flipped from the dedicated popover seconds ago). Cheap calls —
 * atomic loads on the Rust side — so we don't bother caching across
 * hover sessions.
 *
 * That read is repeated whenever it can have gone stale *while the
 * popover is open*, which is the part a mount-only hydration got wrong:
 * the popover lives for as long as the pointer rests on the footer, and
 * a track ending under it swaps `track` for the next one while every
 * output-side field still described the previous stream. Pairing new
 * metadata with an old snapshot is how a verdict gets made about a
 * stream nobody measured — a DoP track handing its `dopActive` to the
 * PCM track after it would have exempted that one from the rate check
 * and badged it bit-perfect.
 */
export function AudioPipelinePopover({ track }: AudioPipelinePopoverProps) {
  const { t } = useTranslation();
  const { playbackSpeed } = usePlayer();
  const [rawSnap, setRawSnap] = useState<PipelineSnapshot | null>(null);
  // A track change and an output rebuild can put two reads in flight at
  // once; the older one resolving second would pin a stale output
  // format under fresh metadata. Same guard, and same reason, as
  // `deviceRefreshTokenRef` in PlayerContext.
  const readTokenRef = useRef(0);

  const trackId = track.id;
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;

    const read = async () => {
      // A read from an effect that has already been torn down must not
      // touch the shared token. `await listen()` can resolve after a
      // track change, and the tail of that dead run would otherwise
      // bump the token past the *new* effect's in-flight read and
      // silence it — leaving the popover on "loading" until the next
      // output rebuild or a fresh hover.
      if (cancelled) return;
      const token = ++readTokenRef.current;
      try {
        const [stateSnap, audioSettings, eqSnap] = await Promise.all([
          playerGetState(),
          playerGetAudioSettings(),
          playerGetEq(),
        ]);
        if (cancelled || token !== readTokenRef.current) return;
        setRawSnap({
          outputSampleRate: stateSnap.sample_rate,
          outputChannels: stateSnap.channels,
          eqEnabled: eqSnap.enabled,
          normalize: audioSettings.normalize,
          replaygain: audioSettings.replaygain,
          mono: audioSettings.mono,
          dopActive: stateSnap.dop_active,
          exclusiveActive: stateSnap.exclusive_active,
          forTrackId: trackId,
        });
      } catch (err) {
        console.error("[AudioPipelinePopover] hydrate failed", err);
      }
    };

    void (async () => {
      // Subscribe before the first read, not after. The output is
      // rebuilt *after* the track changes — a DoP engage, a WASAPI
      // exclusive re-open at the new native rate — so the rebuild can
      // land in the window between the two, and that is the one event
      // we cannot afford to miss (see the "subscribe first, then
      // snapshot" invariant). `player:audio-mode-changed` carries no
      // payload; every engine path that stores a new output mode emits
      // it.
      try {
        const stop = await listen("player:audio-mode-changed", () => {
          void read();
        });
        if (cancelled) {
          stop();
        } else {
          unlisten = stop;
        }
      } catch (err) {
        // A failed subscription must not cost us the snapshot itself:
        // a stale-after-a-rebuild popover still beats an empty one.
        console.error("[AudioPipelinePopover] listen failed", err);
      }
      void read();
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [trackId]);

  // A snapshot taken for the previous track describes the previous
  // stream, so it counts as absent rather than being paired with this
  // track's metadata. Everything below is already written to say
  // "loading" and to withhold the verdict when there is no snapshot,
  // which is exactly the right behaviour for the moment between a
  // track change and the read that follows it.
  const snap = rawSnap?.forTrackId === trackId ? rawSnap : null;

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
  // Native DSD via DoP (#495): the DAC decodes the 1-bit stream itself,
  // so nothing on our side converts it — it counts as bit-perfect and
  // shows a distinct pill instead of the "DSD → PCM" convert chip.
  const isDopNative = isDsd && (snap?.dopActive ?? false);
  // A DoP stream reaches the DAC untouched, so the nominal rate/channel
  // comparison below (DoP ships at dsd_rate/16, which never equals the
  // stored DSD rate) must not be read as resampling / downmixing.
  const isResampling =
    !isDopNative &&
    snap != null &&
    track.sample_rate != null &&
    snap.outputSampleRate > 0 &&
    snap.outputSampleRate !== track.sample_rate;
  const isDownmixing =
    !isDopNative &&
    snap != null &&
    track.channels != null &&
    snap.outputChannels > 0 &&
    snap.outputChannels < track.channels;
  // Every DSP stage below is bypassed on the DoP path — the decoder
  // pushes the 24-bit words straight to the ring, so EQ / normalize /
  // ReplayGain / mono / speed are all inert whatever the preference
  // says. Reading the raw preferences here would badge the stream with
  // effects that aren't running, and cancel the bit-perfect pill.
  const isSpeedShifted = !isDopNative && Math.abs(playbackSpeed - 1.0) > 0.001;
  const isEq = !isDopNative && (snap?.eqEnabled ?? false);
  const isNormalize = !isDopNative && (snap?.normalize ?? false);
  const isReplayGain = !isDopNative && (snap?.replaygain ?? false);
  const isMono = !isDopNative && (snap?.mono ?? false);
  // `isResampling` / `isDownmixing` answer "no" both when the format
  // matches and when we never learned the source format — a track whose
  // scan recorded no sample rate would otherwise sail through them into
  // a verdict nothing checked. The claim needs the comparison to have
  // actually happened. DoP is exempt: it ships at `dsd_rate / 16`, so
  // the nominal rates never match by construction.
  const isFormatProven =
    snap != null &&
    // Channels have to match outright: an output wider than the source
    // means the engine is routing or padding, which is not "untouched"
    // however clean the rest of the chain is.
    track.channels != null &&
    track.channels > 0 &&
    snap.outputChannels === track.channels &&
    // Only the *rate* comparison is exempt for DoP, and only because a
    // DoP stream is carried at `dsd_rate / 16` — the nominal rates are
    // never equal by construction. Everything else still has to hold.
    (isDopNative ||
      (track.sample_rate != null &&
        track.sample_rate > 0 &&
        snap.outputSampleRate > 0 &&
        snap.outputSampleRate === track.sample_rate));
  // Nothing in our own pipeline is touching the samples.
  const isUnprocessed =
    snap != null &&
    isFormatProven &&
    // Native DoP is transparent; only DSD → PCM conversion breaks it.
    (!isDsd || isDopNative) &&
    !isResampling &&
    !isDownmixing &&
    !isSpeedShifted &&
    !isEq &&
    !isNormalize &&
    !isReplayGain &&
    !isMono;
  // …but bit-perfect also means nothing *downstream* touches them, and
  // that only holds when the stream owns the device. A shared-mode
  // stream at the same nominal rate still goes through the system
  // mixer, which re-clocks it and mixes in whatever else is playing —
  // the pill used to claim bit-perfect for exactly that case. DoP
  // implies an exclusive backend, so it qualifies on its own.
  const isBitPerfect =
    isUnprocessed && (isDopNative || (snap?.exclusiveActive ?? false));

  const chips: Array<{ key: string; label: string; tone: "dsp" | "convert" }> =
    [];
  if (isDopNative)
    chips.push({
      key: "dop",
      label: t("playerBar.pipeline.chip.dopNative"),
      tone: "dsp",
    });
  else if (isDsd)
    chips.push({
      key: "dsd",
      label: t("playerBar.pipeline.chip.dsdToPcm"),
      tone: "convert",
    });
  if (isResampling)
    chips.push({
      key: "resample",
      // Surface the actual from→to rates so the chip mirrors the
      // footer's `48 kHz → 44.1 kHz` arrow. Stripped of the unit on
      // the left side since the `kHz` suffix on the right reads for
      // both (matches the way audio devices are usually labelled).
      label: `${t("playerBar.pipeline.chip.resample")} ${(
        track.sample_rate! / 1000
      )
        .toFixed(1)
        .replace(/\.0$/, "")} → ${formatSampleRate(snap!.outputSampleRate)}`,
      tone: "convert",
    });
  if (isDownmixing)
    chips.push({
      key: "downmix",
      label: `${t("playerBar.pipeline.chip.downmix")} ${formatChannelLayout(track.channels)} → ${formatChannelLayout(snap!.outputChannels)}`,
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

      {/* Verdict pill — the last thing the audiophile reads, so it has
          to be the one we can actually stand behind: bit-perfect when
          the stream owns the device, "no processing" when our pipeline
          is transparent but the OS mixer still sits in the path. */}
      {isBitPerfect && (
        <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center gap-2">
          <Sparkles size={14} className="text-emerald-500" aria-hidden="true" />
          <span className="text-xs font-semibold text-emerald-600 dark:text-emerald-400">
            {t("playerBar.pipeline.bitPerfect")}
          </span>
        </div>
      )}
      {!isBitPerfect && isUnprocessed && (
        <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center gap-2">
          <Speaker size={14} className="text-zinc-400" aria-hidden="true" />
          <span className="text-xs font-semibold text-zinc-500 dark:text-zinc-400">
            {t("playerBar.pipeline.sharedOutput")}
          </span>
        </div>
      )}
    </div>
  );
}
