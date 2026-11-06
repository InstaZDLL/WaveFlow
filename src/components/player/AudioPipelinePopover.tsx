import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  AudioWaveform,
  Disc3,
  FileAudio,
  Gauge,
  Merge,
  Music2,
  Scale,
  SlidersHorizontal,
  Sparkles,
  Speaker,
  Volume1,
  Volume2,
  type LucideIcon,
} from "lucide-react";
import type {
  OutputMode,
  QueueTrackPayload,
  ReplayGainMode,
} from "../../lib/tauri/player";
import { playerGetAudioSettings, playerGetState } from "../../lib/tauri/player";
import { playerGetEq } from "../../lib/tauri/eq";
import { usePlayer } from "../../hooks/usePlayer";
import { isPodcastTrack } from "../../lib/playerSources";
import {
  fileKind,
  formatBitrate,
  formatKHz,
  surroundLayout,
} from "../../lib/audioQuality";
import { HiResBadge } from "../common/HiResBadge";
import { useHiResBadgeVisibility } from "../../hooks/useHiResBadgeVisibility";
import { qualityBadge } from "../../lib/hiRes";
import { AudioPipelineCard, type StageImpact } from "./AudioPipelineCard";

interface AudioPipelinePopoverProps {
  track: QueueTrackPayload;
  /** The strip it opens from. The popover is portalled into `body` and
   *  placed above this element, right-aligned with it. */
  anchor: HTMLElement | null;
  /** Hover handlers of the strip, carried over to the popover so the
   *  pointer can travel from one to the other without closing it. */
  onPointerEnter: () => void;
  onPointerLeave: () => void;
  /** The analysis caught the track as a probable fake lossless. */
  probableFakeLossless?: boolean;
}

interface PipelineSnapshot {
  outputSampleRate: number;
  outputChannels: number;
  eqEnabled: boolean;
  normalize: boolean;
  replaygain: boolean;
  replaygainMode: ReplayGainMode;
  mono: boolean;
  /** Left / right balance in percent; 0 is the centre. */
  balance: number;
  /** Stereo width in percent; 100 is the source as it is. */
  stereoWidth: number;
  /** Native DSD via DoP actually engaged for the current track (#495). */
  dopActive: boolean;
  /** The output really owns the device (WASAPI Exclusive today). */
  exclusiveActive: boolean;
  /** What the output engaged as, read in the same snapshot as the rest. */
  outputMode: OutputMode;
  /** Which track was playing when this was read. See `snap` below. */
  forTrackId: number;
}

/** One stage card, before it is rendered. */
interface StageCard {
  key: string;
  icon: LucideIcon;
  title: string;
  body: string;
  figures?: string | null;
  impact: StageImpact;
}

/** Room kept between the strip and the popover, and around the window. */
const POPOVER_GAP_PX = 8;
const POPOVER_EDGE_PX = 16;

/**
 * Hover popover that walks the currently playing track through the
 * engine as a column of cards: the file, one card per stage that
 * touches the samples, the output. Each card says in plain words what
 * happens and whether it changes the sound, with the figures under it.
 * Opens above [`AudioQualityFooter`], its only trigger today, through a
 * portal: the player bar is a stacking context under the glassy skins.
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
export function AudioPipelinePopover({
  track,
  anchor,
  onPointerEnter,
  onPointerLeave,
  probableFakeLossless,
}: AudioPipelinePopoverProps) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const { playbackSpeed, pitchSemitones, volume, currentTrack, outputDevices } =
    usePlayer();
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
        // The engine's own idea of what is playing beats the prop when
        // the two disagree. A track that advanced *after* this read
        // started returns the new stream's output, and the prop only
        // catches up once `player:track-changed` has been through the
        // context and a render — so stamping this with the id we still
        // hold would pair one track's metadata with another's
        // measurement, which is the exact pairing the stamp exists to
        // prevent.
        //
        // Only a positive disagreement counts. `current_track` is null
        // for a Web Radio session (negative sentinel id, no library row)
        // and whenever the persisted queue cursor hasn't caught up with
        // the live engine — reading "no answer" as "another track"
        // would leave radio permanently unhydrated. The read the new
        // effect fires for itself is what fills the gap.
        const reported = stateSnap.current_track?.id;
        if (reported != null && reported !== trackId) return;
        setRawSnap({
          outputSampleRate: stateSnap.sample_rate,
          outputChannels: stateSnap.channels,
          eqEnabled: eqSnap.enabled,
          normalize: audioSettings.normalize,
          replaygain: audioSettings.replaygain,
          replaygainMode: audioSettings.replaygain_mode,
          mono: audioSettings.mono,
          balance: audioSettings.balance,
          stereoWidth: audioSettings.stereo_width,
          dopActive: stateSnap.dop_active,
          exclusiveActive: stateSnap.exclusive_active,
          outputMode: stateSnap.output_mode,
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
  // ReplayGain / mono / balance / width / speed / pitch are all inert whatever
  // the preference says. Reading the raw preferences here would badge the
  // stream with effects that aren't running, and cancel the bit-perfect
  // pill.
  const isSpeedShifted = !isDopNative && Math.abs(playbackSpeed - 1.0) > 0.001;
  const isTransposed =
    !isDopNative && !isPodcastTrack(currentTrack) && pitchSemitones !== 0;
  const isEq = !isDopNative && (snap?.eqEnabled ?? false);
  const isNormalize = !isDopNative && (snap?.normalize ?? false);
  const isReplayGain = !isDopNative && (snap?.replaygain ?? false);
  const isMono = !isDopNative && (snap?.mono ?? false);
  // Balance and width act on each left / right pair, so a single-channel
  // output has nothing for them to change. The width is also skipped for a mono
  // track and once the mono switch has folded the side away.
  const hasPair = (snap?.outputChannels ?? 0) >= 2;
  const isBalanced = !isDopNative && hasPair && (snap?.balance ?? 0) !== 0;
  const isWidened =
    !isDopNative &&
    hasPair &&
    !isMono &&
    track.channels !== 1 &&
    (snap?.stereoWidth ?? 100) !== 100;
  // The volume is a gain like any other: below 100 % every sample is
  // scaled before it reaches the device, exclusive mode included (a
  // device we own has no mixer to do it for us). DoP carries DSD and
  // is never scaled.
  const isAttenuated = !isDopNative && volume < 100;
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
    !isTransposed &&
    !isEq &&
    !isNormalize &&
    !isReplayGain &&
    !isMono &&
    !isBalanced &&
    !isWidened &&
    !isAttenuated;
  // …but bit-perfect also means nothing *downstream* touches them, and
  // that only holds when the stream owns the device. A shared-mode
  // stream at the same nominal rate still goes through the system
  // mixer, which re-clocks it and mixes in whatever else is playing —
  // the pill used to claim bit-perfect for exactly that case. DoP
  // implies an exclusive backend, so it qualifies on its own.
  const isBitPerfect =
    isUnprocessed && (isDopNative || (snap?.exclusiveActive ?? false));

  const sourceRate = formatKHz(track.sample_rate, lang);
  const outputRate = snap ? formatKHz(snap.outputSampleRate, lang) : null;
  const channelLabel = (n: number | null | undefined): string | null => {
    if (n == null || n <= 0) return null;
    if (n === 1) return t("playerBar.pipeline.channels.mono");
    if (n === 2) return t("playerBar.pipeline.channels.stereo");
    return (
      surroundLayout(n) ?? t("playerBar.pipeline.channels.other", { value: n })
    );
  };

  // ── The file ────────────────────────────────────────────────────
  const kind = fileKind(track.codec, track.bit_depth);
  // A lossless file the analysis caught stopping at a lossy encoder's
  // wall is described as what it probably is, and earns no badge.
  const fileText =
    kind === "lossless" && probableFakeLossless === true
      ? "fakeLossless"
      : kind;
  // The badge and the word say the same thing; the card shows the word
  // only where no badge is drawn, as the strip does.
  const { visible: badgesVisible } = useHiResBadgeVisibility();
  const showsBadge =
    badgesVisible &&
    qualityBadge(
      track.bit_depth,
      track.sample_rate,
      track.codec,
      probableFakeLossless,
    ) != null;
  const fileFigures = [
    track.codec,
    track.bit_depth
      ? t("playerBar.pipeline.bits", { value: track.bit_depth })
      : null,
    sourceRate,
    formatBitrate(track.bitrate, lang),
    channelLabel(track.channels),
  ]
    .filter(Boolean)
    .join(" · ");

  // ── What the engine does, one card per stage ───────────────────
  const stages: StageCard[] = [];
  if (isDopNative)
    stages.push({
      key: "dop",
      icon: Disc3,
      title: t("playerBar.pipeline.stage.dop.title"),
      body: t("playerBar.pipeline.stage.dop.body"),
      impact: "none",
    });
  else if (isDsd)
    stages.push({
      key: "dsd",
      icon: Disc3,
      title: t("playerBar.pipeline.stage.dsdToPcm.title"),
      body: t("playerBar.pipeline.stage.dsdToPcm.body"),
      impact: "inaudible",
    });
  if (isResampling && sourceRate && outputRate)
    stages.push({
      key: "resample",
      icon: AudioWaveform,
      title: t("playerBar.pipeline.stage.resample.title"),
      body: t("playerBar.pipeline.stage.resample.body", {
        source: sourceRate,
        output: outputRate,
      }),
      figures: `${sourceRate} → ${outputRate}`,
      impact: "inaudible",
    });
  if (isDownmixing) {
    const from = channelLabel(track.channels);
    const to = channelLabel(snap?.outputChannels);
    stages.push({
      key: "downmix",
      icon: Merge,
      title: t("playerBar.pipeline.stage.downmix.title"),
      body: t("playerBar.pipeline.stage.downmix.body", {
        source: from,
        output: to,
      }),
      figures: `${from} → ${to}`,
      impact: "changes",
    });
  }
  if (isEq)
    stages.push({
      key: "eq",
      icon: SlidersHorizontal,
      title: t("playerBar.pipeline.stage.eq.title"),
      body: t("playerBar.pipeline.stage.eq.body"),
      impact: "changes",
    });
  if (isReplayGain && snap)
    stages.push({
      key: "rg",
      icon: Volume2,
      title: t("playerBar.pipeline.stage.replayGain.title"),
      body: t("playerBar.pipeline.stage.replayGain.body"),
      figures: t(`settings.replayGain.mode.${snap.replaygainMode}`),
      impact: "level",
    });
  if (isNormalize)
    stages.push({
      key: "norm",
      icon: Volume2,
      title: t("playerBar.pipeline.stage.normalize.title"),
      body: t("playerBar.pipeline.stage.normalize.body"),
      impact: "level",
    });
  if (isMono)
    stages.push({
      key: "mono",
      icon: Merge,
      title: t("playerBar.pipeline.stage.mono.title"),
      body: t("playerBar.pipeline.stage.mono.body"),
      impact: "changes",
    });
  if (isBalanced && snap)
    stages.push({
      key: "balance",
      icon: Scale,
      title: t("playerBar.pipeline.stage.balance.title"),
      body: t(
        snap.balance < 0
          ? "playerBar.pipeline.stage.balance.bodyLeft"
          : "playerBar.pipeline.stage.balance.bodyRight",
      ),
      figures: `${Math.abs(snap.balance)} %`,
      impact: "changes",
    });
  if (isWidened && snap)
    stages.push({
      key: "width",
      icon: Speaker,
      title: t("playerBar.pipeline.stage.width.title"),
      body: t(
        snap.stereoWidth < 100
          ? "playerBar.pipeline.stage.width.bodyNarrower"
          : "playerBar.pipeline.stage.width.bodyWider",
      ),
      figures: `${snap.stereoWidth} %`,
      impact: "changes",
    });
  if (isAttenuated)
    stages.push({
      key: "volume",
      icon: Volume1,
      title: t("playerBar.pipeline.stage.volume.title"),
      body: t("playerBar.pipeline.stage.volume.body", {
        value: Math.round(volume),
      }),
      figures: `${Math.round(volume)} %`,
      impact: "level",
    });
  if (isSpeedShifted) {
    const speed = playbackSpeed.toFixed(2).replace(/\.?0+$/, "");
    stages.push({
      key: "speed",
      icon: Gauge,
      title: t("playerBar.pipeline.stage.speed.title"),
      body: t("playerBar.pipeline.stage.speed.body", { value: speed }),
      figures: `${speed}×`,
      impact: "changes",
    });
  }
  if (isTransposed)
    stages.push({
      key: "pitch",
      icon: Music2,
      title: t("playerBar.pipeline.stage.pitch.title"),
      body: t(
        pitchSemitones > 0
          ? "playerBar.pipeline.stage.pitch.bodyUp"
          : "playerBar.pipeline.stage.pitch.bodyDown",
      ),
      figures: t("playerBar.pipeline.stage.pitch.figures", {
        value: `${pitchSemitones > 0 ? "+" : ""}${pitchSemitones}`,
      }),
      impact: "changes",
    });

  // ── The output ─────────────────────────────────────────────────
  // The device the sound really comes out of (`is_active`, #612), not
  // the one picked: they differ while a pinned device is missing.
  const activeDevice = outputDevices.find((d) => d.is_active) ?? null;
  const outputBody = snap
    ? t(
        snap.outputMode === "dop"
          ? "playerBar.pipeline.output.dop"
          : snap.outputMode === "exclusive"
            ? "playerBar.pipeline.output.exclusive"
            : snap.outputMode === "exclusive-refused"
              ? "playerBar.pipeline.output.exclusiveRefused"
              : "playerBar.pipeline.output.shared",
      )
    : t("playerBar.pipeline.loading");
  const outputFigures = snap
    ? [outputRate, channelLabel(snap.outputChannels)]
        .filter(Boolean)
        .join(" · ")
    : null;

  // ── Placement ──────────────────────────────────────────────────
  // Above the strip, flush with its end edge, measured once it is laid
  // out and again whenever the window is resized under it.
  const [place, setPlace] = useState<{
    right: number;
    bottom: number;
    maxHeight: number;
  } | null>(null);
  useLayoutEffect(() => {
    if (!anchor) return;
    const measure = () => {
      const rect = anchor.getBoundingClientRect();
      setPlace({
        right: Math.max(
          POPOVER_EDGE_PX,
          window.innerWidth - rect.right + POPOVER_EDGE_PX,
        ),
        bottom: window.innerHeight - rect.top + POPOVER_GAP_PX,
        maxHeight: Math.max(160, rect.top - POPOVER_GAP_PX - POPOVER_EDGE_PX),
      });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [anchor]);

  if (!place) return null;

  return createPortal(
    // Hover-triggered informational popover — not a dialog. Skipping
    // `role="dialog"` (and the associated `useModalA11y` focus trap)
    // by design: the popover holds no actionable controls, dismisses
    // automatically on hover-leave, and trapping focus inside it
    // would feel broken when the user only meant to glance at the
    // specs. `role="group"` keeps the heading and the cards grouped
    // for assistive tech without claiming dialog semantics.
    <div
      role="group"
      aria-label={t("playerBar.pipeline.title")}
      data-audio-pipeline
      onMouseEnter={onPointerEnter}
      onMouseLeave={onPointerLeave}
      style={{
        right: place.right,
        bottom: place.bottom,
        maxHeight: place.maxHeight,
      }}
      className="fixed z-[100] w-[22rem] max-w-[calc(100vw-2rem)] overflow-y-auto overscroll-contain rounded-2xl border border-zinc-200 bg-white p-4 text-left shadow-xl animate-fade-in dark:border-zinc-800 dark:bg-zinc-900"
    >
      <div className="mb-3 text-xs font-bold uppercase tracking-widest text-zinc-400">
        {t("playerBar.pipeline.title")}
      </div>

      <ol className="flex flex-col gap-2">
        <AudioPipelineCard
          icon={FileAudio}
          title={t("playerBar.pipeline.file.title")}
          badge={
            <HiResBadge
              bitDepth={track.bit_depth}
              sampleRate={track.sample_rate}
              codec={track.codec}
              probableFakeLossless={probableFakeLossless}
              variant="inline"
            />
          }
          subtitle={
            fileText && !showsBadge
              ? t(`playerBar.pipeline.kind.${fileText}`)
              : null
          }
          body={fileText ? t(`playerBar.pipeline.file.${fileText}`) : null}
          figures={fileFigures || null}
        />

        {stages.length === 0
          ? // "Untouched" is only said once the engine has answered;
            // before that the stages are not known yet.
            snap && (
              <AudioPipelineCard
                icon={Sparkles}
                title={t("playerBar.pipeline.stage.none.title")}
                body={t("playerBar.pipeline.stage.none.body")}
                impact="none"
              />
            )
          : stages.map((stage) => (
              <AudioPipelineCard
                key={stage.key}
                icon={stage.icon}
                title={stage.title}
                body={stage.body}
                figures={stage.figures}
                impact={stage.impact}
              />
            ))}

        <AudioPipelineCard
          icon={Speaker}
          title={t("playerBar.pipeline.output.title")}
          subtitle={activeDevice?.name ?? null}
          body={outputBody}
          figures={outputFigures || null}
        />
      </ol>

      {/* The verdict — the last thing read, so it has to be the one we
          can actually stand behind: bit-perfect when the stream owns
          the device, "untouched by WaveFlow" when our pipeline is
          transparent but the system mixer still sits in the path. */}
      {isBitPerfect && (
        <div
          data-pipeline-verdict="bit-perfect"
          className="mt-3 flex items-start gap-2 border-t border-zinc-100 pt-3 dark:border-zinc-800"
        >
          <Sparkles
            size={14}
            className="mt-0.5 shrink-0 text-emerald-500"
            aria-hidden="true"
          />
          <span className="text-xs text-zinc-600 dark:text-zinc-300">
            <span className="font-semibold text-emerald-600 dark:text-emerald-400">
              {t("playerBar.pipeline.bitPerfect")}
            </span>{" "}
            {t("playerBar.pipeline.verdict.bitPerfect")}
          </span>
        </div>
      )}
      {!isBitPerfect && isUnprocessed && (
        <div
          data-pipeline-verdict="shared"
          className="mt-3 flex items-start gap-2 border-t border-zinc-100 pt-3 dark:border-zinc-800"
        >
          <Speaker
            size={14}
            className="mt-0.5 shrink-0 text-zinc-400"
            aria-hidden="true"
          />
          <span className="text-xs text-zinc-600 dark:text-zinc-300">
            {t("playerBar.pipeline.verdict.shared")}
          </span>
        </div>
      )}
    </div>,
    document.body,
  );
}
