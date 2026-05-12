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
} from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  formatLrcTimestamp,
  parseLrc,
  saveLyrics,
  serializeLrc,
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
}

type Mode = "plain" | "synced";

interface SyncedRow {
  /** Stable id so React keys survive reorders. */
  id: number;
  /** -1 when not yet captured. */
  timeMs: number;
  text: string;
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
}: LyricsEditorModalProps) {
  const { t } = useTranslation();
  const { isPlaying, togglePlayback, seek, positionMs } = usePlayer();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  const [mode, setMode] = useState<Mode>("plain");
  const [plainText, setPlainText] = useState("");
  const [syncedRows, setSyncedRows] = useState<SyncedRow[]>([]);
  const [activeRow, setActiveRow] = useState(0);
  const [writeToFile, setWriteToFile] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Global timestamp shift applied to every captured row at save
  // time. Stays "preview" until Save (we don't mutate `syncedRows`
  // on every drag) so the user can dial it in without losing the
  // original capture if they reset.
  const [globalOffsetMs, setGlobalOffsetMs] = useState(0);

  const nextIdRef = useRef(1);
  const newRow = (timeMs: number, text: string): SyncedRow => ({
    id: nextIdRef.current++,
    timeMs,
    text,
  });

  // ── Hydrate from initial payload ─────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    /* eslint-disable react-hooks/set-state-in-effect */
    setError(null);
    setActiveRow(0);
    setGlobalOffsetMs(0);
    nextIdRef.current = 1;

    if (initial == null) {
      setMode("plain");
      setPlainText("");
      setSyncedRows([newRow(-1, "")]);
      return;
    }

    const trimmed = initial.content.trim();
    const isLrc = initial.format === "lrc" || initial.format === "enhanced_lrc";

    setPlainText(trimmed);
    if (isLrc) {
      const parsed = parseLrc(trimmed);
      const rows = parsed.length
        ? parsed.map((line) => newRow(line.timeMs, line.text))
        : [newRow(-1, "")];
      setSyncedRows(rows);
      setMode("synced");
    } else {
      // Pre-fill the synced tab with a row per non-empty line so the
      // user can capture timestamps without retyping.
      const lines = trimmed.length
        ? trimmed.split(/\r?\n/).map((l) => newRow(-1, l))
        : [newRow(-1, "")];
      setSyncedRows(lines);
      setMode("plain");
    }
    /* eslint-enable react-hooks/set-state-in-effect */
    // We intentionally only rehydrate when the modal opens for a track,
    // not on every initial change while open.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen, trackId]);

  // ── Capture handler shared between button + Space shortcut ───────
  const captureCurrent = useCallback(() => {
    setSyncedRows((rows) => {
      if (rows.length === 0) return rows;
      const idx = Math.min(activeRow, rows.length - 1);
      const next = rows.slice();
      next[idx] = { ...next[idx], timeMs: Math.max(0, positionMs) };
      // If there's no row after this one, append a fresh blank so the
      // user can keep typing the next line.
      if (idx === next.length - 1) {
        next.push(newRow(-1, ""));
      }
      return next;
    });
    setActiveRow((i) => i + 1);
  }, [activeRow, positionMs]);

  // ── Space-to-capture in synced mode (avoid hijacking inputs) ─────
  useEffect(() => {
    if (!isOpen || mode !== "synced") return;
    const handler = (e: KeyboardEvent) => {
      if (e.code !== "Space") return;
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase();
      if (tag === "input" || tag === "textarea") return;
      e.preventDefault();
      captureCurrent();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [isOpen, mode, captureCurrent]);

  // ── Player nudges (compose a ±2 s seek with current position) ────
  const nudge = (deltaMs: number) => {
    seek(Math.max(0, positionMs + deltaMs)).catch(() => {});
  };

  // ── Row-level helpers ────────────────────────────────────────────
  const updateRowText = (id: number, text: string) => {
    setSyncedRows((rows) =>
      rows.map((r) => (r.id === id ? { ...r, text } : r)),
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

  // ── Save ─────────────────────────────────────────────────────────
  const handleSave = async () => {
    if (trackId == null) return;
    setIsSaving(true);
    setError(null);
    try {
      const isSyncedMode = mode === "synced";
      const content = isSyncedMode
        ? serializeLrc(
            syncedRows
              .filter((r) => r.text.trim().length > 0 || r.timeMs >= 0)
              // Bake the previewed global offset into every captured
              // timestamp on save. Negative results are clamped to 0
              // so a user who shifts past the start of the track
              // doesn't end up with invalid LRC entries.
              .map((r) =>
                r.timeMs >= 0
                  ? { ...r, timeMs: Math.max(0, r.timeMs + globalOffsetMs) }
                  : r,
              )
              .sort((a, b) => {
                if (a.timeMs < 0 && b.timeMs < 0) return 0;
                if (a.timeMs < 0) return 1;
                if (b.timeMs < 0) return -1;
                return a.timeMs - b.timeMs;
              }),
          )
        : plainText.trim();

      // The backend pauses playback if we're editing the currently
      // playing file, so the flag is passed through as-is.
      const next = await saveLyrics(trackId, {
        content,
        format: isSyncedMode ? "lrc" : "plain",
        write_to_file: writeToFile,
      });
      onSaved(next);
      onClose();
    } catch (err) {
      console.error("[LyricsEditor] save failed", err);
      setError(String(err));
    } finally {
      setIsSaving(false);
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

  if (!isOpen) return null;

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="lyrics-editor-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-3xl max-h-[90vh] flex flex-col overflow-hidden animate-fade-in"
        onClick={(e) => e.stopPropagation()}
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

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-6 min-h-75">
          {mode === "plain" ? (
            <textarea
              value={plainText}
              onChange={(e) => setPlainText(e.target.value)}
              placeholder={t("lyricsEditor.plainPlaceholder")}
              className="w-full h-[50vh] resize-none rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 p-4 text-sm leading-relaxed focus:outline-none focus:ring-2 focus:ring-pink-500"
            />
          ) : (
            <SyncedEditor
              rows={syncedRows}
              activeRow={activeRow}
              playingRow={playingRowIdx}
              offsetMs={globalOffsetMs}
              onActivate={setActiveRow}
              onUpdateText={updateRowText}
              onRemove={removeRow}
              onInsertBelow={insertRowBelow}
              onSeekTo={seekToRow}
              onRecapture={recapture}
            />
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
              {t("lyricsEditor.captureHint")} · {captured}/{syncedRows.length}{" "}
              {t("lyricsEditor.lines")}
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
      </div>
    </div>
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
        return (
          <li
            key={row.id}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-lg transition-colors ${
              isActive
                ? "bg-pink-50 dark:bg-pink-950/30 ring-1 ring-pink-200 dark:ring-pink-900"
                : isPlaying
                  ? "bg-emerald-50/60 dark:bg-emerald-950/20"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/50"
            }`}
            onFocus={() => onActivate(idx)}
          >
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
          </li>
        );
      })}
    </ul>
  );
}
