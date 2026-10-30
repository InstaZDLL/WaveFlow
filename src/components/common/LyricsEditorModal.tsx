import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { useTranslation } from "react-i18next";
import {
  FileDown,
  Loader2,
  Redo2,
  Save,
  Undo2,
  X,
  AlertTriangle,
} from "lucide-react";
import { save as showSaveDialog } from "@tauri-apps/plugin-dialog";
import { usePlayer } from "../../hooks/usePlayer";
import { useModalA11y } from "../../hooks/useModalA11y";
import { usePlaybackClock } from "../../hooks/usePlaybackClock";
import { useUndoHistory } from "../../hooks/useUndoHistory";
import { useLyricsEditorReaction } from "../../hooks/useLyricsEditorReaction";
import { PageScrollContext } from "../../contexts/PageScrollContext";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  exportLyricsToPath,
  getLyricsDefaultDestination,
  saveLyrics,
  type LyricsDestination,
  type LyricsFormat,
  type LyricsPayload,
} from "../../lib/tauri/lyrics";
import {
  EMPTY_LYRICS_DOCUMENT,
  readLyricsDocument,
  writeLyricsDocument,
  type LyricsDocument,
  type LyricsDocumentWrite,
} from "../../lib/tauri/lyricsDocument";
import {
  cleanedDocument,
  isWordSynced,
  lineBegin,
} from "../../lib/lyricsDocumentEdit";
import { EditStep } from "./lyricsEditor/EditStep";
import { SyncStep } from "./lyricsEditor/SyncStep";
import { PreviewStep } from "./lyricsEditor/PreviewStep";
import { EditorTransport } from "./lyricsEditor/EditorTransport";
import {
  isTypingTarget,
  keepFocus,
  type SyncCursor,
  type SyncMode,
} from "./lyricsEditor/shared";

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

type Step = "edit" | "sync" | "preview";
const STEPS: Step[] = ["edit", "sync", "preview"];

/** The formats offered, richest first, with the extension each is saved
 *  under. */
const FORMATS: { id: LyricsFormat; ext: string }[] = [
  { id: "ttml", ext: "ttml" },
  { id: "enhanced_lrc", ext: "lrc" },
  { id: "lrc", ext: "lrc" },
  { id: "plain", ext: "txt" },
];

type Unsynced = Extract<LyricsDocumentWrite, { kind: "unsynced" }>;

/**
 * The lyrics editor, in three steps over one document:
 *
 * - **Edit** — the text, the singers, and per line the section, the
 *   backing vocals, a translation and a romanization;
 * - **Sync** — keys stamp the lines or the words as the song plays;
 * - **Preview** — the karaoke the app will draw from what is saved.
 *
 * The song plays through the app's own player (its seek, its speed), so
 * the stamps are taken against the position the engine reports. The
 * document is the backend's lyrics model (`waveflow_core::lyrics`): it is
 * read from the stored lyrics on open and written back in the format
 * picked here, then saved through `save_lyrics` like any edit.
 */
export function LyricsEditorModal(props: LyricsEditorModalProps) {
  // The backdrop asks the dialog, which knows whether closing would lose
  // an edit. The dialog is mounted only while the modal is open, so a
  // closed editor neither reads the profile nor re-renders on every
  // position event.
  const requestCloseRef = useRef(props.onClose);
  return (
    <AnimatedModalShell
      isOpen={props.isOpen}
      onBackdropClick={() => requestCloseRef.current()}
    >
      <LyricsEditorDialog {...props} requestCloseRef={requestCloseRef} />
    </AnimatedModalShell>
  );
}

function LyricsEditorDialog({
  isOpen,
  onClose,
  trackId,
  trackTitle,
  initial,
  onSaved,
  trackFilePath,
  requestCloseRef,
}: LyricsEditorModalProps & { requestCloseRef: RefObject<() => void> }) {
  const { t } = useTranslation();
  const { currentTrack, togglePlayback, seek, setPlaybackSpeed } = usePlayer();
  const { estimate, isPlaying, playbackSpeed } = usePlaybackClock();
  const { reactionMs, setReactionMs } = useLyricsEditorReaction();

  const history = useUndoHistory<LyricsDocument>(EMPTY_LYRICS_DOCUMENT);
  const doc = history.state;
  const { set: edit, undo, redo } = history;

  const [step, setStep] = useState<Step>("edit");
  const [selected, setSelected] = useState(0);
  const [syncMode, setSyncMode] = useState<SyncMode>("line");
  const [cursor, setCursor] = useState<SyncCursor>({ line: 0, word: 0 });
  const [format, setFormat] = useState<LyricsFormat>("lrc");
  // Whether the user picked the format on this open. Until then, plain
  // text that gets its first stamp moves to a format that keeps it.
  const formatTouchedRef = useRef(false);
  const [lrcTranslations, setLrcTranslations] = useState(true);
  const [loading, setLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The stored lyrics could not be read: the document shown is not them,
  // so nothing is saved or exported over them.
  const [readFailed, setReadFailed] = useState(false);
  /** Surfaced after save when the backend kept the lyrics in-DB but
   *  couldn't write them to the audio file's tag (e.g. TTML on MP3). */
  const [warning, setWarning] = useState<string | null>(null);
  /** The unit a save or an export stopped at. */
  const [unsynced, setUnsynced] = useState<Unsynced | null>(null);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  // The document as last read or saved: anything else is an edit closing
  // would lose. Compared by identity, so undoing back to it is clean.
  const [savedDoc, setSavedDoc] = useState<LyricsDocument | null>(null);
  const dirty = savedDoc !== null && doc !== savedDoc;
  // The track whose lyrics were opened. Saving goes to it even if the
  // player has moved on; syncing against another song is flagged.
  const [editingTrackId, setEditingTrackId] = useState<number | null>(null);

  // Per-edit destination override. Initial value mirrors the app-wide
  // default fetched on open; the user can flip it for the current save
  // without touching the global setting (Settings → Lyrics). Defaults
  // to `tag` for pre-init renders so a very fast save before the fetch
  // lands behaves like the legacy flow.
  const [destination, setDestinationState] = useState<LyricsDestination>("tag");
  // True once the user has touched the segmented control on the current
  // open, so the async default fetch defers to a manual pick that landed
  // first. Reset on every open.
  const destinationUserTouchedRef = useRef(false);
  const setDestination = useCallback(
    (next: LyricsDestination, fromUser: boolean = true) => {
      if (fromUser) destinationUserTouchedRef.current = true;
      setDestinationState(next);
    },
    [],
  );

  // Close, unless that would throw away an unsaved edit the user has not
  // agreed to lose: Escape is one key away from the ones that sync.
  const requestClose = useCallback(() => {
    if (dirty && !confirmDiscard) {
      setConfirmDiscard(true);
      return;
    }
    onClose();
  }, [dirty, confirmDiscard, onClose]);
  useLayoutEffect(() => {
    requestCloseRef.current = requestClose;
  });
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, requestClose);
  const bodyRef = useRef<HTMLDivElement>(null);

  // ── Destination default ───────────────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    destinationUserTouchedRef.current = false;
    let cancelled = false;
    getLyricsDefaultDestination().then(
      (def) => {
        if (cancelled || destinationUserTouchedRef.current) return;
        setDestination(def, false);
      },
      (err) => {
        // Never block the editor from opening over a stale setting fetch.
        if (cancelled || destinationUserTouchedRef.current) return;
        console.warn("[LyricsEditor] default destination fetch failed", err);
        setDestination("tag", false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [isOpen, setDestination]);

  // ── Hydrate from the stored lyrics ────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    /* eslint-disable react-hooks/set-state-in-effect */
    setStep("edit");
    setSelected(0);
    setCursor({ line: 0, word: 0 });
    setError(null);
    setWarning(null);
    setUnsynced(null);
    setConfirmDiscard(false);
    setEditingTrackId(trackId);
    /* eslint-enable react-hooks/set-state-in-effect */
    // What the song itself says, for the fields the stored lyrics leave
    // empty: an LRC header and a TTML title carry them.
    const withTrack = (d: LyricsDocument): LyricsDocument => ({
      ...d,
      metadata: {
        ...d.metadata,
        title: d.metadata.title ?? trackTitle ?? undefined,
        artist: d.metadata.artist ?? currentTrack?.artist_name ?? undefined,
        album: d.metadata.album ?? currentTrack?.album_title ?? undefined,
      },
    });
    const start = (d: LyricsDocument) => {
      history.reset(d);
      setSavedDoc(d);
    };
    if (!initial || !initial.content.trim()) {
      start(withTrack(EMPTY_LYRICS_DOCUMENT));
      setFormat("lrc");
      setSyncMode("line");
      return;
    }
    let cancelled = false;
    setLoading(true);
    start(withTrack(EMPTY_LYRICS_DOCUMENT));
    readLyricsDocument(initial.content, initial.format)
      .then(({ format: read, document }) => {
        if (cancelled) return;
        start(withTrack(document));
        setFormat(read);
        setSyncMode(
          document.lines.some(isWordSynced) || read === "enhanced_lrc"
            ? "word"
            : "line",
        );
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("[LyricsEditor] read failed", err);
        setError(String(err));
        setReadFailed(true);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // Rehydrate when the modal opens, not when the panel's payload or the
    // playing track changes under an open editor.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen]);

  // ── The engine's speed, given back on close ───────────────────────
  // The editor's speed buttons drive the player itself; leaving the
  // song at half speed after the editor closes is not what was asked.
  const speedRef = useRef(playbackSpeed);
  useEffect(() => {
    speedRef.current = playbackSpeed;
  }, [playbackSpeed]);
  useEffect(() => {
    if (!isOpen) return;
    const before = speedRef.current;
    return () => {
      if (Math.abs(speedRef.current - before) > 0.001) {
        setPlaybackSpeed(before);
      }
    };
  }, [isOpen, setPlaybackSpeed]);

  // ── Undo / redo ───────────────────────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      // A field has its own undo for what is typed in it.
      if (isTypingTarget(e.target)) return;
      const key = e.key.toLowerCase();
      if (key === "z" && !e.shiftKey) undo();
      else if ((key === "z" && e.shiftKey) || key === "y") redo();
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [isOpen, undo, redo]);

  // ── The keys belong to the Sync step ──────────────────────────────
  // Opening focuses the first control (the Edit step's button), and a
  // step reached with the mouse leaves the focus there: Space would
  // press that button rather than stamp. The dialog takes the focus
  // instead; Tab still walks the controls from there.
  useEffect(() => {
    if (step !== "sync") return;
    const active = document.activeElement;
    if (
      active instanceof HTMLElement &&
      !isTypingTarget(active) &&
      active.closest("button, a, [role='radio']")
    ) {
      dialogRef.current?.focus();
    }
  }, [step, dialogRef]);

  // ── Plain text that starts being synced ───────────────────────────
  // Lyrics opened as plain text save as plain text, unless they are
  // synced here: the first stamp moves them to LRC (TTML when stamped
  // word by word), or the sync would be written away on save. A format
  // the user picked is theirs.
  const stamped = doc.lines.some((l) => lineBegin(l) !== undefined);
  useEffect(() => {
    if (format !== "plain" || !stamped || formatTouchedRef.current) return;
    setFormat(syncMode === "word" ? "ttml" : "lrc");
  }, [format, stamped, syncMode]);

  // ── Writing ───────────────────────────────────────────────────────
  const writeOptions = useMemo(
    () => ({ translations: lrcTranslations }),
    [lrcTranslations],
  );

  // What the Preview step draws: the document as it would be saved, in
  // the format picked, without the lines that have no time yet. Plain
  // text has nothing to play along, and Enhanced LRC refuses a line not
  // timed word by word, so those preview as TTML, which takes both.
  const [preview, setPreview] = useState<{
    content: string;
    format: LyricsFormat;
  } | null>(null);
  const previewSource = useMemo(() => {
    const cleaned = cleanedDocument(doc);
    const timed = cleaned.lines.filter(
      (l) => l.words.length > 0 && lineBegin(l) !== undefined,
    );
    const hidden = cleaned.lines.filter((l) => l.words.length > 0).length;
    return {
      doc: { ...cleaned, lines: timed },
      hidden: hidden - timed.length,
    };
  }, [doc]);
  useEffect(() => {
    if (!isOpen || step !== "preview") return;
    let cancelled = false;
    // Typing in another step never reaches here; a burst of nudges does.
    const timer = window.setTimeout(async () => {
      try {
        const asked: LyricsFormat = format === "plain" ? "ttml" : format;
        let written = await writeLyricsDocument(
          previewSource.doc,
          asked,
          writeOptions,
        );
        let shown = asked;
        if (written.kind === "unsynced") {
          shown = "ttml";
          written = await writeLyricsDocument(previewSource.doc, "ttml");
        }
        if (cancelled) return;
        setPreview(
          written.kind === "written"
            ? { content: written.content, format: shown }
            : { content: "", format: "plain" },
        );
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    }, 120);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [isOpen, step, previewSource, format, writeOptions]);

  const hasText = doc.lines.some((l) => l.words.length > 0);

  /** The document in the format picked, or `null` after showing which
   *  unit keeps it from being written. */
  const writeForSave = async (): Promise<string | null> => {
    const written = await writeLyricsDocument(
      cleanedDocument(doc),
      format,
      writeOptions,
    );
    if (written.kind === "unsynced") {
      setUnsynced(written);
      return null;
    }
    setUnsynced(null);
    return written.content;
  };

  const showUnsynced = (at: Unsynced) => {
    setStep("sync");
    if (at.word !== undefined) setSyncMode("word");
    setCursor({ line: at.line, word: at.word ?? 0 });
    setUnsynced(null);
  };

  // ── Save ──────────────────────────────────────────────────────────
  const handleSave = async () => {
    if (editingTrackId == null || !hasText) return;
    setIsSaving(true);
    setError(null);
    setWarning(null);
    try {
      const content = await writeForSave();
      if (content === null) return;
      // The backend pauses playback if we're editing the currently
      // playing file before it rewrites the tag.
      const next = await saveLyrics(editingTrackId, {
        content,
        format,
        destination,
      });
      // The panel shows the playing track: hand it lyrics only if that
      // is still the one they belong to (`lyrics:updated` covers the rest).
      if (next.track_id === currentTrack?.id) onSaved(next);
      if (next.tag_write_skipped) {
        // Keep the modal open with a warning so the user knows the file
        // itself wasn't touched — DB cache still updated.
        setSavedDoc(doc);
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
      const content = await writeForSave();
      if (content === null) return;
      const ext = FORMATS.find((f) => f.id === format)?.ext ?? "lrc";
      const stem = filenameStem(trackFilePath, trackTitle);
      const target = await showSaveDialog({
        title: t("lyricsEditor.exportToFile") ?? undefined,
        defaultPath: defaultExportPath(trackFilePath, stem, ext),
        filters: [
          {
            name: t(`lyricsEditor.format.${format}`),
            extensions: [ext],
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

  const unsyncedText = unsynced
    ? unsynced.word !== undefined
      ? t("lyricsEditor.unsynced.word", {
          format: t(`lyricsEditor.format.${format}`),
          line: unsynced.line + 1,
          word: unsynced.word + 1,
        })
      : t("lyricsEditor.unsynced.line", {
          format: t(`lyricsEditor.format.${format}`),
          line: unsynced.line + 1,
        })
    : "";
  const otherTrackPlaying =
    editingTrackId != null && currentTrack?.id !== editingTrackId;
  const hasTranslations = doc.lines.some((l) => l.translations?.length);

  return (
    <AnimatedModalContent
      ref={dialogRef}
      // Focusable itself, so the Sync step can take the focus off a
      // button without sending it out of the dialog.
      tabIndex={-1}
      role="dialog"
      aria-modal="true"
      aria-labelledby="lyrics-editor-title"
      className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-5xl h-[min(56rem,calc(100vh-2rem))] flex flex-col overflow-hidden"
    >
      {/* Header */}
      <div className="flex items-center gap-4 px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
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
        <nav
          aria-label={t("lyricsEditor.stepsLabel")}
          className="mx-auto flex rounded-full bg-zinc-100 dark:bg-zinc-800 p-0.5 text-sm"
        >
          {STEPS.map((s, k) => (
            <button
              key={s}
              type="button"
              aria-current={step === s ? "step" : undefined}
              onMouseDown={keepFocus}
              onClick={() => setStep(s)}
              className={`px-4 py-1.5 rounded-full font-medium transition-colors ${
                step === s
                  ? "bg-white dark:bg-zinc-700 text-zinc-900 dark:text-white shadow-sm"
                  : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200"
              }`}
            >
              <span className="mr-1.5 text-xs tabular-nums opacity-60">
                {k + 1}
              </span>
              {t(`lyricsEditor.steps.${s}`)}
            </button>
          ))}
        </nav>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onMouseDown={keepFocus}
            onClick={history.undo}
            disabled={!history.canUndo}
            title={t("lyricsEditor.undo")}
            aria-label={t("lyricsEditor.undo")}
            className="p-2 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-30 transition-colors"
          >
            <Undo2 size={16} />
          </button>
          <button
            type="button"
            onMouseDown={keepFocus}
            onClick={history.redo}
            disabled={!history.canRedo}
            title={t("lyricsEditor.redo")}
            aria-label={t("lyricsEditor.redo")}
            className="p-2 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-30 transition-colors"
          >
            <Redo2 size={16} />
          </button>
          <button
            type="button"
            onClick={requestClose}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>
      </div>

      {otherTrackPlaying && step !== "edit" && (
        <div className="flex items-center gap-2 px-6 py-2 text-xs bg-amber-50 dark:bg-amber-950/30 text-amber-700 dark:text-amber-300 border-b border-amber-200 dark:border-amber-900">
          <AlertTriangle size={14} />
          {t("lyricsEditor.otherTrackPlaying")}
        </div>
      )}

      {/* Body — the one scroller of the editor, handed to its lists as
            the page they virtualize against. The preview scrolls itself,
            as the immersive view does. */}
      <PageScrollContext.Provider value={bodyRef}>
        <div
          ref={bodyRef}
          className={`flex-1 min-h-0 p-6 ${
            step === "preview" ? "overflow-hidden" : "overflow-y-auto"
          }`}
        >
          {loading ? (
            <div className="flex h-full items-center justify-center text-zinc-400">
              <Loader2 size={20} className="animate-spin" />
            </div>
          ) : step === "edit" ? (
            <EditStep
              doc={doc}
              edit={edit}
              selected={selected}
              onSelect={(line) => {
                setSelected(line);
                setCursor({ line, word: 0 });
              }}
            />
          ) : step === "sync" ? (
            <SyncStep
              doc={doc}
              edit={edit}
              mode={syncMode}
              setMode={setSyncMode}
              cursor={cursor}
              setCursor={setCursor}
              reactionMs={reactionMs}
              setReactionMs={setReactionMs}
              estimate={estimate}
              seek={(ms) => void seek(ms).catch(() => {})}
              togglePlayback={() => void togglePlayback()}
              active={isOpen && !confirmDiscard}
            />
          ) : currentTrack ? (
            <PreviewStep
              track={currentTrack}
              written={preview}
              hiddenLines={previewSource.hidden}
              seek={(ms) => void seek(ms).catch(() => {})}
            />
          ) : null}
        </div>
      </PageScrollContext.Provider>

      {/* Transport */}
      <div className="px-6 py-3 border-t border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-950/40">
        <EditorTransport
          isPlaying={isPlaying}
          togglePlayback={() => void togglePlayback()}
          seek={(ms) => void seek(Math.max(0, ms)).catch(() => {})}
          estimate={estimate}
          speed={playbackSpeed}
          setSpeed={setPlaybackSpeed}
          durationMs={currentTrack?.duration_ms ?? null}
        />
      </div>

      {/* Footer — flex-wrap so on a narrow window the pickers and the
            action group stack instead of pushing Save off-screen (#341). */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
        {confirmDiscard ? (
          <div className="flex basis-full items-center justify-end gap-3 text-sm">
            <span className="mr-auto text-amber-600 dark:text-amber-400">
              {t("lyricsEditor.discard.question")}
            </span>
            <button
              type="button"
              onClick={() => setConfirmDiscard(false)}
              className="px-4 py-2 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              {t("lyricsEditor.discard.keep")}
            </button>
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 rounded-full bg-red-500 hover:bg-red-600 text-white font-medium transition-colors"
            >
              {t("lyricsEditor.discard.confirm")}
            </button>
          </div>
        ) : (
          <>
            <label className="flex items-center gap-2 text-xs text-zinc-600 dark:text-zinc-300">
              {t("lyricsEditor.formatLabel")}
              <select
                value={format}
                onChange={(e) => {
                  formatTouchedRef.current = true;
                  setFormat(e.target.value as LyricsFormat);
                  setUnsynced(null);
                }}
                className="rounded-full border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 px-3 py-1 text-xs font-medium focus:outline-none focus:ring-2 focus:ring-pink-500"
              >
                {FORMATS.map((f) => (
                  <option key={f.id} value={f.id}>
                    {t(`lyricsEditor.format.${f.id}`)}
                  </option>
                ))}
              </select>
            </label>
            {(format === "lrc" || format === "enhanced_lrc") &&
              hasTranslations && (
                <label className="flex items-center gap-1.5 text-xs text-zinc-600 dark:text-zinc-300">
                  <input
                    type="checkbox"
                    checked={lrcTranslations}
                    onChange={(e) => setLrcTranslations(e.target.checked)}
                    className="accent-pink-500"
                  />
                  {t("lyricsEditor.lrcTranslations")}
                </label>
              )}
            {/* Destination segmented control (issue #201): the
                  app-wide default fetched on open, overridable per save. */}
            <div
              role="radiogroup"
              aria-label={t("lyricsEditor.destinationLabel")}
              className="flex items-center shrink-0 whitespace-nowrap rounded-full bg-zinc-100 dark:bg-zinc-800 p-0.5 text-xs"
            >
              {(["tag", "sidecar", "db_only"] as const).map((value) => {
                const checked = destination === value;
                return (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={checked}
                    onClick={() => setDestination(value)}
                    title={
                      t(`lyricsEditor.destination.${value}.hint`) ?? undefined
                    }
                    className={[
                      "px-3 py-1 rounded-full transition-colors font-medium",
                      checked
                        ? "bg-white dark:bg-zinc-700 text-zinc-900 dark:text-white shadow-sm"
                        : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200",
                    ].join(" ")}
                  >
                    {t(`lyricsEditor.destination.${value}.label`)}
                  </button>
                );
              })}
            </div>
            {/* min-w-0 lets the messages truncate instead of squeezing
                  the Save button out of view (issue #341). */}
            <div className="flex basis-full lg:flex-1 items-center gap-2 min-w-0 justify-end">
              {unsynced && (
                <span className="flex items-center gap-2 text-xs text-amber-600 dark:text-amber-400 min-w-0">
                  <span className="truncate" title={unsyncedText}>
                    {unsyncedText}
                  </span>
                  <button
                    type="button"
                    onClick={() => showUnsynced(unsynced)}
                    className="shrink-0 underline underline-offset-2"
                  >
                    {t("lyricsEditor.unsynced.show")}
                  </button>
                </span>
              )}
              {warning && (
                <span className="text-xs text-amber-600 dark:text-amber-400 truncate min-w-0">
                  {warning}
                </span>
              )}
              {error && (
                <span className="text-xs text-red-500 truncate min-w-0">
                  {error}
                </span>
              )}
              <div className="flex items-center gap-2 shrink-0">
                <button
                  type="button"
                  onClick={handleExportToFile}
                  disabled={isSaving || readFailed || !hasText}
                  className="px-4 py-2 rounded-full text-sm border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors disabled:opacity-50 flex items-center gap-2 whitespace-nowrap"
                  title={t("lyricsEditor.exportToFileHint") ?? undefined}
                >
                  <FileDown size={14} />
                  {t("lyricsEditor.exportToFile")}
                </button>
                <button
                  type="button"
                  onClick={handleSave}
                  disabled={
                    isSaving || readFailed || editingTrackId == null || !hasText
                  }
                  className="px-5 py-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 text-sm font-medium hover:opacity-90 disabled:opacity-50 transition-opacity flex items-center gap-2 whitespace-nowrap"
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
          </>
        )}
      </div>
    </AnimatedModalContent>
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
 * Build the default `defaultPath` the Tauri save dialog should
 * open at. When `filePath` is known we anchor on its parent
 * directory so the user lands next to the song — the conventional
 * sidecar location. When `filePath` is absent we still hand back
 * a bare `<stem>.<ext>` filename so the dialog pre-fills the name
 * field; Tauri's save dialog interprets a name-only `defaultPath`
 * as "land in the last-used directory with this name" (OS-native
 * behaviour on Windows / macOS / Linux), which is the best
 * fallback when we have no song path to anchor on.
 */
function defaultExportPath(
  filePath: string | null | undefined,
  stem: string,
  ext: string,
): string {
  if (!filePath) return `${stem}.${ext}`;
  // Preserve OS-native separators so the dialog re-renders the path
  // correctly on Windows + macOS + Linux. Splitting on both is fine
  // because Windows accepts both `\` and `/` in API paths.
  const sepIdx = Math.max(
    filePath.lastIndexOf("/"),
    filePath.lastIndexOf("\\"),
  );
  if (sepIdx < 0) return `${stem}.${ext}`;
  const sep = filePath[sepIdx];
  return `${filePath.substring(0, sepIdx)}${sep}${stem}.${ext}`;
}
