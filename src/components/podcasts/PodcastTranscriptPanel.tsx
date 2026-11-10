import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { useVirtualizer } from "@tanstack/react-virtual";
import { FileText, Loader2, X } from "lucide-react";

import { useEpisodeJump } from "../../hooks/useEpisodeJump";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlayerPositionValue } from "../../hooks/usePlayerPosition";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { lyricsLineClass } from "../../lib/lyricsLine";
import { indexAtPosition } from "../../lib/podcastTimeline";
import { podcastFailureMessage } from "../../lib/podcastWords";
import {
  podcastEpisodeTranscript,
  podcastErrorWord,
  type PodcastEpisodeTranscript,
  type PodcastTranscriptLine,
} from "../../lib/tauri/podcast";
import { formatDuration } from "../../lib/tauri/track";

/** How long the list leaves someone reading alone after they moved it themselves. */
const FOLLOW_PAUSE_MS = 8_000;

type Read =
  | { state: "loading" }
  | { state: "failed"; word: string | null }
  | { state: "ready"; answer: PodcastEpisodeTranscript };

/**
 * An episode's transcript, followed line by line as lyrics are. A panel of its own:
 * it is not lyrics, and neither their providers nor their editor have anything to
 * say about it. Asked for when this panel opens and not before: opening it is what
 * contacts the host, and only when nothing was kept.
 */
export function PodcastTranscriptPanel({
  embedded = false,
}: {
  embedded?: boolean;
} = {}) {
  const { t } = useTranslation();
  const { closeRightPanel, transcriptEpisodeId } = usePlayer();
  const episodeId = transcriptEpisodeId;
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<{ key: string; read: Read } | null>(null);
  const key = `${episodeId}:${attempt}`;

  useEffect(() => {
    if (episodeId == null) return;
    let cancelled = false;
    podcastEpisodeTranscript(episodeId)
      .then((answer) => {
        if (!cancelled) setRead({ key, read: { state: "ready", answer } });
      })
      .catch((err) => {
        const word = podcastErrorWord(err);
        if (word == null) console.error("[podcasts] transcript failed", err);
        if (!cancelled) setRead({ key, read: { state: "failed", word } });
      });
    return () => {
      cancelled = true;
    };
  }, [episodeId, key]);

  const shown: Read = read?.key === key ? read.read : { state: "loading" };

  const body = (
    <div
      data-podcast-transcript
      className={
        embedded ? "flex h-full flex-col" : "flex h-full w-80 flex-col"
      }
    >
      {!embedded && (
        <div className="flex items-center justify-between border-b border-zinc-100 p-6 pb-4 dark:border-zinc-800">
          <h2 className="text-sm font-bold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
            {t("podcasts.transcript.title")}
          </h2>
          <button
            type="button"
            onClick={closeRightPanel}
            aria-label={t("common.close")}
            className="rounded-full p-2 transition-colors hover:bg-zinc-100 dark:hover:bg-zinc-800"
          >
            <X size={18} />
          </button>
        </div>
      )}
      {episodeId == null ? (
        <PanelMessage text={t("podcasts.transcript.noEpisode")} icon />
      ) : shown.state === "loading" ? (
        <PanelMessage text={t("podcasts.transcript.loading")} spinner />
      ) : shown.state === "failed" ? (
        <PanelMessage
          text={t("podcasts.transcript.failed", {
            reason: podcastFailureMessage(t, shown.word),
          })}
          retry={() => setAttempt((value) => value + 1)}
        />
      ) : !shown.answer.available ? (
        <PanelMessage text={t("podcasts.transcript.none")} icon />
      ) : (
        <TranscriptLines
          key={episodeId}
          episodeId={episodeId}
          answer={shown.answer}
        />
      )}
    </div>
  );

  if (embedded) return <div className="h-full">{body}</div>;
  return (
    <motion.aside
      key="transcript"
      initial={{ width: 0, opacity: 0 }}
      animate={{ width: 320, opacity: 1 }}
      exit={{ width: 0, opacity: 0 }}
      transition={{ type: "spring", stiffness: 320, damping: 32, mass: 0.8 }}
      className="h-full shrink-0 overflow-hidden border-s bg-surface-light border-zinc-200 text-zinc-800 dark:bg-surface-dark dark:border-zinc-800 dark:text-zinc-100"
    >
      {body}
    </motion.aside>
  );
}

function PanelMessage({
  text,
  icon = false,
  spinner = false,
  retry,
}: {
  text: string;
  icon?: boolean;
  spinner?: boolean;
  retry?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div
      className="flex flex-1 flex-col items-center justify-center gap-3 px-6 py-16 text-center text-zinc-400"
      role={retry ? "alert" : "status"}
    >
      {icon && <FileText size={40} aria-hidden="true" />}
      {spinner && (
        <Loader2 size={20} className="animate-spin" aria-hidden="true" />
      )}
      <p className="text-sm">{text}</p>
      {retry && (
        <button
          type="button"
          onClick={retry}
          className="text-sm font-medium text-zinc-600 underline-offset-2 hover:underline dark:text-zinc-300"
        >
          {t("podcasts.retry")}
        </button>
      )}
    </div>
  );
}

/**
 * The lines, virtual against the panel's scroller: a transcript may hold 50 000, of
 * whatever height a long sentence takes. Every line is React text, so a tag that
 * got this far is shown as the characters it is. Re-renders when the line under the
 * position changes, not at every tick.
 */
function TranscriptLines({
  episodeId,
  answer,
}: {
  episodeId: number;
  answer: PodcastEpisodeTranscript;
}) {
  "use no memo";
  const { t } = useTranslation();
  const { jump, inPlayer } = useEpisodeJump();
  const reducedMotion = usePrefersReducedMotion();
  const { lines, timed } = answer;
  const playing = inPlayer === episodeId;
  const [failed, setFailed] = useState<string | null>(null);
  const active = usePlayerPositionValue((ms) =>
    playing && timed
      ? indexAtPosition(lines.length, (i) => lines[i].start_ms, ms)
      : -1,
  );

  const scrollRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 38,
    overscan: 10,
  });

  // A hand on the list leaves it alone for a moment: someone reading ahead should
  // not be pulled back every few seconds.
  const movedAt = useRef(0);
  const noteMoved = useCallback(() => {
    movedAt.current = performance.now();
  }, []);
  useEffect(() => {
    if (active < 0) return;
    if (performance.now() - movedAt.current < FOLLOW_PAUSE_MS) return;
    const scroller = scrollRef.current;
    const target = virtualizer.getOffsetForIndex(active, "center");
    if (!scroller || !target) return;
    // The next line glides, by hand since `scrollToIndex` does not glide over rows
    // of different heights. A jump goes through it: the rows it lands on have
    // only been estimated, and it settles once they are measured.
    const nearby =
      Math.abs(target[0] - scroller.scrollTop) < scroller.clientHeight;
    if (nearby && !reducedMotion) {
      scroller.scrollTo({ top: target[0], behavior: "smooth" });
    } else {
      virtualizer.scrollToIndex(active, { align: "center" });
    }
  }, [active, reducedMotion, virtualizer]);

  const go = (line: PodcastTranscriptLine) => {
    if (line.start_ms == null) return;
    setFailed(null);
    movedAt.current = 0;
    jump(episodeId, line.start_ms).catch((err) => {
      const word = podcastErrorWord(err);
      if (word == null) console.error("[podcasts] jump failed", err);
      setFailed(podcastFailureMessage(t, word));
    });
  };

  return (
    <>
      {(!timed || !playing || failed) && (
        <p
          className="border-b border-zinc-100 px-6 py-2 text-xs text-zinc-500 dark:border-zinc-800 dark:text-zinc-400"
          role={failed ? "alert" : undefined}
        >
          {failed ??
            t(
              timed
                ? "podcasts.transcript.notPlaying"
                : "podcasts.transcript.untimed",
            )}
        </p>
      )}
      <div
        ref={scrollRef}
        onWheel={noteMoved}
        onPointerDown={noteMoved}
        onKeyDown={noteMoved}
        className="min-h-0 flex-1 overflow-y-auto px-6 py-4"
      >
        <ul
          style={{
            height: `${virtualizer.getTotalSize()}px`,
            position: "relative",
          }}
        >
          {virtualizer.getVirtualItems().map((row) => {
            const line = lines[row.index];
            if (!line) return null;
            const speakerChanged =
              line.speaker != null &&
              line.speaker !== lines[row.index - 1]?.speaker;
            return (
              <li
                key={row.key}
                ref={virtualizer.measureElement}
                data-index={row.index}
                className="pb-3"
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${row.start}px)`,
                }}
              >
                {speakerChanged && (
                  <div
                    className="mb-0.5 truncate text-[11px] font-semibold uppercase tracking-wide text-zinc-400"
                    dir="auto"
                  >
                    {line.speaker}
                  </div>
                )}
                {timed ? (
                  // One level down from the `li` on purpose: the fluid layout fades
                  // and blurs `li > button` by its distance from the sung line, and
                  // a transcript is there to be read.
                  <div>
                    <button
                      type="button"
                      dir="auto"
                      data-transcript-line={row.index}
                      data-past={row.index < active || undefined}
                      aria-current={row.index === active || undefined}
                      aria-label={t("podcasts.transcript.jump", {
                        time: formatDuration(line.start_ms ?? 0),
                        text: line.text,
                      })}
                      onClick={() => go(line)}
                      className={`block w-full text-start ${lyricsLineClass(
                        row.index === active,
                        row.index < active,
                      )}`}
                    >
                      {line.text}
                    </button>
                  </div>
                ) : (
                  <p
                    dir="auto"
                    className="text-sm leading-relaxed text-zinc-700 dark:text-zinc-200"
                  >
                    {line.text}
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}
