import { useTranslation } from "react-i18next";
import { ChevronsLeft, ChevronsRight, FileText } from "lucide-react";

import { useEpisodeChapters } from "../../hooks/useEpisodeChapters";
import { usePlayer } from "../../hooks/usePlayer";
import {
  getPlayerPosition,
  usePlayerPositionValue,
} from "../../hooks/usePlayerPosition";
import {
  indexAtPosition,
  nextChapterStart,
  previousChapterStart,
} from "../../lib/podcastTimeline";

/**
 * The player's "⋯" menu during an episode: the chapters around the position, and
 * the transcript. Mounted only while the menu is open, so it asks for nothing and
 * re-renders for nobody otherwise.
 */
export function PodcastPlayerMenu({
  episodeId,
  onOpenTranscript,
}: {
  episodeId: number;
  onOpenTranscript: () => void;
}) {
  const { t } = useTranslation();
  const { seek } = usePlayer();
  const { chapters } = useEpisodeChapters(episodeId);
  const current = usePlayerPositionValue((ms) =>
    indexAtPosition(chapters.length, (i) => chapters[i].start_ms, ms),
  );
  const canGoNext = usePlayerPositionValue(
    (ms) => nextChapterStart(chapters, ms) != null,
  );
  const canGoBack = usePlayerPositionValue(
    (ms) => previousChapterStart(chapters, ms) != null,
  );
  const go = (target: number | null) => {
    if (target == null) return;
    seek(target).catch((err) =>
      console.error("[PodcastPlayerMenu] jump failed", err),
    );
  };

  return (
    <>
      {chapters.length > 0 && (
        <div className="p-1" data-podcast-chapter-menu>
          {current >= 0 && (
            <div className="px-2 pb-1 pt-0.5">
              <div className="text-xs text-zinc-400">
                {t("player.podcast.chapterOf", {
                  current: current + 1,
                  total: chapters.length,
                })}
              </div>
              <div
                className="truncate text-sm text-zinc-700 dark:text-zinc-200"
                dir="auto"
                title={chapters[current].title}
              >
                {chapters[current].title}
              </div>
            </div>
          )}
          <div className="grid grid-cols-2 gap-1">
            <button
              type="button"
              disabled={!canGoBack}
              onClick={() =>
                go(previousChapterStart(chapters, getPlayerPosition()))
              }
              className="flex items-center justify-center gap-1.5 rounded-lg px-2 py-2 text-sm text-zinc-700 transition-colors hover:bg-zinc-100 disabled:pointer-events-none disabled:opacity-40 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              <ChevronsLeft
                size={14}
                className="shrink-0 rtl:rotate-180"
                aria-hidden="true"
              />
              <span className="truncate">
                {t("player.podcast.previousChapter")}
              </span>
            </button>
            <button
              type="button"
              disabled={!canGoNext}
              onClick={() =>
                go(nextChapterStart(chapters, getPlayerPosition()))
              }
              className="flex items-center justify-center gap-1.5 rounded-lg px-2 py-2 text-sm text-zinc-700 transition-colors hover:bg-zinc-100 disabled:pointer-events-none disabled:opacity-40 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              <span className="truncate">
                {t("player.podcast.nextChapter")}
              </span>
              <ChevronsRight
                size={14}
                className="shrink-0 rtl:rotate-180"
                aria-hidden="true"
              />
            </button>
          </div>
        </div>
      )}
      <div className="p-1">
        <button
          type="button"
          onClick={onOpenTranscript}
          className="flex w-full items-center gap-2 rounded-lg px-2 py-2 text-left text-sm text-zinc-700 transition-colors hover:bg-zinc-100 dark:text-zinc-200 dark:hover:bg-zinc-800"
        >
          <FileText size={14} className="text-zinc-400" aria-hidden="true" />
          {t("podcasts.transcript.title")}
        </button>
      </div>
      <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-800" />
    </>
  );
}
