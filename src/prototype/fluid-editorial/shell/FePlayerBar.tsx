import {
  Ellipsis,
  Heart,
  ListMusic,
  Maximize2,
  Mic2,
  MonitorSpeaker,
  Pause,
  PictureInPicture2,
  Play,
  Repeat,
  Shuffle,
  SkipBack,
  SkipForward,
  Volume2,
} from "lucide-react";
import { qualityTag } from "../format";
import { useFe } from "../useFe";
import { Cover } from "../ui/Cover";
import { WaveProgress } from "./WaveProgress";

/**
 * Compact (84 px), no border: a faint top gradient in the artwork's
 * colour says "this surface belongs to what is playing". Three groups,
 * three weights: identity on the left (cover 56 px, title 15 px / artist
 * 12 px / a mono quality tag), transport in the middle around one filled
 * accent button, and the panels on the right in two quiet clusters.
 *
 * Every control of the production bar is still here — lyrics, queue,
 * devices, volume, mini-player, immersive, the "⋯" overflow — nothing
 * is removed, only regrouped and lowered in contrast.
 */
export function FePlayerBar() {
  const {
    copy,
    current,
    isPlaying,
    togglePlay,
    next,
    previous,
    liked,
    toggleLike,
    radioMood,
  } = useFe();
  const tag = qualityTag(current.codec, current.sampleRate, current.bitDepth);
  const moodLabel = radioMood ? copy.mood.moods[radioMood].title : null;

  return (
    <footer
      className="relative z-50 shrink-0"
      style={{
        backgroundColor: "var(--fe-surface)",
        backgroundImage:
          "linear-gradient(180deg, color-mix(in srgb, var(--fe-glow) 28%, transparent), transparent 55%)",
      }}
    >
      <div className="fe-hairline-t grid h-[5.25rem] grid-cols-[minmax(0,1fr)_minmax(28rem,38rem)_minmax(0,1fr)] items-center gap-6 px-5">
        {/* Identity */}
        <div className="flex min-w-0 items-center gap-3.5">
          <button
            type="button"
            className="fe-press shrink-0"
            aria-label={copy.player.immersive}
          >
            <Cover
              src={current.cover}
              palette={current.palette}
              alt={current.album ?? current.title}
              decorative
              className="h-14 w-14 rounded-[0.625rem]!"
            />
          </button>
          <div className="min-w-0">
            <div className="truncate text-[0.9375rem] font-[540] leading-tight tracking-[-0.01em] text-[var(--fe-ink)]">
              {current.title}
            </div>
            <div className="fe-ink-2 mt-0.5 truncate text-[0.75rem]">
              {moodLabel ? `${copy.canvas.radio} · ${moodLabel} · ` : ""}
              {current.artist}
            </div>
            {tag && (
              <div className="fe-ink-3 fe-mono mt-0.5 text-[0.625rem] tracking-[0.04em]">
                {tag}
                {current.bitDepth === 24 && (
                  <span className="fe-accent ml-2 font-medium">
                    {copy.player.bitPerfect}
                  </span>
                )}
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={toggleLike}
            className="fe-icon-btn ml-1 shrink-0"
            aria-label={copy.player.like}
            aria-pressed={liked}
          >
            <Heart
              size={17}
              strokeWidth={1.6}
              className={liked ? "fill-current" : ""}
            />
          </button>
        </div>

        {/* Transport */}
        <div className="flex flex-col items-center">
          <div className="mb-1.5 flex items-center gap-4">
            <button
              type="button"
              className="fe-icon-btn h-8 w-8"
              aria-label={copy.player.shuffle}
            >
              <Shuffle size={16} strokeWidth={1.6} />
            </button>
            <button
              type="button"
              className="fe-icon-btn h-9 w-9"
              aria-label={copy.player.previous}
              onClick={previous}
            >
              <SkipBack size={18} strokeWidth={1.6} className="fill-current" />
            </button>
            <button
              type="button"
              onClick={togglePlay}
              aria-label={isPlaying ? copy.player.pause : copy.player.play}
              className="fe-press grid h-11 w-11 place-items-center rounded-full"
              style={{
                backgroundColor: "var(--fe-accent)",
                color: "var(--fe-accent-ink)",
                boxShadow:
                  "0 12px 28px -12px color-mix(in srgb, var(--fe-accent) 80%, transparent)",
              }}
            >
              {isPlaying ? (
                <Pause size={18} className="fill-current" strokeWidth={0} />
              ) : (
                <Play
                  size={18}
                  className="translate-x-px fill-current"
                  strokeWidth={0}
                />
              )}
            </button>
            <button
              type="button"
              className="fe-icon-btn h-9 w-9"
              aria-label={copy.player.next}
              onClick={next}
            >
              <SkipForward
                size={18}
                strokeWidth={1.6}
                className="fill-current"
              />
            </button>
            <button
              type="button"
              className="fe-icon-btn h-8 w-8"
              aria-label={copy.player.repeat}
            >
              <Repeat size={16} strokeWidth={1.6} />
            </button>
          </div>
          <WaveProgress />
        </div>

        {/* Panels + output */}
        <div className="flex items-center justify-end gap-0.5">
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.lyrics}
          >
            <Mic2 size={17} strokeWidth={1.6} />
          </button>
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.queue}
          >
            <ListMusic size={17} strokeWidth={1.6} />
          </button>
          <span className="mx-2 h-5 w-px bg-white/[0.08]" aria-hidden="true" />
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.devices}
          >
            <MonitorSpeaker size={17} strokeWidth={1.6} />
          </button>
          <div className="group flex items-center">
            <button
              type="button"
              className="fe-icon-btn"
              aria-label={copy.player.volume}
            >
              <Volume2 size={17} strokeWidth={1.6} />
            </button>
            <div
              className="relative h-1 w-20 rounded-full bg-white/[0.1]"
              role="slider"
              aria-label={copy.player.volume}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={72}
              tabIndex={0}
            >
              <div
                className="h-full w-[72%] rounded-full"
                style={{ backgroundColor: "var(--fe-ink-2)" }}
              />
            </div>
          </div>
          <span className="mx-2 h-5 w-px bg-white/[0.08]" aria-hidden="true" />
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.miniPlayer}
          >
            <PictureInPicture2 size={17} strokeWidth={1.6} />
          </button>
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.immersive}
          >
            <Maximize2 size={17} strokeWidth={1.6} />
          </button>
          <button
            type="button"
            className="fe-icon-btn"
            aria-label={copy.player.more}
          >
            <Ellipsis size={17} strokeWidth={1.6} />
          </button>
        </div>
      </div>
    </footer>
  );
}
