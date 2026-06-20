import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import {
  X,
  Music2,
  Upload,
  RefreshCcw,
  Trash2,
  Maximize2,
  Pencil,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { pickFile } from "../../lib/tauri/dialog";
import {
  clearLyrics,
  fetchLyrics,
  findActiveLineIndex,
  findActiveWordIndex,
  importLrcFile,
  parseLyrics,
  type LyricsLine,
  type LyricsPayload,
} from "../../lib/tauri/lyrics";
import { FullscreenLyrics } from "../player/FullscreenLyrics";
import { LyricsEditorModal } from "../common/LyricsEditorModal";

/**
 * Spotify-style right-edge panel showing the currently-playing track's
 * lyrics. Resolves them lazily via the three-tier `fetch_lyrics`
 * command (cache → embedded tag → LRCLIB).
 *
 * Renders synchronized LRC lyrics with a karaoke-style highlight when
 * timestamps are present; falls back to plain wrapped text otherwise.
 *
 * Shares the w-80 right-edge slot with `QueuePanel` and
 * `NowPlayingPanel` via mutual exclusion in `PlayerContext`.
 */
export function LyricsPanel() {
  const { t } = useTranslation();
  const {
    isLyricsOpen,
    toggleLyrics,
    currentTrack,
    positionMs,
    seek,
    isFullscreenLyricsOpen,
    openFullscreenLyrics,
    closeFullscreenLyrics,
    openFullscreenNowPlaying,
  } = usePlayer();

  const [payload, setPayload] = useState<LyricsPayload | null>(null);
  const [isFetching, setIsFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [isEditing, setIsEditing] = useState(false);

  const trackId = currentTrack?.id ?? null;

  // ── Fetch when the focused track changes (or panel opens) ───────
  useEffect(() => {
    if (trackId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPayload(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setIsFetching(true);
    setError(null);
    fetchLyrics(trackId)
      .then((p) => {
        if (cancelled) return;
        setPayload(p);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("[LyricsPanel] fetch failed", err);
        setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setIsFetching(false);
      });
    return () => {
      cancelled = true;
    };
  }, [trackId]);

  // ── Parse lyrics once per content change ─────────────────────────
  const lrcLines = useMemo<LyricsLine[]>(() => {
    if (!payload) return [];
    return parseLyrics(payload.content, payload.format);
  }, [payload]);

  const isSynced = lrcLines.length > 0;

  // ── Active-line tracking with auto-scroll ───────────────────────
  const [activeIndex, setActiveIndex] = useState(-1);
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!isSynced) return;
    const idx = findActiveLineIndex(
      lrcLines,
      positionMs,
      Math.max(activeIndex, 0),
    );
    if (idx !== activeIndex) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setActiveIndex(idx);
    }
  }, [positionMs, lrcLines, isSynced, activeIndex]);

  useEffect(() => {
    if (!isLyricsOpen || !isSynced || activeIndex < 0) return;
    const node = lineRefs.current[activeIndex];
    if (node) {
      node.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [activeIndex, isLyricsOpen, isSynced]);

  // Active word inside the active line — only computed when the line
  // carries `words[]` so plain LRC stays cheap.
  const activeLine = activeIndex >= 0 ? lrcLines[activeIndex] : undefined;
  const activeWordIndex = useMemo(() => {
    if (!activeLine?.words || activeLine.words.length === 0) return -1;
    return findActiveWordIndex(activeLine.words, positionMs);
  }, [activeLine, positionMs]);

  // ── Actions ─────────────────────────────────────────────────────
  const handleImport = async () => {
    if (trackId == null) return;
    try {
      const path = await pickFile(
        ["lrc", "elrc", "ttml", "xml", "txt"],
        t("lyrics.importTitle"),
      );
      if (!path) return;
      const next = await importLrcFile(trackId, path);
      setPayload(next);
    } catch (err) {
      console.error("[LyricsPanel] import failed", err);
      setError(String(err));
    }
  };

  const handleRefetch = async () => {
    if (trackId == null) return;
    try {
      // Drop the cache then re-run the waterfall.
      await clearLyrics(trackId);
      setIsFetching(true);
      const next = await fetchLyrics(trackId);
      setPayload(next);
    } catch (err) {
      console.error("[LyricsPanel] refetch failed", err);
      setError(String(err));
    } finally {
      setIsFetching(false);
    }
  };

  const handleClear = async () => {
    if (trackId == null) return;
    try {
      await clearLyrics(trackId);
      setPayload(null);
    } catch (err) {
      console.error("[LyricsPanel] clear failed", err);
    }
  };

  const handleSeekToLine = (line: LyricsLine) => {
    seek(line.timeMs).catch(() => {});
  };

  // ── Render ──────────────────────────────────────────────────────
  return (
    <motion.aside
      key="lyrics"
      initial={{ width: 0, opacity: 0 }}
      animate={{ width: 320, opacity: 1 }}
      exit={{ width: 0, opacity: 0 }}
      transition={{ type: "spring", stiffness: 320, damping: 32, mass: 0.8 }}
      className="h-full shrink-0 overflow-hidden border-l bg-surface-light border-zinc-200 text-zinc-800 dark:bg-surface-dark dark:border-zinc-800 dark:text-zinc-100"
    >
      <div className="flex flex-col h-full w-80">
        {/* Header */}
        <div className="flex items-center justify-between p-6 pb-4 border-b border-zinc-100 dark:border-zinc-800">
          <div className="min-w-0">
            <h2 className="text-sm font-bold tracking-widest uppercase text-zinc-500 dark:text-zinc-400">
              {t("lyrics.title")}
            </h2>
            {currentTrack && (
              <p className="text-xs text-zinc-400 truncate mt-1">
                {currentTrack.title}
              </p>
            )}
          </div>
          <div className="flex items-center gap-1 shrink-0">
            <button
              type="button"
              onClick={() => setIsEditing(true)}
              aria-label={t("lyrics.actions.edit")}
              title={t("lyrics.actions.edit")}
              disabled={currentTrack == null}
              className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <Pencil size={16} />
            </button>
            <button
              type="button"
              onClick={openFullscreenLyrics}
              aria-label={t("lyrics.actions.fullscreen")}
              title={t("lyrics.actions.fullscreen")}
              disabled={currentTrack == null}
              className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              <Maximize2 size={16} />
            </button>
            <button
              type="button"
              onClick={toggleLyrics}
              aria-label={t("common.close")}
              className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            >
              <X size={18} />
            </button>
          </div>
        </div>

        {/* Body */}
        <div ref={containerRef} className="flex-1 overflow-y-auto px-6 py-4">
          {currentTrack == null ? (
            <EmptyState
              icon={<Music2 size={40} />}
              text={t("lyrics.noTrack")}
            />
          ) : isFetching && !payload ? (
            <EmptyState text={t("lyrics.loading")} />
          ) : error || !payload || payload.content.trim() === "" ? (
            <EmptyState
              icon={<Music2 size={40} />}
              text={t("lyrics.notFound")}
            />
          ) : isSynced ? (
            <ul className="space-y-3 py-32">
              {lrcLines.map((line, index) => {
                const isActive = index === activeIndex;
                const isPast = index < activeIndex;
                const hasWords = isActive && (line.words?.length ?? 0) > 0;
                return (
                  <li
                    key={`${line.timeMs}-${index}`}
                    ref={(el) => {
                      lineRefs.current[index] = el;
                    }}
                  >
                    <button
                      type="button"
                      onClick={() => handleSeekToLine(line)}
                      className={`block w-full text-left text-base leading-relaxed cursor-pointer transition-all select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded ${
                        isActive
                          ? "text-zinc-900 dark:text-white font-semibold scale-[1.02]"
                          : isPast
                            ? "text-zinc-300 dark:text-zinc-600"
                            : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                      }`}
                    >
                      {hasWords ? (
                        <span>
                          {line.words!.map((word, wi) => (
                            // See FullscreenLyrics for the rationale:
                            // `inline-block` strips the JSX whitespace
                            // that would normally separate inline
                            // siblings, and many Enhanced LRC sources
                            // omit spaces between word stamps. A literal
                            // `" "` text node restores the gap; if the
                            // source did carry trailing whitespace in
                            // `word.text`, `white-space: normal`
                            // collapses the pair to one.
                            <Fragment key={wi}>
                              <span
                                className={
                                  wi === activeWordIndex
                                    ? "text-pink-500 dark:text-pink-400"
                                    : wi < activeWordIndex
                                      ? ""
                                      : "opacity-60"
                                }
                                style={{
                                  display: "inline-block",
                                  transform:
                                    wi === activeWordIndex
                                      ? "scale(1.04)"
                                      : "scale(1)",
                                  transition:
                                    "color 150ms ease, opacity 150ms ease, transform 150ms ease",
                                }}
                              >
                                {word.text}
                              </span>
                              {wi < line.words!.length - 1 && " "}
                            </Fragment>
                          ))}
                        </span>
                      ) : (
                        line.text || " "
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="text-sm leading-relaxed text-zinc-700 dark:text-zinc-200 whitespace-pre-line">
              {payload.content}
            </p>
          )}
        </div>

        {/* Editor modal — sibling so it floats above the panel. */}
        <LyricsEditorModal
          isOpen={isEditing}
          onClose={() => setIsEditing(false)}
          trackId={trackId}
          trackTitle={currentTrack?.title ?? null}
          trackFilePath={currentTrack?.file_path ?? null}
          initial={payload}
          onSaved={(next) => setPayload(next)}
        />

        {/* Fullscreen overlay — portalled to `document.body` so it
            escapes the `motion.aside`'s opacity/width animation; without
            the portal the overlay inherits the parent's `opacity: 0`
            tween at mount and the app background flashes through during
            the immersive→lyrics transition (the reverse direction is
            unaffected because LyricsPanel is already fully opaque by
            then). Mounted as a portal sibling at the document root keeps
            the panel as the owner of the lyrics fetch / parse state. */}
        {isFullscreenLyricsOpen &&
          currentTrack &&
          createPortal(
            <FullscreenLyrics
              track={currentTrack}
              payload={payload}
              lrcLines={lrcLines}
              isSynced={isSynced}
              activeIndex={activeIndex}
              isFetching={isFetching}
              error={error}
              onClose={closeFullscreenLyrics}
              onOpenNowPlaying={openFullscreenNowPlaying}
              onSeek={handleSeekToLine}
            />,
            document.body,
          )}

        {/* Footer actions */}
        {currentTrack != null && (
          <div className="flex items-center justify-between p-4 border-t border-zinc-100 dark:border-zinc-800 text-xs text-zinc-500 dark:text-zinc-400">
            <span className="flex items-center gap-2 min-w-0">
              <span className="truncate">
                {payload ? sourceLabel(payload.source, t) : ""}
              </span>
              {payload &&
                (payload.format === "enhanced_lrc" ||
                  payload.format === "ttml") && (
                  <span
                    className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium uppercase tracking-wider bg-pink-100 dark:bg-pink-950/40 text-pink-600 dark:text-pink-300"
                    title={t(`lyrics.format.${payload.format}`)}
                  >
                    {payload.format === "ttml" ? "TTML" : "WORD"}
                  </span>
                )}
            </span>
            <div className="flex items-center space-x-1 shrink-0">
              <button
                type="button"
                onClick={handleImport}
                aria-label={t("lyrics.actions.import")}
                title={t("lyrics.actions.import")}
                className="p-1.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
              >
                <Upload size={14} />
              </button>
              <button
                type="button"
                onClick={handleRefetch}
                disabled={isFetching}
                aria-label={t("lyrics.actions.refetch")}
                title={t("lyrics.actions.refetch")}
                className="p-1.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50"
              >
                <RefreshCcw
                  size={14}
                  className={isFetching ? "animate-spin" : ""}
                />
              </button>
              {payload && (
                <button
                  type="button"
                  onClick={handleClear}
                  aria-label={t("lyrics.actions.clear")}
                  title={t("lyrics.actions.clear")}
                  className="p-1.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors text-zinc-400 hover:text-red-500"
                >
                  <Trash2 size={14} />
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </motion.aside>
  );
}

function EmptyState({ icon, text }: { icon?: React.ReactNode; text: string }) {
  return (
    <div className="flex-1 flex flex-col items-center justify-center text-center text-zinc-400 py-16">
      {icon && <div className="mb-3">{icon}</div>}
      <p className="text-sm">{text}</p>
    </div>
  );
}

function sourceLabel(
  source: LyricsPayload["source"],
  t: (key: string) => string,
): string {
  switch (source) {
    case "embedded":
      return t("lyrics.source.embedded");
    case "lrc_file":
      return t("lyrics.source.lrc_file");
    case "api":
      return t("lyrics.source.api");
    case "manual":
      return t("lyrics.source.manual");
  }
}
