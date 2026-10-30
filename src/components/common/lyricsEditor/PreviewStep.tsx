import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { ImmersiveLyricsColumn } from "../../player/ImmersiveLyricsColumn";
import { useLyricsTimeline } from "../../../hooks/useLyricsTimeline";
import type { LyricsFormat, LyricsPayload } from "../../../lib/tauri/lyrics";
import type { Track } from "../../../lib/tauri/track";

interface PreviewStepProps {
  track: Track;
  /** The document as it would be saved, or `null` while it is written. */
  written: { content: string; format: LyricsFormat } | null;
  /** Lines left out because they have no time yet. */
  hiddenLines: number;
  seek: (ms: number) => void;
}

const noop = () => {};

/**
 * The Preview step: the document as it would be saved, read back by the
 * app's own lyrics parser and drawn by the immersive view's karaoke
 * column, playing along with the song. What shows here is what the
 * panel and the immersive view will show once it is saved.
 */
export function PreviewStep({
  track,
  written,
  hiddenLines,
  seek,
}: PreviewStepProps) {
  const { t } = useTranslation();
  const payload = useMemo<LyricsPayload | null>(
    () =>
      written
        ? {
            track_id: track.id,
            content: written.content,
            format: written.format,
            source: "manual",
          }
        : null,
    [track.id, written],
  );
  const timeline = useLyricsTimeline(payload, false);

  return (
    <div className="flex h-full min-h-0 flex-col gap-2">
      {hiddenLines > 0 && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("lyricsEditor.preview.hiddenLines", { n: hiddenLines })}
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-hidden rounded-2xl bg-zinc-950">
        {payload && timeline.isSynced ? (
          <ImmersiveLyricsColumn
            track={track}
            payload={timeline.shownPayload}
            lrcLines={timeline.lrcLines}
            isSynced={timeline.isSynced}
            activeIndex={timeline.activeIndex}
            activeWordIndex={timeline.activeWordIndex}
            activeBackgroundWordIndex={timeline.activeBackgroundWordIndex}
            overlapIndex={timeline.overlapIndex}
            overlapWordIndex={timeline.overlapWordIndex}
            overlapBackgroundWordIndex={timeline.overlapBackgroundWordIndex}
            activeInterlude={timeline.activeInterlude}
            isFetching={false}
            error={null}
            staticText={null}
            isRadio={false}
            onSeek={(line) => seek(line.timeMs)}
            onImport={noop}
            onRefetch={noop}
            showHeader={false}
          />
        ) : (
          <div className="flex h-full items-center justify-center p-8 text-center text-sm text-white/60">
            {written === null
              ? t("lyricsEditor.preview.writing")
              : t("lyricsEditor.preview.nothingSynced")}
          </div>
        )}
      </div>
    </div>
  );
}
