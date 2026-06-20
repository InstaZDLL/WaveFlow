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
  AlertCircle,
  ChevronDown,
  Check,
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { pickFile } from "../../lib/tauri/dialog";
import {
  clearLyrics,
  fetchLyrics,
  findActiveLineIndex,
  findActiveWordIndex,
  importLrcFile,
  LYRICS_PROVIDERS,
  parseLyrics,
  refetchLyrics,
  type LyricsLine,
  type LyricsPayload,
  type LyricsProvider,
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
  // Provider picker dropdown — opens on click of the source label so the
  // user can re-query a specific source when the auto-waterfall cached a
  // low-quality hit. Closed by an outside click + by `handleRefetch`
  // itself after the new fetch lands. Anchored to the source-label
  // button via a wrapper `<span class="relative">` so the menu floats
  // above the footer instead of pushing the row.
  const [pickerOpen, setPickerOpen] = useState(false);
  // Attached to the `<span class="relative inline-flex">` wrapper
  // around the source label, so the type must match — using
  // `HTMLDivElement` would compile under React's loose ref typing but
  // any future code that read `pickerRef.current.classList` etc. would
  // get a `DOMTokenList | undefined` mismatch with the actual span.
  const pickerRef = useRef<HTMLSpanElement | null>(null);

  const trackId = currentTrack?.id ?? null;

  // Live mirror of `trackId` so async event handlers can detect
  // when the user switched tracks during an `await` — without it the
  // closure carries whatever `trackId` was current at click time and
  // a stale `refetchLyrics` / `importLrcFile` response would happily
  // overwrite the new track's payload after the user moved on. The
  // initial-fetch useEffect already has its own per-render
  // `cancelled` flag; this ref serves the user-triggered handlers
  // below which can't rely on effect cleanup for the same job.
  const trackIdRef = useRef<number | null>(trackId);
  useEffect(() => {
    trackIdRef.current = trackId;
  }, [trackId]);

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
      // Drop any error left from a prior failed fetch — otherwise the
      // error-vs-notFound conditional below would mask the freshly
      // imported lyrics behind the stale error state.
      setError(null);
    } catch (err) {
      console.error("[LyricsPanel] import failed", err);
      setError(String(err));
    }
  };

  const handleRefetch = async (provider?: LyricsProvider) => {
    if (trackId == null) return;
    // Capture the requested track at the call site so we can detect a
    // mid-flight switch by comparing against the live `trackIdRef`
    // when the await resolves. Without this guard a refetch on track
    // A that takes longer than the user's switch to track B would
    // land its result into B's payload (CodeRabbit-flagged race).
    const requestedTrackId = trackId;
    try {
      // `refetchLyrics` drops the cache row + re-queries in one Tauri
      // call. With `provider = undefined` it re-runs the full waterfall
      // (legacy behaviour). With `provider` set it queries ONLY that
      // source, bypassing local tiers — the path the user takes when
      // the auto-fetch cached a low-quality hit from one provider and
      // they want to try a different one (issue #284).
      setIsFetching(true);
      setPickerOpen(false);
      const next = await refetchLyrics(requestedTrackId, provider);
      if (requestedTrackId !== trackIdRef.current) return;
      setPayload(next);
      // Same as handleImport: clear any previous error so the refetched
      // payload actually paints instead of being shadowed by stale state.
      setError(null);
    } catch (err) {
      console.error("[LyricsPanel] refetch failed", err);
      // Don't surface an error for a track the user no longer cares
      // about — the new track's useEffect already handles its own
      // error state.
      if (requestedTrackId !== trackIdRef.current) return;
      setError(String(err));
    } finally {
      // Only clear the spinner when we're still on the same track.
      // After a switch, the new track's useEffect has already flipped
      // `isFetching` to `true` for its own request and our clear
      // would race that state.
      if (requestedTrackId === trackIdRef.current) {
        setIsFetching(false);
      }
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

  // Close the provider picker on any click outside the menu (or its
  // anchor button). The menu has no portal — it sits inside the panel
  // root — so a single capture-phase mousedown listener on the document
  // is enough; we just skip when the click landed inside the menu.
  useEffect(() => {
    if (!pickerOpen) return;
    const handler = (e: MouseEvent) => {
      if (
        pickerRef.current &&
        e.target instanceof Node &&
        pickerRef.current.contains(e.target)
      ) {
        return;
      }
      setPickerOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => {
      document.removeEventListener("mousedown", handler);
    };
  }, [pickerOpen]);

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
          ) : error ? (
            // Transient error (network, DB, profile pool) — distinct
            // from a cached empty miss so the user knows to retry via
            // the refetch button below rather than reaching for Import.
            // The raw error stays in console.error; the panel surfaces
            // a localized, action-oriented message.
            <EmptyState
              icon={<AlertCircle size={40} />}
              text={t("lyrics.fetchError")}
            />
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
              {/* Source label is a chip-button when API-sourced + an
                  enabled track id is in scope, so the user can pop the
                  provider picker and re-query a different source.
                  Embedded / sidecar / manual rows render as static text
                  — the picker would have nothing meaningful to do for
                  a tag-embedded lyric. */}
              <span ref={pickerRef} className="relative inline-flex">
                {payload && payload.source === "api" ? (
                  <button
                    type="button"
                    onClick={() => setPickerOpen((v) => !v)}
                    disabled={isFetching}
                    aria-haspopup="menu"
                    aria-expanded={pickerOpen}
                    title={t("lyrics.source.pickerHint")}
                    className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50 truncate"
                  >
                    <span className="truncate">
                      {sourceLabel(payload, t)}
                    </span>
                    <ChevronDown size={11} className="shrink-0" />
                  </button>
                ) : (
                  <span className="truncate">
                    {payload ? sourceLabel(payload, t) : ""}
                  </span>
                )}
                {pickerOpen && (
                  <div
                    role="menu"
                    aria-label={t("lyrics.source.pickerHint")}
                    className="absolute left-0 bottom-full mb-1 z-20 min-w-44 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 shadow-lg p-1"
                  >
                    {LYRICS_PROVIDERS.map((p) => {
                      const isActive = payload?.provider === p;
                      return (
                        <button
                          key={p}
                          type="button"
                          role="menuitemradio"
                          aria-checked={isActive}
                          onClick={() => handleRefetch(p)}
                          className="w-full flex items-center justify-between gap-2 px-2.5 py-1.5 rounded text-xs hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors text-zinc-700 dark:text-zinc-200"
                        >
                          <span>{t(`lyrics.provider.${p}`)}</span>
                          {isActive && (
                            <Check
                              size={12}
                              className="shrink-0 text-emerald-500"
                            />
                          )}
                        </button>
                      );
                    })}
                  </div>
                )}
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
                onClick={() => handleRefetch()}
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
  payload: LyricsPayload,
  t: (key: string) => string,
): string {
  switch (payload.source) {
    case "embedded":
      return t("lyrics.source.embedded");
    case "lrc_file":
      return t("lyrics.source.lrc_file");
    case "api":
      // Surface the actual provider when known (LRCLIB / Genius /
      // NetEase / Megalobiz / Musixmatch). Pre-1.5.1 rows + the
      // empty-miss rows still leave `provider` null — fall back to
      // the generic "Online source" label so the badge stays
      // informative without lying about which provider ran.
      return payload.provider
        ? t(`lyrics.provider.${payload.provider}`)
        : t("lyrics.source.api");
    case "manual":
      return t("lyrics.source.manual");
  }
}
