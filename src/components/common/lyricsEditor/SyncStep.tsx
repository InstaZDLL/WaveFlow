import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Minus, Plus } from "lucide-react";
import { usePageVirtualizer } from "../../../hooks/usePageVirtualizer";
import { usePlayerPosition } from "../../../hooks/usePlayerPosition";
import { MAX_REACTION_MS } from "../../../hooks/useLyricsEditorReaction";
import {
  activeLineIndex,
  clearLine,
  clearUnit,
  endLine,
  endUnit,
  lineBegin,
  lineText,
  nudge,
  shiftDocument,
  stampLine,
  stampUnit,
  syncProgress,
} from "../../../lib/lyricsDocumentEdit";
import type { LyricsDocument } from "../../../lib/tauri/lyricsDocument";
import {
  formatEditorTime,
  isTypingTarget,
  keepFocus,
  type EditDocument,
  type SyncCursor,
  type SyncMode,
} from "./shared";

interface SyncStepProps {
  doc: LyricsDocument;
  edit: EditDocument;
  mode: SyncMode;
  setMode: (mode: SyncMode) => void;
  cursor: SyncCursor;
  setCursor: (cursor: SyncCursor) => void;
  /** Subtracted from every tap, to make up for reaction time. */
  reactionMs: number;
  setReactionMs: (ms: number) => void;
  /** The playback position now, extrapolated between position events. */
  estimate: () => number;
  seek: (ms: number) => void;
  togglePlayback: () => void;
  /** Whether the keys are listened to: the step is shown and nothing
   *  covers it. */
  active: boolean;
}

/** The first position at or after `from` that holds a unit. */
function forward(doc: LyricsDocument, from: SyncCursor): SyncCursor | null {
  for (let i = from.line; i < doc.lines.length; i += 1) {
    const start = i === from.line ? from.word : 0;
    if (start < doc.lines[i].words.length) return { line: i, word: start };
  }
  return null;
}

/** The unit just before `from`. */
function backward(doc: LyricsDocument, from: SyncCursor): SyncCursor | null {
  const line = Math.min(from.line, doc.lines.length);
  if (line < doc.lines.length && from.word > 0) {
    return {
      line,
      word: Math.min(from.word, doc.lines[line].words.length) - 1,
    };
  }
  for (let i = line - 1; i >= 0; i -= 1) {
    const n = doc.lines[i].words.length;
    if (n > 0) return { line: i, word: n - 1 };
  }
  return null;
}

/** How far a seek with the arrow keys moves. */
const ARROW_SEEK_MS = 2000;
/** How much earlier than a line ↑ / ↓ start playing, to hear it come. */
const LEAD_IN_MS = 1500;
/** One click on a line's − / + moves it this much. */
const NUDGE_MS = 10;
/** One click on the whole document's − / + moves it this much. */
const SHIFT_MS = 100;

/**
 * The Sync step: the song plays through the app's own player, and each
 * key press stamps the moment it is pressed — a line's start, or a
 * word's in word mode. The reaction offset moves every stamp earlier by
 * the user's own delay between hearing and pressing.
 */
export function SyncStep({
  doc,
  edit,
  mode,
  setMode,
  cursor,
  setCursor,
  reactionMs,
  setReactionMs,
  estimate,
  seek,
  togglePlayback,
  active,
}: SyncStepProps) {
  const { t } = useTranslation();

  // Read by the key handler, so it is registered once per activation and
  // never misses a key between a render and an effect.
  const live = useRef({ doc, cursor, mode, reactionMs });
  useEffect(() => {
    live.current = { doc, cursor, mode, reactionMs };
  });

  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (isTypingTarget(e.target) || e.ctrlKey || e.metaKey || e.altKey) {
        return;
      }
      // A control focused from the keyboard keeps Space and Enter, so
      // the editor can be saved or left without a mouse. The units of
      // the list are the exception: picking one is how a stamp is aimed.
      if (
        (e.code === "Space" ||
          e.code === "Enter" ||
          e.code === "NumpadEnter") &&
        e.target instanceof HTMLElement &&
        e.target.matches(":focus-visible") &&
        e.target.closest("button, a, [role='radio']") &&
        !e.target.closest("[data-sync-list]")
      ) {
        return;
      }
      const { doc, cursor, mode, reactionMs } = live.current;
      const n = doc.lines.length;
      const now = () => Math.max(0, Math.round(estimate() - reactionMs));
      // The cursor is written to the ref at once as well: a second key
      // pressed before the next render must start from this one.
      const go = (next: SyncCursor) => {
        live.current.cursor = next;
        setCursor(next);
      };
      let handled = true;
      if (e.code === "Space") {
        if (mode === "line") {
          // The next line with words: a stanza break is not sung.
          const next = forward(doc, { line: cursor.line, word: 0 });
          if (next) {
            const at = now();
            edit((d) => stampLine(d, next.line, at));
            go({ line: next.line + 1, word: 0 });
          }
        } else {
          const next = forward(doc, cursor);
          if (next) {
            const at = now();
            edit((d) => stampUnit(d, next.line, next.word, at));
            go({ line: next.line, word: next.word + 1 });
          }
        }
      } else if (e.code === "Enter" || e.code === "NumpadEnter") {
        // Where what was just sung stops: a breath, an instrumental.
        const at = now();
        if (mode === "line") {
          const prev = backward(doc, { line: cursor.line, word: 0 });
          if (prev) edit((d) => endLine(d, prev.line, at));
        } else {
          const prev = backward(doc, cursor);
          if (prev) edit((d) => endUnit(d, prev.line, prev.word, at));
        }
      } else if (e.code === "Backspace") {
        if (mode === "line") {
          const prev = backward(doc, { line: cursor.line, word: 0 });
          if (prev) {
            edit((d) => clearLine(d, prev.line));
            go({ line: prev.line, word: 0 });
          }
        } else {
          const prev = backward(doc, cursor);
          if (prev) {
            edit((d) => clearUnit(d, prev.line, prev.word));
            go(prev);
          }
        }
      } else if (e.code === "KeyP") {
        togglePlayback();
      } else if (e.code === "ArrowLeft" || e.code === "ArrowRight") {
        const delta = e.code === "ArrowLeft" ? -ARROW_SEEK_MS : ARROW_SEEK_MS;
        seek(Math.max(0, estimate() + delta));
      } else if (e.code === "ArrowUp" || e.code === "ArrowDown") {
        if (n > 0) {
          const step = e.code === "ArrowUp" ? -1 : 1;
          const line = Math.max(0, Math.min(n - 1, cursor.line + step));
          go({ line, word: 0 });
          const begin = lineBegin(doc.lines[line]);
          if (begin !== undefined && !e.shiftKey) {
            seek(Math.max(0, begin - LEAD_IN_MS));
          }
        }
      } else {
        handled = false;
      }
      if (handled) {
        // Ahead of the app's own shortcuts, which bail out on a handled
        // key: Space is play / pause there, and it stamps here.
        e.preventDefault();
        e.stopPropagation();
      }
    };
    // Capture: the global shortcuts listen on `window` too, registered
    // long before this.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [active, edit, estimate, seek, setCursor, togglePlayback]);

  const progress = syncProgress(doc);

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-x-5 gap-y-3">
        <div
          role="radiogroup"
          aria-label={t("lyricsEditor.sync.modeLabel")}
          className="flex rounded-full bg-zinc-100 dark:bg-zinc-800 p-0.5 text-xs"
        >
          {(["line", "word"] as const).map((m) => (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={mode === m}
              onMouseDown={keepFocus}
              onClick={() => setMode(m)}
              className={`px-3 py-1 rounded-full font-medium transition-colors ${
                mode === m
                  ? "bg-pink-500 text-white"
                  : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200"
              }`}
            >
              {t(`lyricsEditor.sync.mode.${m}`)}
            </button>
          ))}
        </div>
        <label
          className="flex items-center gap-2 text-xs text-zinc-600 dark:text-zinc-300"
          title={t("lyricsEditor.sync.reactionHint")}
        >
          {t("lyricsEditor.sync.reaction")}
          <input
            type="number"
            className="w-20 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 px-2 py-1 text-right tabular-nums focus:outline-none focus:ring-2 focus:ring-pink-500"
            value={reactionMs}
            min={0}
            max={MAX_REACTION_MS}
            step={10}
            onChange={(e) => setReactionMs(Number(e.target.value))}
            onKeyDown={(e) => {
              // Back to the keys that stamp once the value is typed.
              if (e.key === "Enter") e.currentTarget.blur();
            }}
          />
          ms
        </label>
        <div
          className="flex items-center gap-1 text-xs text-zinc-600 dark:text-zinc-300"
          title={t("lyricsEditor.sync.shiftHint")}
        >
          {t("lyricsEditor.sync.shift")}
          <button
            type="button"
            className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-800"
            aria-label={t("lyricsEditor.sync.shiftEarlier", { ms: SHIFT_MS })}
            title={t("lyricsEditor.sync.shiftEarlier", { ms: SHIFT_MS })}
            onMouseDown={keepFocus}
            onClick={() => edit((d) => shiftDocument(d, -SHIFT_MS))}
          >
            <Minus size={12} />
          </button>
          <button
            type="button"
            className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-800"
            aria-label={t("lyricsEditor.sync.shiftLater", { ms: SHIFT_MS })}
            title={t("lyricsEditor.sync.shiftLater", { ms: SHIFT_MS })}
            onMouseDown={keepFocus}
            onClick={() => edit((d) => shiftDocument(d, SHIFT_MS))}
          >
            <Plus size={12} />
          </button>
        </div>
        <span className="ml-auto text-xs text-zinc-500 dark:text-zinc-400 tabular-nums">
          {mode === "line"
            ? t("lyricsEditor.sync.progressLines", {
                done: progress.lines,
                total: progress.totalLines,
              })
            : t("lyricsEditor.sync.progressWords", {
                done: progress.units,
                total: progress.totalUnits,
              })}
        </span>
      </div>
      <Keys mode={mode} />
      {doc.lines.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400">
          {t("lyricsEditor.sync.empty")}
        </p>
      ) : (
        <SyncList
          doc={doc}
          edit={edit}
          mode={mode}
          cursor={cursor}
          setCursor={setCursor}
          seek={seek}
        />
      )}
    </div>
  );
}

function Keys({ mode }: { mode: SyncMode }) {
  const { t } = useTranslation();
  const keys: [string, string][] = [
    [
      t("lyricsEditor.sync.keyNames.space"),
      t(`lyricsEditor.sync.keys.space.${mode}`),
    ],
    [
      t("lyricsEditor.sync.keyNames.enter"),
      t(`lyricsEditor.sync.keys.enter.${mode}`),
    ],
    ["⌫", t("lyricsEditor.sync.keys.backspace")],
    ["P", t("lyricsEditor.sync.keys.play")],
    ["← →", t("lyricsEditor.sync.keys.seek")],
    ["↑ ↓", t("lyricsEditor.sync.keys.line")],
  ];
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-zinc-500 dark:text-zinc-400">
      {keys.map(([key, what]) => (
        <span key={key}>
          <kbd className="px-1.5 py-0.5 mr-1 rounded border border-zinc-300 dark:border-zinc-600 bg-zinc-50 dark:bg-zinc-800 font-mono text-[10px] text-zinc-700 dark:text-zinc-200">
            {key}
          </kbd>
          {what}
        </span>
      ))}
    </div>
  );
}

function SyncList({
  doc,
  edit,
  mode,
  cursor,
  setCursor,
  seek,
}: {
  doc: LyricsDocument;
  edit: EditDocument;
  mode: SyncMode;
  cursor: SyncCursor;
  setCursor: (cursor: SyncCursor) => void;
  seek: (ms: number) => void;
}) {
  const { t } = useTranslation();
  // The highlight follows the position events: four a second is plenty
  // to see which line is playing, and the stamps never read it.
  const positionMs = usePlayerPosition();
  const playing = activeLineIndex(doc, positionMs);
  const { parentRef, virtualizer, scrollMargin } = usePageVirtualizer({
    count: doc.lines.length,
    estimateSize: () => (mode === "word" ? 52 : 40),
    overscan: 10,
  });

  // Keep the line being stamped in view, a little above the middle so the
  // ones coming up can be read ahead.
  useEffect(() => {
    if (cursor.line < doc.lines.length) {
      virtualizer.scrollToIndex(cursor.line, { align: "center" });
    }
  }, [cursor.line, doc.lines.length, virtualizer]);

  return (
    <div
      ref={parentRef}
      data-sync-list
      className="relative"
      style={{ height: virtualizer.getTotalSize() }}
    >
      {virtualizer.getVirtualItems().map((row) => {
        const i = row.index;
        const line = doc.lines[i];
        const begin = lineBegin(line);
        const isCursor = i === cursor.line;
        return (
          <div
            key={row.key}
            data-index={i}
            ref={virtualizer.measureElement}
            className="absolute inset-x-0 top-0 pb-1"
            style={{ transform: `translateY(${row.start - scrollMargin}px)` }}
          >
            <div
              className={`flex items-center gap-3 rounded-lg px-2 py-1.5 cursor-pointer ${
                isCursor
                  ? "bg-pink-50 dark:bg-pink-950/30 ring-1 ring-pink-200 dark:ring-pink-900"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
              }`}
              onClick={() => setCursor({ line: i, word: 0 })}
            >
              <span
                className={`w-7 text-right font-mono text-xs tabular-nums ${
                  i === playing
                    ? "text-pink-500 font-semibold"
                    : "text-zinc-400"
                }`}
              >
                {i + 1}
              </span>
              <div className="flex items-center gap-0.5 shrink-0">
                <button
                  type="button"
                  className="p-1 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400"
                  aria-label={t("lyricsEditor.sync.nudgeEarlier", {
                    ms: NUDGE_MS,
                  })}
                  title={t("lyricsEditor.sync.nudgeEarlier", { ms: NUDGE_MS })}
                  onClick={(e) => {
                    e.stopPropagation();
                    edit((d) => nudge(d, i, null, -NUDGE_MS), `nudge-${i}`);
                  }}
                >
                  <Minus size={11} />
                </button>
                <button
                  type="button"
                  className={`w-[4.75rem] font-mono text-xs tabular-nums ${
                    begin === undefined
                      ? "text-zinc-400"
                      : "text-zinc-700 dark:text-zinc-200 hover:text-pink-500"
                  }`}
                  title={t("lyricsEditor.sync.playFromHere")}
                  disabled={begin === undefined}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (begin !== undefined) seek(begin);
                  }}
                >
                  {formatEditorTime(begin)}
                </button>
                <button
                  type="button"
                  className="p-1 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400"
                  aria-label={t("lyricsEditor.sync.nudgeLater", {
                    ms: NUDGE_MS,
                  })}
                  title={t("lyricsEditor.sync.nudgeLater", { ms: NUDGE_MS })}
                  onClick={(e) => {
                    e.stopPropagation();
                    edit((d) => nudge(d, i, null, NUDGE_MS), `nudge-${i}`);
                  }}
                >
                  <Plus size={11} />
                </button>
              </div>
              {mode === "line" ? (
                <span
                  className={`flex-1 text-base ${
                    i === playing
                      ? "text-zinc-900 dark:text-white font-medium"
                      : begin === undefined
                        ? "text-zinc-400"
                        : "text-zinc-600 dark:text-zinc-300"
                  }`}
                >
                  {lineText(line) || (
                    <em className="text-zinc-400">
                      {t("lyricsEditor.sync.emptyLine")}
                    </em>
                  )}
                </span>
              ) : (
                <span className="flex flex-1 flex-wrap items-center gap-1">
                  {line.words.map((unit, j) => {
                    const next = isCursor && j === cursor.word;
                    const done = unit.beginMs !== undefined;
                    return (
                      <button
                        key={j}
                        type="button"
                        className={`px-1.5 py-0.5 rounded text-sm transition-colors ${
                          unit.joined ? "-mr-1 rounded-r-none" : ""
                        } ${
                          next
                            ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-200 ring-1 ring-emerald-400"
                            : done
                              ? "bg-pink-100 dark:bg-pink-900/40 text-pink-700 dark:text-pink-200"
                              : "bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400"
                        }`}
                        title={
                          done
                            ? formatEditorTime(unit.beginMs)
                            : t("lyricsEditor.sync.notTimed")
                        }
                        onClick={(e) => {
                          e.stopPropagation();
                          setCursor({ line: i, word: j });
                        }}
                      >
                        {unit.text}
                      </button>
                    );
                  })}
                </span>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
