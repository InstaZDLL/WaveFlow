import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Music2, Upload, RefreshCcw, Trash2 } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { pickFile } from "../../lib/tauri/dialog";
import {
  clearLyrics,
  fetchLyrics,
  findActiveLineIndex,
  importLrcFile,
  parseLrc,
  type LrcLine,
  type LyricsPayload,
} from "../../lib/tauri/lyrics";

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
  const { isLyricsOpen, toggleLyrics, currentTrack, positionMs, seek } =
    usePlayer();

  const [payload, setPayload] = useState<LyricsPayload | null>(null);
  const [isFetching, setIsFetching] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  // ── Parse LRC once per content change ───────────────────────────
  const lrcLines = useMemo<LrcLine[]>(() => {
    if (!payload) return [];
    if (payload.format !== "lrc" && payload.format !== "enhanced_lrc") {
      return [];
    }
    return parseLrc(payload.content);
  }, [payload]);

  const isSynced = lrcLines.length > 0;

  // ── Active-line tracking with auto-scroll ───────────────────────
  const [activeIndex, setActiveIndex] = useState(-1);
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!isSynced) return;
    const idx = findActiveLineIndex(lrcLines, positionMs, Math.max(activeIndex, 0));
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

  // ── Actions ─────────────────────────────────────────────────────
  const handleImport = async () => {
    if (trackId == null) return;
    try {
      const path = await pickFile(["lrc", "txt"], t("lyrics.importTitle"));
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

  const handleSeekToLine = (line: LrcLine) => {
    seek(line.timeMs).catch(() => {});
  };

  // ── Render ──────────────────────────────────────────────────────
  return (
    <div
      className="absolute top-0 right-0 h-full w-80 shadow-2xl z-40 border-l bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-900 dark:border-zinc-800 dark:text-zinc-100"
    >
      <div className="flex flex-col h-full">
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
          <button
            type="button"
            onClick={toggleLyrics}
            aria-label={t("common.close")}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors shrink-0"
          >
            <X size={18} />
          </button>
        </div>

        {/* Body */}
        <div ref={containerRef} className="flex-1 overflow-y-auto px-6 py-4">
          {currentTrack == null ? (
            <EmptyState icon={<Music2 size={40} />} text={t("lyrics.noTrack")} />
          ) : isFetching && !payload ? (
            <EmptyState text={t("lyrics.loading")} />
          ) : error ? (
            <EmptyState text={error} />
          ) : !payload || payload.content.trim() === "" ? (
            <EmptyState
              icon={<Music2 size={40} />}
              text={t("lyrics.notFound")}
            />
          ) : isSynced ? (
            <ul className="space-y-3 py-32">
              {lrcLines.map((line, index) => {
                const isActive = index === activeIndex;
                const isPast = index < activeIndex;
                return (
                  <li
                    key={`${line.timeMs}-${index}`}
                    ref={(el) => {
                      lineRefs.current[index] = el;
                    }}
                    onClick={() => handleSeekToLine(line)}
                    className={`text-base leading-relaxed cursor-pointer transition-all select-none ${
                      isActive
                        ? "text-zinc-900 dark:text-white font-semibold scale-[1.02]"
                        : isPast
                          ? "text-zinc-300 dark:text-zinc-600"
                          : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                    }`}
                  >
                    {line.text || " "}
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

        {/* Footer actions */}
        {currentTrack != null && (
          <div className="flex items-center justify-between p-4 border-t border-zinc-100 dark:border-zinc-800 text-xs text-zinc-500 dark:text-zinc-400">
            <span className="truncate">
              {payload ? sourceLabel(payload.source, t) : ""}
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
    </div>
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
