import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Loader2 } from "lucide-react";

import { useEpisodeChapters } from "../../hooks/useEpisodeChapters";
import { useEpisodeJump } from "../../hooks/useEpisodeJump";
import { usePageVirtualizer } from "../../hooks/usePageVirtualizer";
import { usePlayerPositionValue } from "../../hooks/usePlayerPosition";
import { podcastFailureMessage } from "../../lib/podcastWords";
import { indexAtPosition } from "../../lib/podcastTimeline";
import { podcastErrorWord, type PodcastChapter } from "../../lib/tauri/podcast";
import { formatDuration } from "../../lib/tauri/track";

const ROW_HEIGHT = 40;

/** The chapter under the position, for the episode the player holds, and `-1` for
 *  any other. A re-render only when it changes, not four times a second. */
function useCurrentChapter(chapters: PodcastChapter[], playing: boolean) {
  return usePlayerPositionValue((ms) =>
    playing
      ? indexAtPosition(chapters.length, (i) => chapters[i].start_ms, ms)
      : -1,
  );
}

function ChapterRow({
  chapter,
  current,
  onJump,
}: {
  chapter: PodcastChapter;
  current: boolean;
  onJump: (chapter: PodcastChapter) => void;
}) {
  const { t } = useTranslation();
  const time = formatDuration(chapter.start_ms);
  return (
    <button
      type="button"
      className="pc-chapter"
      data-current={current || undefined}
      aria-current={current || undefined}
      aria-label={t("podcasts.chapters.jump", { title: chapter.title, time })}
      onClick={() => onJump(chapter)}
    >
      <span className="pc-chapter__time">{time}</span>
      {/* Text from a feed or a tag, drawn as text. */}
      <span className="pc-chapter__title" dir="auto">
        {chapter.title}
      </span>
    </button>
  );
}

/** What stands in for a list: looking, why there is none, or what went wrong. */
function ChaptersState({
  loading,
  failure,
  empty,
  onRetry,
}: {
  loading: boolean;
  failure: string | null;
  empty: boolean;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  if (loading) {
    return (
      <p className="pc-meta inline-flex items-center gap-2" role="status">
        <Loader2 size={13} className="animate-spin" aria-hidden="true" />
        {t("podcasts.chapters.loading")}
      </p>
    );
  }
  if (failure != null) {
    return (
      <div className="pc-notice" role="status" data-tone="warn">
        <p className="min-w-0 flex-1">
          {t("podcasts.chapters.failed", {
            reason: podcastFailureMessage(t, failure || null),
          })}
        </p>
        <button type="button" className="pc-link" onClick={onRetry}>
          {t("podcasts.retry")}
        </button>
      </div>
    );
  }
  return empty ? (
    <p className="pc-meta">{t("podcasts.chapters.none")}</p>
  ) : null;
}

function useJumpTo(episodeId: number) {
  const { t } = useTranslation();
  const { jump, inPlayer } = useEpisodeJump();
  const [failed, setFailed] = useState<string | null>(null);
  const go = (chapter: PodcastChapter) => {
    setFailed(null);
    jump(episodeId, chapter.start_ms).catch((err) => {
      const word = podcastErrorWord(err);
      if (word == null) console.error("[podcasts] jump failed", err);
      setFailed(podcastFailureMessage(t, word));
    });
  };
  return { go, failed, playing: inPlayer === episodeId };
}

/** On an episode's page: one virtual list, driven by the page's scroller. */
export function PodcastPageChapters({ episodeId }: { episodeId: number }) {
  const { t } = useTranslation();
  const { chapters, failure, loading, retry } = useEpisodeChapters(episodeId);
  const { go, failed, playing } = useJumpTo(episodeId);
  const current = useCurrentChapter(chapters, playing);
  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: chapters.length,
    estimateSize: () => ROW_HEIGHT,
  });
  return (
    <section data-podcast-chapters>
      <h2 className="pc-heading__title mb-3">{t("podcasts.chapters.title")}</h2>
      <ChaptersState
        loading={loading}
        failure={failure}
        empty={chapters.length === 0}
        onRetry={retry}
      />
      {failed && (
        <p className="pc-notice mb-2" role="alert" data-tone="warn">
          {failed}
        </p>
      )}
      <div ref={parentRef}>
        <div
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            position: "relative",
          }}
        >
          {virtualizer.getVirtualItems().map((row) => {
            const chapter = chapters[row.index];
            if (!chapter) return null;
            return (
              <div
                key={row.key}
                ref={virtualizer.measureElement}
                data-index={row.index}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${row.start - scrollMargin}px)`,
                }}
              >
                <ChapterRow
                  chapter={chapter}
                  current={row.index === current}
                  onJump={go}
                />
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

/**
 * In the player's panel, for the episode it holds. Virtual against a scroller of
 * its own, since the panel is a column with its own and a feed may name 2 000. It
 * stays out of the way when there are none: the panel shows more than chapters.
 */
export function PodcastPanelChapters({
  episodeId,
  className = "",
}: {
  episodeId: number;
  className?: string;
}) {
  "use no memo";
  const { t } = useTranslation();
  const { chapters, failure, loading, retry } = useEpisodeChapters(episodeId);
  const { go, failed, playing } = useJumpTo(episodeId);
  const current = useCurrentChapter(chapters, playing);
  const scrollRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: chapters.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 6,
  });

  useEffect(() => {
    if (current >= 0) virtualizer.scrollToIndex(current, { align: "center" });
  }, [current, virtualizer]);

  if (!loading && failure == null && chapters.length === 0) return null;
  return (
    <div data-podcasts data-podcast-chapters className={className}>
      <h3 className="mb-2 text-xs font-bold uppercase tracking-widest text-zinc-400">
        {t("podcasts.chapters.title")}
      </h3>
      <ChaptersState
        loading={loading}
        failure={failure}
        empty={false}
        onRetry={retry}
      />
      {failed && (
        <p className="pc-notice mb-2" role="alert" data-tone="warn">
          {failed}
        </p>
      )}
      {chapters.length > 0 && (
        <div ref={scrollRef} className="max-h-72 overflow-y-auto">
          <div
            style={{
              height: `${virtualizer.getTotalSize()}px`,
              position: "relative",
            }}
          >
            {virtualizer.getVirtualItems().map((row) => {
              const chapter = chapters[row.index];
              if (!chapter) return null;
              return (
                <div
                  key={row.key}
                  ref={virtualizer.measureElement}
                  data-index={row.index}
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    transform: `translateY(${row.start}px)`,
                  }}
                >
                  <ChapterRow
                    chapter={chapter}
                    current={row.index === current}
                    onJump={go}
                  />
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
