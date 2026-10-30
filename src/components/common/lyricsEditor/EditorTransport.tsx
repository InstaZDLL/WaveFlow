import { useTranslation } from "react-i18next";
import { Pause, Play, RotateCcw, RotateCw } from "lucide-react";
import { usePlayerPosition } from "../../../hooks/usePlayerPosition";
import { formatEditorTime } from "./shared";

/** The speeds offered while syncing: slower helps place fast words. */
const SPEEDS = [0.5, 0.75, 1] as const;
const SKIP_MS = 2000;

interface EditorTransportProps {
  isPlaying: boolean;
  togglePlayback: () => void;
  seek: (ms: number) => void;
  estimate: () => number;
  speed: number;
  setSpeed: (speed: number) => void;
  durationMs: number | null;
}

/**
 * Play, pause, skip and slow down the song being synced — the app's own
 * player, so the stamps are taken against the position the engine
 * reports and the speed is the engine's: a resampler shift, so a slower
 * song also sounds lower, and the position still counts song time.
 */
export function EditorTransport({
  isPlaying,
  togglePlayback,
  seek,
  estimate,
  speed,
  setSpeed,
  durationMs,
}: EditorTransportProps) {
  const { t } = useTranslation();
  const positionMs = usePlayerPosition();
  return (
    <div className="flex flex-wrap items-center gap-2">
      <button
        type="button"
        onClick={() => seek(Math.max(0, estimate() - SKIP_MS))}
        title={t("lyricsEditor.transport.back")}
        aria-label={t("lyricsEditor.transport.back")}
        className="p-2 rounded-full hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
      >
        <RotateCcw size={16} />
      </button>
      <button
        type="button"
        onClick={togglePlayback}
        title={
          isPlaying ? t("player.controls.pause") : t("player.controls.play")
        }
        aria-label={
          isPlaying ? t("player.controls.pause") : t("player.controls.play")
        }
        className="p-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 hover:opacity-90 transition-opacity"
      >
        {isPlaying ? <Pause size={16} /> : <Play size={16} />}
      </button>
      <button
        type="button"
        onClick={() => seek(estimate() + SKIP_MS)}
        title={t("lyricsEditor.transport.forward")}
        aria-label={t("lyricsEditor.transport.forward")}
        className="p-2 rounded-full hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
      >
        <RotateCw size={16} />
      </button>
      <span className="ml-1 font-mono text-xs text-zinc-500 dark:text-zinc-400 tabular-nums">
        {formatEditorTime(positionMs)}
        {durationMs ? ` / ${formatEditorTime(durationMs)}` : ""}
      </span>
      <div
        role="radiogroup"
        aria-label={t("lyricsEditor.transport.speed")}
        title={t("lyricsEditor.transport.speed")}
        className="ml-auto flex rounded-full bg-zinc-100 dark:bg-zinc-800 p-0.5 text-xs"
      >
        {SPEEDS.map((value) => {
          const checked = Math.abs(speed - value) < 0.001;
          return (
            <button
              key={value}
              type="button"
              role="radio"
              aria-checked={checked}
              onClick={() => setSpeed(value)}
              className={`px-2.5 py-1 rounded-full font-medium tabular-nums transition-colors ${
                checked
                  ? "bg-white dark:bg-zinc-700 text-zinc-900 dark:text-white shadow-sm"
                  : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200"
              }`}
            >
              {value}×
            </button>
          );
        })}
      </div>
    </div>
  );
}
