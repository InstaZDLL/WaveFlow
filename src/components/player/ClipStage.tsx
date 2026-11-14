import { useTranslation } from "react-i18next";
import { Minimize2, Music, VolumeX } from "lucide-react";

import { usePlayer } from "../../hooks/usePlayer";
import { requestBackToAudio, useOpenClip } from "../../lib/clipPlayer";
import { ClipVideo } from "./ClipVideo";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";

/**
 * A music video enlarged (#1073): over the whole immersive view, theater
 * mode included, with WaveFlow's own progress and transport under it —
 * the same controls as the player bar, which are the clip's while it is
 * shown. Shrinking it closes the immersive view, and the clip goes on in
 * the now-playing panel (`ClipHost` decides where it shows).
 */
export function ClipStage() {
  const { t } = useTranslation();
  const clip = useOpenClip();
  const { closeImmersive, outputMode } = usePlayer();
  if (!clip) return null;

  const exclusive = outputMode === "exclusive" || outputMode === "dop";

  return (
    <div
      aria-label={t("clip.label", { title: clip.title })}
      role="region"
      className="absolute inset-0 z-20 flex flex-col bg-black text-white"
    >
      <ClipVideo className="min-h-0 w-full flex-1" />

      <div
        data-theater-chrome
        // A scrim: the title is white over colours nobody can know ahead.
        className="absolute top-0 inset-x-0 flex items-start justify-between gap-4 bg-linear-to-b from-black/70 to-transparent px-8 pt-6 pb-12 pointer-events-none *:pointer-events-auto"
      >
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => requestBackToAudio("user")}
            className="flex items-center gap-2 rounded-full bg-black/50 px-4 py-2.5 text-sm font-medium backdrop-blur-sm transition-colors hover:bg-black/70"
          >
            <Music size={18} />
            {t("clip.switchToAudio")}
          </button>
          <button
            type="button"
            onClick={closeImmersive}
            aria-label={t("clip.shrink")}
            title={t("clip.shrink")}
            className="rounded-full bg-black/50 p-2.5 backdrop-blur-sm transition-colors hover:bg-black/70"
          >
            <Minimize2 size={18} />
          </button>
        </div>
        <div className="min-w-0 text-end">
          <p className="truncate text-sm font-semibold">{clip.title}</p>
          {clip.artist && (
            <p className="truncate text-xs text-white/70">{clip.artist}</p>
          )}
        </div>
      </div>

      <div
        data-theater-chrome
        className="flex flex-col gap-2 bg-linear-to-t from-black/80 to-transparent px-8 pt-6 pb-6"
      >
        {exclusive && (
          <div
            role="status"
            className="mx-auto flex w-fit items-center gap-2 rounded-full bg-black/70 px-4 py-2 text-xs text-white/90"
          >
            <VolumeX size={14} />
            {t("clip.exclusiveNoSound")}
          </div>
        )}
        <ProgressBar />
        <div className="flex justify-center">
          <PlaybackControls />
        </div>
      </div>
    </div>
  );
}
