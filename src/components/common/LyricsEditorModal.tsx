import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  X,
  Play,
  Pause,
  SkipBack,
  SkipForward,
  Crosshair,
  Trash2,
  Plus,
  Loader2,
  Save,
  Minus,
  RotateCcw,
  FileDown,
} from "lucide-react";
import { save as showSaveDialog } from "@tauri-apps/plugin-dialog";
import { usePlayer } from "../../hooks/usePlayer";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  exportLyricsToPath,
  formatLrcTimestamp,
  parseLrc,
  parseLyrics,
  saveLyrics,
  serializeEnhancedLrc,
  serializeLrc,
  type LyricsLine,
  type LyricsPayload,
} from "../../lib/tauri/lyrics";

interface LyricsEditorModalProps {
  isOpen: boolean;
  onClose: () => void;
  trackId: number | null;
  trackTitle: string | null;
  /** Initial payload — used to pre-fill the editor on open. */
  initial: LyricsPayload | null;
  /** Called with the freshly-saved payload so the panel can update. */
  onSaved: (next: LyricsPayload) => void;
  /**
   * Audio file path of the current track. Used as the default
   * location + filename stem for the "Save to file…" affordance
   * (issue #201). Optional — when absent the dialog opens at the
   * user's last-used directory with no suggested filename.
   */
  trackFilePath?: string | null;
}

type Mode = "plain" | "synced";
/** Capture granularity inside the synced tab. */
type Granularity = "line" | "word";

interface SyncedWord {
  /** -1 when not yet captured. */
  timeMs: number;
  /** Word text, kept verbatim including any trailing spaces. */
  text: string;
}

interface SyncedRow {
  /** Stable id so React keys survive reorders. */
  id: number;
  /** -1 when not yet captured. */
  timeMs: number;
  text: string;
  /**
   * Populated in word-mode once the user starts capturing per-word
   * stamps for the row. Absent in line-mode and for plain rows.
   */
  words?: SyncedWord[];
  /** Cursor inside `words` — index of the next word to capture. */
  wordCursor?: number;
}

/**
 * Two-mode lyrics editor — plain textarea on one tab, Musicolet-style
 * line-by-line capture on the other. The synced tab pilots the
 * existing player (play / pause / ±2 s) so the user can match each
 * line to the playback head with a single keystroke.
 */
export function LyricsEditorModal({
  isOpen,
  onClose,
  trackId,
  trackTitle,
  initial,
  onSaved,
  trackFilePath,
}: LyricsEditorModalProps) {
  const { t } = useTranslation();
  const { isPlaying, togglePlayback, seek, positionMs } = usePlayer();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  const [mode, setMode] = useState<Mode>("plain");
  const [granularity, setGranularity] = useState<Granularity>("line");
  const [plainText, setPlainText] = useState("");
  const [syncedRows, setSyncedRows] = useState<SyncedRow[]>([]);
  const [activeRow, setActiveRow] = useState(0);
  const [writeToFile, setWriteToFile] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Surfaced after save when the backend kept the lyrics in-DB but
   *  couldn't write them to the audio file's tag (e.g. TTML on MP3). */
  const [warning, setWarning] = useState<string | null>(null);
  // Global timestamp shift applied to every captured row at save
  // time. Stays "preview" until Save (we don't mutate `syncedRows`
  // on every drag) so the user can dial it in without losing the
  // original capture if they reset.
  const [globalOffsetMs, setGlobalOffsetMs] = useState(0);

  const nextIdRef = useRef(1);
  const newRow = (
    timeMs: number,
    text: string,
    words?: SyncedWord[],
  ): SyncedRow => ({
    id: nextIdRef.current++,
    timeMs,
    text,
    words,
    wordCursor: words ? 0 : undefined,
  });

  /** Split a line into tokens that preserve trailing spaces, so the
   *  reassembled text still reads naturally. Empty tokens are dropped. */
  const tokenize = (text: string): SyncedWord[] => {
    if (!text.trim()) return [];
    const re = /\S+\s*/g;
    const out: SyncedWord[] = [];
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      out.push({ timeMs: -1, text: m[0] });
    }
    return out;
  };

  // ── Hydrate from initial payload ─────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    /* eslint-disable react-hooks/set-state-in-effect */
    setError(null);
    setWarning(null);
    setActiveRow(0);
    setGlobalOffsetMs(0);
    nextIdRef.current = 1;

    if (initial == null) {
      setMode("plain");
      setGranularity("line");
      setPlainText("");
      setSyncedRows([newRow(-1, "")]);
      return;
    }

    const trimmed = initial.content.trim();
    const isSynced =
      initial.format === "lrc" ||
      initial.format === "enhanced_lrc" ||
      initial.format === "ttml";
    const hasWordTiming =
      initial.format === "enhanced_lrc" || initial.format === "ttml";

    setPlainText(trimmed);
    if (isSynced) {
      let parsed: LyricsLine[];
      if (hasWordTiming) {
        parsed = parseLyrics(trimmed, initial.format);
      } else {
        parsed = parseLrc(trimmed);
      }
      const rows = parsed.length
        ? parsed.map((line) => {
            const words = line.words?.map((w) => ({
              timeMs: w.timeMs,
              text: w.text,
            }));
            const cursor = words
              ? Math.min(
                  words.length,
                  words.findIndex((w) => w.timeMs < 0),
                )
              : undefined;
            return {
              id: nextIdRef.current++,
              timeMs: line.timeMs,
              text: line.text,
              words,
              wordCursor: cursor != null && cursor < 0 ? words!.length : cursor,
            } satisfies SyncedRow;
          })
        : [newRow(-1, "")];
      setSyncedRows(rows);
      setMode("synced");
      setGranularity(hasWordTiming ? "word" : "line");
    } else {
      // Pre-fill the synced tab with a row per non-empty line so the
      // user can capture timestamps without retyping.
      const lines = trimmed.length
        ? trimmed.split(/\r?\n/).map((l) => newRow(-1, l))
        : [newRow(-1, "")];
      setSyncedRows(lines);
      setMode("plain");
      setGranularity("line");
    }
    /* eslint-enable react-hooks/set-state-in-effect */
    // We intentionally only rehydrate when the modal opens for a track,
    // not on every initial change while open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, trackId]);

  // ── Capture handlers ─────────────────────────────────────────────
  // Line mode: stamp the active row, append a fresh row if needed,
  // advance the cursor.
  const captureLine = useCallback(() => {
    setSyncedRows((rows) => {
      if (rows.length === 0) return rows;
      const idx = Math.min(activeRow, rows.length - 1);
      const next = rows.slice();
      next[idx] = { ...next[idx], timeMs: Math.max(0, positionMs) };
      if (idx === next.length - 1) {
        next.push(newRow(-1, ""));
      }
      return next;
    });
    setActiveRow((i) => i + 1);
  }, [activeRow, positionMs]);

  // Word mode: stamp the next word in the active row. If the row has
  // no `words` yet, tokenize its text first. Once every word is
  // stamped, the next press advances to the next line (and stamps the
  // line's own timeMs if it's still -1, like line mode).
  const captureWord = useCallback(() => {
    setSyncedRows((rows) => {
      if (rows.length === 0) return rows;
      const idx = Math.min(activeRow, rows.length - 1);
      const next = rows.slice();
      const row = { ...next[idx] };

      // Seed words from row.text on first capture.
      let words = row.words ? row.words.slice() : tokenize(row.text);
      if (words.length === 0) {
        // Empty line — degrade to line capture so we don't get stuck.
        row.timeMs = Math.max(0, positionMs);
        next[idx] = row;
        return next;
      }

      const cursor = row.wordCursor ?? 0;
      if (cursor >= words.length) {
        // Out of words on this row — let the caller advance lines.
        return rows;
      }
      // Stamp the line's timeMs on the very first word capture if the
      // line itself isn't stamped yet.
      if (row.timeMs < 0 && cursor === 0) {
        row.timeMs = Math.max(0, positionMs);
      }
      words = words.slice();
      words[cursor] = { ...words[cursor], timeMs: Math.max(0, positionMs) };
      row.words = words;
      row.wordCursor = cursor + 1;
      next[idx] = row;
      return next;
    });
  }, [activeRow, positionMs]);

  // Advance to the next line in word mode (Enter shortcut). Appends a
  // fresh empty row if we're at the end, mirroring line mode's UX.
  const advanceLine = useCallback(() => {
    setSyncedRows((rows) => {
      if (rows.length === 0) return rows;
      const idx = Math.min(activeRow, rows.length - 1);
      if (idx === rows.length - 1) {
        return [...rows, newRow(-1, "")];
      }
      return rows;
    });
    setActiveRow((i) => i + 1);
  }, [activeRow]);

  // Undo the last word capture on the active row (Backspace in word
  // mode). If no words are stamped yet, clears the line's own timeMs.
  const undoLastWord = useCallback(() => {
    setSyncedRows((rows) => {
      if (rows.length === 0) return rows;
      const idx = Math.min(activeRow, rows.length - 1);
      const row = { ...rows[idx] };
      if (!row.words || row.words.length === 0) {
        if (row.timeMs >= 0) {
          row.timeMs = -1;
          const next = rows.slice();
          next[idx] = row;
          return next;
        }
        return rows;
      }
      const cursor = Math.max(0, (row.wordCursor ?? 0) - 1);
      const words = row.words.slice();
      if (words[cursor]) {
        words[cursor] = { ...words[cursor], timeMs: -1 };
      }
      row.words = words;
      row.wordCursor = cursor;
      // If we backed all the way out, clear the line stamp too.
      if (cursor === 0 && words.every((w) => w.timeMs < 0)) {
        row.timeMs = -1;
      }
      const next = rows.slice();
      next[idx] = row;
      return next;
    });
  }, [activeRow]);

  // Single entry point used by the capture button + Space shortcut.
  const captureCurrent = useCallback(() => {
    if (granularity === "word") {
      captureWord();
    } else {
      captureLine();
    }
  }, [granularity, captureWord, captureLine]);

  // ── Keyboard shortcuts in synced mode (avoid hijacking inputs) ───
  useEffect(() => {
    if (!isOpen || mode !== "synced") return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase();
      const isInput = tag === "input" || tag === "textarea";
      if (e.code === "Space" && !isInput) {
        e.preventDefault();
        captureCurrent();
        return;
      }
      if (
        granularity === "word" &&
        !isInput &&
        (e.code === "Enter" || e.code === "NumpadEnter")
      ) {
        e.preventDefault();
        advanceLine();
        return;
      }
      if (
        granularity === "word" &&
        !isInput &&
        (e.code === "Backspace" || e.code === "Delete")
      ) {
        e.preventDefault();
        undoLastWord();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [isOpen, mode, granularity, captureCurrent, advanceLine, undoLastWord]);

  // ── Player nudges (compose a ±2 s seek with current position) ────
  const nudge = (deltaMs: number) => {
    seek(Math.max(0, positionMs + deltaMs)).catch(() => {});
  };

  // ── Row-level helpers ────────────────────────────────────────────
  const updateRowText = (id: number, text: string) => {
    setSyncedRows((rows) =>
      rows.map((r) => {
        if (r.id !== id) return r;
        // In word mode editing the text invalidates the captured word
        // stamps (tokenization changes). Drop them so the user can
        // re-capture cleanly — keep the line-level timeMs.
        if (r.words) {
          return { ...r, text, words: undefined, wordCursor: undefined };
        }
        return { ...r, text };
      }),
    );
  };
  const removeRow = (id: number) => {
    setSyncedRows((rows) => {
      const next = rows.filter((r) => r.id !== id);
      return next.length ? next : [newRow(-1, "")];
    });
  };
  const insertRowBelow = (id: number) => {
    setSyncedRows((rows) => {
      const idx = rows.findIndex((r) => r.id === id);
      if (idx < 0) return rows;
      const next = rows.slice();
      next.splice(idx + 1, 0, newRow(-1, ""));
      return next;
    });
  };
  const seekToRow = (row: SyncedRow) => {
    if (row.timeMs >= 0) {
      // Honour the in-flight global offset so seeking a previewed
      // timestamp lands on the position the user can actually hear.
      seek(Math.max(0, row.timeMs + globalOffsetMs)).catch(() => {});
    }
  };
  const recapture = (id: number) => {
    setSyncedRows((rows) =>
      rows.map((r) =>
        r.id === id ? { ...r, timeMs: Math.max(0, positionMs) } : r,
      ),
    );
  };

  // Serialize the current editor state (plain text or synced rows) to
  // the same `{ content, format }` shape both the in-app save path
  // (DB cache + optional tag write) and the "Save to file…" path
  // consume. Extracted from `handleSave` so the export-to-disk
  // affordance (issue #201) doesn't have to re-implement the
  // word/line/plain dispatch.
  const buildPayload = (): { content: string; format: "plain" | "lrc" | "enhanced_lrc" } => {
    const isSyncedMode = mode === "synced";
    const isWordMode = isSyncedMode && granularity === "word";

    // Bake the previewed global offset into every captured stamp on
    // save (both line- and word-level). Negative results are clamped
    // to 0 so a user shifting past the start of the track doesn't
    // emit invalid stamps.
    const shift = (ts: number): number =>
      ts < 0 ? -1 : Math.max(0, ts + globalOffsetMs);

    if (!isSyncedMode) {
      return { content: plainText.trim(), format: "plain" };
    }
    if (isWordMode) {
      // Keep every row the user typed text into, even if they
      // haven't captured a stamp yet — line mode does the same,
      // so saving in word mode shouldn't silently delete unstamped
      // text. `serializeEnhancedLrc` emits `[--:--.--]` for rows
      // with `timeMs < 0` and folds uncaptured words into the
      // previous segment (no phantom `<00:00.00>` stamp), so half-
      // finished work round-trips cleanly through save → reload.
      const rowsForSave: LyricsLine[] = syncedRows
        .filter(
          (r) =>
            r.text.trim().length > 0 ||
            r.timeMs >= 0 ||
            (r.words?.some((w) => w.timeMs >= 0) ?? false),
        )
        .map((r) => ({
          timeMs: shift(r.timeMs),
          endMs: -1,
          text: r.text,
          words: r.words?.map((w) => ({
            timeMs: shift(w.timeMs),
            endMs: -1,
            text: w.text,
          })),
        }))
        // Untimed rows (timeMs < 0) sort to the end so the synced
        // body stays monotonically ordered; the user can resume
        // capturing them on the next edit.
        .sort((a, b) => {
          if (a.timeMs < 0 && b.timeMs < 0) return 0;
          if (a.timeMs < 0) return 1;
          if (b.timeMs < 0) return -1;
          return a.timeMs - b.timeMs;
        });
      return {
        content: serializeEnhancedLrc(rowsForSave),
        format: "enhanced_lrc",
      };
    }
    return {
      content: serializeLrc(
        syncedRows
          .filter((r) => r.text.trim().length > 0 || r.timeMs >= 0)
          .map((r) => (r.timeMs >= 0 ? { ...r, timeMs: shift(r.timeMs) } : r))
          .sort((a, b) => {
            if (a.timeMs < 0 && b.timeMs < 0) return 0;
            if (a.timeMs < 0) return 1;
            if (b.timeMs < 0) return -1;
            return a.timeMs - b.timeMs;
          }),
      ),
      format: "lrc",
    };
  };

  // ── Save ─────────────────────────────────────────────────────────
  const handleSave = async () => {
    if (trackId == null) return;
    setIsSaving(true);
    setError(null);
    try {
      const { content, format: saveFormat } = buildPayload();

      // The backend pauses playback if we're editing the currently
      // playing file, so the flag is passed through as-is.
      const next = await saveLyrics(trackId, {
        content,
        format: saveFormat,
        write_to_file: writeToFile,
      });
      onSaved(next);
      if (next.tag_write_skipped) {
        // Keep the modal open with a warning so the user knows the
        // file itself wasn't touched — DB cache still updated.
        setWarning(t("lyrics.toast.tagWriteSkipped"));
      } else {
        onClose();
      }
    } catch (err) {
      console.error("[LyricsEditor] save failed", err);
      setError(String(err));
    } finally {
      setIsSaving(false);
    }
  };

  // ── Export to standalone file (issue #201) ────────────────────────
  const handleExportToFile = async () => {
    setError(null);
    setWarning(null);
    try {
      const { content, format: saveFormat } = buildPayload();
      // Default to `.lrc` for synced output, `.txt` for plain — the
      // user can switch via the dialog's format dropdown either way.
      const defaultExt = saveFormat === "plain" ? "txt" : "lrc";
      const stem = filenameStem(trackFilePath, trackTitle);
      const defaultPath = defaultExportPath(trackFilePath, stem, defaultExt);
      const target = await showSaveDialog({
        title: t("lyricsEditor.exportToFile") ?? undefined,
        defaultPath: defaultPath ?? undefined,
        filters: [
          {
            name: defaultExt === "lrc" ? "Synced lyrics (.lrc)" : "Plain lyrics (.txt)",
            extensions: [defaultExt],
          },
          {
            name: defaultExt === "lrc" ? "Plain lyrics (.txt)" : "Synced lyrics (.lrc)",
            extensions: [defaultExt === "lrc" ? "txt" : "lrc"],
          },
        ],
      });
      if (!target) return;
      await exportLyricsToPath(target, content);
      setWarning(t("lyricsEditor.exportedToFile", { path: target }));
    } catch (err) {
      console.error("[LyricsEditor] export failed", err);
      setError(String(err));
    }
  };

  // ── Stats for the footer ─────────────────────────────────────────
  const captured = useMemo(
    () => syncedRows.filter((r) => r.timeMs >= 0).length,
    [syncedRows],
  );

  // Index of the row whose effective timestamp (raw + global offset)
  // is the largest one ≤ current playback position. Drives a subtle
  // "now playing" dot so the user can see whether their offset is
  // dialled in correctly while music plays.
  const playingRowIdx = useMemo(() => {
    let best = -1;
    let bestTs = -1;
    for (let i = 0; i < syncedRows.length; i += 1) {
      const ts = syncedRows[i].timeMs;
      if (ts < 0) continue;
      const effective = ts + globalOffsetMs;
      if (effective <= positionMs && effective > bestTs) {
        best = i;
        bestTs = effective;
      }
    }
    return best;
  }, [syncedRows, globalOffsetMs, positionMs]);

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="lyrics-editor-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-3xl max-h-[calc(100vh-2rem)] flex flex-col overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="min-w-0">
            <h2
              id="lyrics-editor-title"
              className="text-lg font-semibold truncate"
            >
              {t("lyricsEditor.title")}
            </h2>
            {trackTitle && (
              <p className="text-xs text-zinc-500 dark:text-zinc-400 truncate mt-0.5">
                {trackTitle}
              </p>
            )}
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex border-b border-zinc-200 dark:border-zinc-800">
          <TabButton
            active={mode === "plain"}
            onClick={() => setMode("plain")}
            label={t("lyricsEditor.tabs.plain")}
          />
          <TabButton
            active={mode === "synced"}
            onClick={() => setMode("synced")}
            label={t("lyricsEditor.tabs.synced")}
          />
        </div>

        {/* Body — flex-1 so it absorbs the leftover height between the
            header/tabs and the footer (and the synced controls when
            visible) without ever pushing the action bar off-screen.
            Plain-mode textarea fills the body via `h-full`; in synced
            mode the row list scrolls inside this same scroll container.
            See #172 — a fixed `h-[50vh]` here hid the footer on 1080p
            + Windows display scaling. */}
        <div className="flex-1 min-h-0 overflow-y-auto p-6">
          {mode === "plain" ? (
            <textarea
              value={plainText}
              onChange={(e) => setPlainText(e.target.value)}
              placeholder={t("lyricsEditor.plainPlaceholder")}
              className="w-full h-full min-h-48 resize-none rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 p-4 text-sm leading-relaxed focus:outline-none focus:ring-2 focus:ring-pink-500"
            />
          ) : (
            <>
              {/* Granularity toggle. Sits above the row list so users
                  can flip between line + word capture without losing
                  what they've already stamped. */}
              <div className="flex items-center gap-2 mb-3">
                <span className="text-xs text-zinc-500 dark:text-zinc-400 mr-1">
                  {t("lyricsEditor.granularity.label")}
                </span>
                <button
                  type="button"
                  onClick={() => setGranularity("line")}
                  className={`px-3 py-1 rounded-full text-xs font-medium transition-colors ${
                    granularity === "line"
                      ? "bg-pink-500 text-white"
                      : "bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                  }`}
                >
                  {t("lyricsEditor.granularity.line")}
                </button>
                <button
                  type="button"
                  onClick={() => setGranularity("word")}
                  className={`px-3 py-1 rounded-full text-xs font-medium transition-colors ${
                    granularity === "word"
                      ? "bg-pink-500 text-white"
                      : "bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                  }`}
                >
                  {t("lyricsEditor.granularity.word")}
                </button>
              </div>
              <SyncedEditor
                rows={syncedRows}
                activeRow={activeRow}
                playingRow={playingRowIdx}
                offsetMs={globalOffsetMs}
                granularity={granularity}
                onActivate={setActiveRow}
                onUpdateText={updateRowText}
                onRemove={removeRow}
                onInsertBelow={insertRowBelow}
                onSeekTo={seekToRow}
                onRecapture={recapture}
              />
            </>
          )}
        </div>

        {/* Synced controls */}
        {mode === "synced" && (
          <div className="px-6 py-3 border-t border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-950/40">
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={() => nudge(-2000)}
                title="-2s"
                className="p-2 rounded-full hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
              >
                <SkipBack size={16} />
              </button>
              <button
                type="button"
                onClick={() => togglePlayback()}
                title={isPlaying ? t("player.pause") : t("player.play")}
                className="p-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 hover:opacity-90 transition-opacity"
              >
                {isPlaying ? <Pause size={16} /> : <Play size={16} />}
              </button>
              <button
                type="button"
                onClick={() => nudge(2000)}
                title="+2s"
                className="p-2 rounded-full hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
              >
                <SkipForward size={16} />
              </button>
              <div className="text-xs font-mono text-zinc-500 dark:text-zinc-400 ml-2 w-24">
                {formatLrcTimestamp(positionMs)}
              </div>
              <div className="flex-1" />
              <button
                type="button"
                onClick={captureCurrent}
                className="px-4 py-2 rounded-full bg-pink-500 hover:bg-pink-600 text-white text-sm font-medium flex items-center gap-2 transition-colors"
                title={t("lyricsEditor.captureHint")}
              >
                <Crosshair size={14} />
                {t("lyricsEditor.capture")}
              </button>
            </div>
            <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-2">
              {granularity === "word"
                ? t("lyricsEditor.captureHintWord")
                : t("lyricsEditor.captureHint")}{" "}
              · {captured}/{syncedRows.length} {t("lyricsEditor.lines")}
            </p>

            {/* Global timestamp shift — applied to every captured row
                at save time. Useful when imported LRC files are
                consistently early/late, or when the user's reaction
                latency drifted every capture by the same amount. */}
            <div className="mt-3 flex items-center gap-2">
              <span
                className="text-xs font-medium text-zinc-600 dark:text-zinc-300 shrink-0"
                title={t("lyricsEditor.offset.help")}
              >
                {t("lyricsEditor.offset.label")}
              </span>
              <button
                type="button"
                onClick={() =>
                  setGlobalOffsetMs((v) => Math.max(-5000, v - 100))
                }
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
                title="-100 ms"
              >
                <Minus size={12} />
              </button>
              <input
                type="range"
                min={-5000}
                max={5000}
                step={50}
                value={globalOffsetMs}
                onChange={(e) =>
                  setGlobalOffsetMs(Number(e.currentTarget.value))
                }
                aria-label={t("lyricsEditor.offset.label")}
                className="flex-1 accent-pink-500"
              />
              <button
                type="button"
                onClick={() =>
                  setGlobalOffsetMs((v) => Math.min(5000, v + 100))
                }
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-800 transition-colors"
                title="+100 ms"
              >
                <Plus size={12} />
              </button>
              <span
                className={`font-mono text-xs w-16 text-right ${
                  globalOffsetMs === 0
                    ? "text-zinc-400 dark:text-zinc-500"
                    : "text-pink-500"
                }`}
              >
                {globalOffsetMs > 0 ? "+" : ""}
                {(globalOffsetMs / 1000).toFixed(2)} s
              </span>
              <button
                type="button"
                onClick={() => setGlobalOffsetMs(0)}
                disabled={globalOffsetMs === 0}
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-800 text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-100 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                title={t("lyricsEditor.offset.reset")}
              >
                <RotateCcw size={12} />
              </button>
            </div>
          </div>
        )}

        {/* Footer */}
        <div className="flex items-center justify-between gap-4 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <label className="flex items-center gap-2 text-sm text-zinc-600 dark:text-zinc-300 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={writeToFile}
              onChange={(e) => setWriteToFile(e.target.checked)}
              className="rounded"
            />
            {t("lyricsEditor.writeToFile")}
          </label>
          <div className="flex items-center gap-2">
            {warning && (
              <span className="text-xs text-amber-600 dark:text-amber-400 truncate max-w-xs">
                {warning}
              </span>
            )}
            {error && (
              <span className="text-xs text-red-500 truncate max-w-xs">
                {error}
              </span>
            )}
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              onClick={handleExportToFile}
              disabled={isSaving}
              className="px-4 py-2 rounded-full text-sm border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50 flex items-center gap-2"
              title={t("lyricsEditor.exportToFileHint") ?? undefined}
            >
              <FileDown size={14} />
              {t("lyricsEditor.exportToFile")}
            </button>
            <button
              type="button"
              onClick={handleSave}
              disabled={isSaving || trackId == null}
              className="px-5 py-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 text-sm font-medium hover:opacity-90 disabled:opacity-50 transition-opacity flex items-center gap-2"
            >
              {isSaving ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Save size={14} />
              )}
              {t("common.save")}
            </button>
          </div>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

function TabButton({
  active,
  onClick,
  label,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`px-5 py-3 text-sm font-medium transition-colors border-b-2 -mb-px ${
        active
          ? "border-pink-500 text-zinc-900 dark:text-white"
          : "border-transparent text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
      }`}
    >
      {label}
    </button>
  );
}

interface SyncedEditorProps {
  rows: SyncedRow[];
  activeRow: number;
  /** Index of the row currently sounding at playback position
   * (after the global offset preview). -1 = none. */
  playingRow: number;
  /** Global timestamp shift previewed in the timestamp buttons. */
  offsetMs: number;
  /** Capture granularity — drives the per-word chip row. */
  granularity: Granularity;
  onActivate: (idx: number) => void;
  onUpdateText: (id: number, text: string) => void;
  onRemove: (id: number) => void;
  onInsertBelow: (id: number) => void;
  onSeekTo: (row: SyncedRow) => void;
  onRecapture: (id: number) => void;
}

function SyncedEditor({
  rows,
  activeRow,
  playingRow,
  offsetMs,
  granularity,
  onActivate,
  onUpdateText,
  onRemove,
  onInsertBelow,
  onSeekTo,
  onRecapture,
}: SyncedEditorProps) {
  const { t } = useTranslation();
  return (
    <ul className="space-y-1.5">
      {rows.map((row, idx) => {
        const isActive = idx === activeRow;
        const isPlaying = idx === playingRow;
        const captured = row.timeMs >= 0;
        const shifted = captured && offsetMs !== 0;
        const previewMs = captured ? Math.max(0, row.timeMs + offsetMs) : -1;
        const showWordChips =
          granularity === "word" && isActive && (row.words?.length ?? 0) > 0;
        return (
          <li
            key={row.id}
            className={`flex flex-col gap-1 px-2 py-1.5 rounded-lg transition-colors ${
              isActive
                ? "bg-pink-50 dark:bg-pink-950/30 ring-1 ring-pink-200 dark:ring-pink-900"
                : isPlaying
                  ? "bg-emerald-50/60 dark:bg-emerald-950/20"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
            }`}
            onFocus={() => onActivate(idx)}
          >
            <div className="flex items-center gap-2">
              <span
                aria-hidden
                className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                  isPlaying ? "bg-emerald-500 animate-pulse" : "bg-transparent"
                }`}
              />
              <button
                type="button"
                onClick={() => (captured ? onSeekTo(row) : onRecapture(row.id))}
                title={
                  captured
                    ? t("lyricsEditor.seekToLine")
                    : t("lyricsEditor.captureNow")
                }
                className={`shrink-0 font-mono text-xs px-2 py-1 rounded w-20 text-center transition-colors ${
                  captured
                    ? shifted
                      ? "bg-pink-100 dark:bg-pink-900/40 text-pink-600 dark:text-pink-300 hover:bg-pink-200 dark:hover:bg-pink-900/60 italic"
                      : "bg-zinc-200 dark:bg-zinc-700 hover:bg-zinc-300 dark:hover:bg-zinc-600"
                    : "bg-zinc-100 dark:bg-zinc-800 text-zinc-400 dark:text-zinc-500 hover:text-pink-500"
                }`}
              >
                {captured ? formatLrcTimestamp(previewMs) : "--:--.--"}
              </button>
              <input
                type="text"
                value={row.text}
                onChange={(e) => onUpdateText(row.id, e.target.value)}
                onFocus={() => onActivate(idx)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    onInsertBelow(row.id);
                    // Defer so the new row exists before we activate it.
                    setTimeout(() => onActivate(idx + 1), 0);
                  }
                }}
                placeholder={t("lyricsEditor.linePlaceholder")}
                className="flex-1 bg-transparent text-sm focus:outline-none px-2 py-1"
              />
              <button
                type="button"
                onClick={() => onRecapture(row.id)}
                title={t("lyricsEditor.recapture")}
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400 hover:text-pink-500 transition-colors"
              >
                <Crosshair size={12} />
              </button>
              <button
                type="button"
                onClick={() => onInsertBelow(row.id)}
                title={t("lyricsEditor.insertBelow")}
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
              >
                <Plus size={12} />
              </button>
              <button
                type="button"
                onClick={() => onRemove(row.id)}
                title={t("lyricsEditor.removeLine")}
                className="p-1.5 rounded hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-400 hover:text-red-500 transition-colors"
              >
                <Trash2 size={12} />
              </button>
            </div>
            {showWordChips && (
              <div className="flex flex-wrap items-center gap-1 pl-22 pr-2 pb-1">
                {row.words!.map((w, wi) => {
                  const wCursor = row.wordCursor ?? 0;
                  const wCaptured = w.timeMs >= 0;
                  const isNext = wi === wCursor;
                  return (
                    <span
                      key={wi}
                      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[11px] font-mono transition-colors ${
                        wCaptured
                          ? "bg-pink-100 dark:bg-pink-900/40 text-pink-700 dark:text-pink-200"
                          : isNext
                            ? "bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-200 ring-1 ring-emerald-400"
                            : "bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400"
                      }`}
                      title={
                        wCaptured
                          ? formatLrcTimestamp(Math.max(0, w.timeMs + offsetMs))
                          : t("lyricsEditor.notCaptured")
                      }
                    >
                      <span>{w.text.trim() || "·"}</span>
                    </span>
                  );
                })}
              </div>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * Resolve the filename stem the "Save to file…" dialog should
 * suggest. Prefers the audio file's basename (so the sidecar lands
 * next to it with a matching name, the convention every offline
 * player understands), falls back to a sanitized track title, then
 * to the literal string "lyrics" so the dialog isn't blank.
 */
function filenameStem(
  filePath: string | null | undefined,
  trackTitle: string | null | undefined,
): string {
  if (filePath) {
    const normalized = filePath.replace(/\\/g, "/");
    const base = normalized.substring(normalized.lastIndexOf("/") + 1);
    const dot = base.lastIndexOf(".");
    const stem = dot > 0 ? base.substring(0, dot) : base;
    if (stem.length > 0) return stem;
  }
  if (trackTitle) {
    // Sanitize the title for filesystem use — strip the characters
    // every common OS rejects, collapse whitespace, trim. Falls back
    // to "lyrics" if the result is empty (e.g. an emoji-only title).
    const sanitized = trackTitle
      .replace(/[/\\:*?"<>|]/g, "")
      .replace(/\s+/g, " ")
      .trim();
    if (sanitized.length > 0) return sanitized;
  }
  return "lyrics";
}

/**
 * Build the full default `defaultPath` the Tauri save dialog should
 * open at. When `filePath` is known we anchor on its parent
 * directory so the user lands next to the song — the conventional
 * sidecar location. Returns `null` to let the dialog fall back to
 * its remembered last-used directory.
 */
function defaultExportPath(
  filePath: string | null | undefined,
  stem: string,
  ext: string,
): string | null {
  if (!filePath) return null;
  // Preserve OS-native separators so the dialog re-renders the path
  // correctly on Windows + macOS + Linux. Splitting on both is fine
  // because Windows accepts both `\` and `/` in API paths.
  const sepIdx = Math.max(filePath.lastIndexOf("/"), filePath.lastIndexOf("\\"));
  if (sepIdx < 0) return `${stem}.${ext}`;
  const sep = filePath[sepIdx];
  return `${filePath.substring(0, sepIdx)}${sep}${stem}.${ext}`;
}
