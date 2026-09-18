import { useTranslation } from "react-i18next";
import {
  Shuffle,
  Disc3,
  SkipBack,
  Play,
  Pause,
  SkipForward,
  Repeat,
  Repeat1,
  Loader2,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { isRadioTrack, isRemoteTrack } from "../../lib/playerSources";

export function PlaybackControls() {
  const { t } = useTranslation();
  const {
    isPlaying,
    playbackState,
    togglePlayback,
    isShuffled,
    shuffleMode,
    cycleShuffleMode,
    repeatMode,
    cycleRepeatMode,
    next,
    previous,
    currentTrack,
  } = usePlayer();

  const isLoading = playbackState === "loading";
  const disableTransport = !currentTrack && playbackState === "idle";
  // Web Radio is a single-stream source — there's no queue cursor to
  // advance, so Previous / Next / Shuffle / Repeat would either be a
  // no-op (the queue cursor still points at the last local track) or
  // worse, kick off the next queued local track in the background.
  // Disable the queue-bound transports while a live stream is loaded.
  // Discrimination contract lives in `isRadioTrack` — keep the
  // gating decentralised here, the invariant centralised there.
  const isRadio = isRadioTrack(currentTrack);
  // A remote-queue track advances (Previous / Next / Repeat drive the
  // remote queue in the backend), so it is NOT gated like radio — only
  // Shuffle stays off, since the remote queue has no shuffle yet.
  const isRemote = isRemoteTrack(currentTrack);
  // State labels, like the repeat control next to it — with three
  // positions, "enable / disable" no longer says which one you are in.
  const shuffleLabel = t(
    shuffleMode === "albums"
      ? "player.controls.shuffleAlbums"
      : shuffleMode === "tracks"
        ? "player.controls.shuffleTracks"
        : "player.controls.shuffleOff",
  );
  const RepeatIcon = repeatMode === "one" ? Repeat1 : Repeat;
  const isRepeatActive = repeatMode !== "off";

  return (
    <div className="flex items-center space-x-5 mb-1.5">
      <button
        type="button"
        onClick={cycleShuffleMode}
        disabled={isRadio || isRemote}
        aria-pressed={isShuffled}
        aria-label={shuffleLabel}
        title={shuffleLabel}
        className={`relative transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
          isShuffled
            ? "text-emerald-500 hover:text-emerald-400"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <Shuffle size={20} />
        {/* Album grouping is still shuffle, so it keeps the shuffle
            icon and earns a mark rather than a different one — the
            same way Repeat One stays a repeat. */}
        {shuffleMode === "albums" && (
          <Disc3
            size={11}
            className="absolute -top-1 -right-1.5"
            aria-hidden="true"
          />
        )}
      </button>
      <button
        type="button"
        onClick={() => previous()}
        disabled={disableTransport || isRadio}
        aria-label={t("player.controls.previous")}
        className="text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
      >
        <SkipBack size={20} />
      </button>

      <button
        type="button"
        onClick={() => togglePlayback()}
        disabled={disableTransport}
        aria-label={
          isPlaying ? t("player.controls.pause") : t("player.controls.play")
        }
        aria-busy={isLoading}
        className="w-10 h-10 rounded-full bg-emerald-500 hover:bg-emerald-400 text-white flex items-center justify-center shadow-md transition-transform active:scale-95 disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {isLoading ? (
          <Loader2 size={20} className="animate-spin" />
        ) : isPlaying ? (
          <Pause size={20} className="fill-current" />
        ) : (
          <Play size={20} className="fill-current translate-x-px" />
        )}
      </button>

      <button
        type="button"
        onClick={() => next()}
        disabled={disableTransport || isRadio}
        aria-label={t("player.controls.next")}
        className="text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
      >
        <SkipForward size={20} />
      </button>
      <button
        type="button"
        onClick={cycleRepeatMode}
        disabled={isRadio}
        aria-label={
          repeatMode === "off"
            ? t("player.controls.repeatOff")
            : repeatMode === "all"
              ? t("player.controls.repeatAll")
              : t("player.controls.repeatOne")
        }
        className={`transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
          isRepeatActive
            ? "text-emerald-500 hover:text-emerald-400"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <RepeatIcon size={20} />
      </button>
    </div>
  );
}
