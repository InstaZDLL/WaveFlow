import { Fragment, useEffect, useRef, useState } from "react";
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
import { useTrackLyrics } from "../../hooks/useTrackLyrics";
import {
  LYRICS_PROVIDERS,
  type LyricsLine,
  type LyricsPayload,
  type LyricsProvider,
} from "../../lib/tauri/lyrics";
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
  const { isLyricsOpen, toggleLyrics, currentTrack, openFullscreenLyrics } =
    usePlayer();

  // All lyrics fetch / parse / active-line state + the import / refetch /
  // clear mutations live in the shared hook so the immersive view reuses
  // the exact same logic (and staleness guards) without a second fetch
  // path. Auto-scroll stays here — it's view-local (own ref array).
  const {
    payload,
    isFetching,
    error,
    lrcLines,
    isSynced,
    radioPlainText,
    isRadio,
    activeIndex,
    activeWordIndex,
    importLyrics,
    refetch,
    clear,
    seekToLine,
    applyPayload,
  } = useTrackLyrics();

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

  // ── Active-line auto-scroll (view-local) ─────────────────────────
  // The active index itself comes from the shared hook; only the
  // scroll-into-view (which targets *this* panel's line nodes) lives
  // here. The immersive view keeps its own ref array + scroll effect.
  const lineRefs = useRef<Array<HTMLLIElement | null>>([]);
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!isLyricsOpen || !isSynced || activeIndex < 0) return;
    const node = lineRefs.current[activeIndex];
    if (node) {
      node.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  }, [activeIndex, isLyricsOpen, isSynced]);

  // Provider picker closes itself, then the shared refetch runs.
  const handleRefetch = async (provider?: LyricsProvider) => {
    setPickerOpen(false);
    await refetch(provider);
  };

  const handleClear = () => {
    void clear();
  };

  const handleSeekToLine = (line: LyricsLine) => {
    seekToLine(line);
  };

  // Close the provider picker on any click outside the menu (or its
  // anchor button) AND on Escape — both gestures are part of the
  // WAI-ARIA menu dismissal pattern. The mousedown handler skips
  // when the click landed inside the menu wrapper; the keydown
  // handler is unconditional so Escape closes the menu regardless
  // of whether focus sits on the trigger button (post-click default)
  // or on a menu item (after a Tab). The menu has no portal — it
  // sits inside the panel root — so document-level listeners are
  // enough.
  useEffect(() => {
    if (!pickerOpen) return;
    const handleMouseDown = (e: MouseEvent) => {
      if (
        pickerRef.current &&
        e.target instanceof Node &&
        pickerRef.current.contains(e.target)
      ) {
        return;
      }
      setPickerOpen(false);
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setPickerOpen(false);
      }
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown);
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
              disabled={currentTrack == null || isRadio}
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
              {isRadio ? radioPlainText : payload.content}
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
          onSaved={(next) => applyPayload(next)}
        />

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
                {payload && payload.source === "api" && !isRadio ? (
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
                {pickerOpen && !isRadio && (
                  <div
                    role="menu"
                    aria-label={t("lyrics.source.pickerHint")}
                    onKeyDown={(e) => {
                      // Local Escape handler in addition to the
                      // document-level one in the picker useEffect.
                      // Redundant in practice (both fire on the same
                      // event) but keeps the WAI-ARIA menu pattern
                      // self-contained on the element itself + stops
                      // the event from bubbling further into ancestor
                      // panels that might also listen for Escape.
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        setPickerOpen(false);
                      }
                    }}
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
              {/* Import / refetch / clear operate on a library row
                  (track_id / file_hash); a radio session has neither, so
                  they're hidden for radio — the lyrics auto-fetch by
                  artist + title and there's nothing to import-to or
                  clear-from. */}
              {!isRadio && (
                <>
                  <button
                    type="button"
                    onClick={() => void importLyrics()}
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
                </>
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
