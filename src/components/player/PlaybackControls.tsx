import { useTranslation } from "react-i18next";
import {
  Shuffle,
  SkipBack,
  Play,
  Pause,
  SkipForward,
  Repeat,
  Repeat1,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";

export function PlaybackControls() {
  const { t } = useTranslation();
  const {
    isPlaying,
    togglePlayback,
    isShuffled,
    toggleShuffle,
    repeatMode,
    cycleRepeatMode,
  } = usePlayer();

  const RepeatIcon = repeatMode === "one" ? Repeat1 : Repeat;
  const isRepeatActive = repeatMode !== "off";

  return (
    <div className="flex items-center space-x-6 mb-3">
      <button
        type="button"
        onClick={toggleShuffle}
        aria-pressed={isShuffled}
        aria-label={
          isShuffled
            ? t("player.controls.shuffleOn")
            : t("player.controls.shuffleOff")
        }
        className={`transition-colors ${
          isShuffled
            ? "text-emerald-500 hover:text-emerald-400"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <Shuffle size={18} />
      </button>
      <button
        type="button"
        aria-label={t("player.controls.previous")}
        className="text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
      >
        <SkipBack size={20} />
      </button>

      <button
        type="button"
        onClick={togglePlayback}
        aria-label={isPlaying ? t("player.controls.pause") : t("player.controls.play")}
        className="w-10 h-10 rounded-full bg-emerald-500 hover:bg-emerald-400 text-white flex items-center justify-center shadow-md transition-transform active:scale-95"
      >
        {isPlaying ? (
          <Pause size={20} className="fill-current" />
        ) : (
          <Play size={20} className="fill-current translate-x-px" />
        )}
      </button>

      <button
        type="button"
        aria-label={t("player.controls.next")}
        className="text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
      >
        <SkipForward size={20} />
      </button>
      <button
        type="button"
        onClick={cycleRepeatMode}
        aria-label={
          repeatMode === "off"
            ? t("player.controls.repeatOff")
            : repeatMode === "all"
              ? t("player.controls.repeatAll")
              : t("player.controls.repeatOne")
        }
        className={`transition-colors ${
          isRepeatActive
            ? "text-emerald-500 hover:text-emerald-400"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <RepeatIcon size={18} />
      </button>
    </div>
  );
}
