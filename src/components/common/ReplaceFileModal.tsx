import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { FileAudio, Loader2, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import { pickFile } from "../../lib/tauri/dialog";
import { formatDuration, type Track } from "../../lib/tauri/track";
import {
  checkTrackReplacement,
  replaceTrackFile,
  type FileFacts,
  type ReplaceOutcome,
  type ReplacementCheck,
  type ReplaceRefusal,
} from "../../lib/tauri/replace";

/** The extensions the library takes and the player decodes. */
const AUDIO_EXTENSIONS = [
  "flac",
  "wav",
  "aiff",
  "aif",
  "mp3",
  "m4a",
  "mp4",
  "aac",
  "ogg",
  "oga",
  "opus",
];

interface ReplaceFileModalProps {
  /** The track whose file is replaced; `null` closes the dialog. */
  track: Track | null;
  onClose: () => void;
}

type Stage =
  | { kind: "picking" }
  | { kind: "checking" }
  | { kind: "ready"; check: ReplacementCheck; source: string }
  | { kind: "replacing"; check: ReplacementCheck; source: string }
  | {
      kind: "done";
      outcome: Extract<ReplaceOutcome, { kind: "replaced" }>;
    }
  | { kind: "refused"; refusal: ReplaceRefusal }
  | { kind: "error"; message: string };

/**
 * Replace a track's file with a better copy of the same recording (a
 * lossless rip of an MP3, a cleaner re-rip), keeping the track.
 *
 * The file picker opens first; the dialog then shows the current file
 * against the chosen one — format and length — and where the new one
 * will be saved, and asks. The backend checks everything again when the
 * user confirms, so a track that started playing in between is refused
 * rather than swapped under the player.
 */
export function ReplaceFileModal({ track, onClose }: ReplaceFileModalProps) {
  const { t, i18n } = useTranslation();
  const isOpen = track != null;
  const [stage, setStage] = useState<Stage>({ kind: "picking" });
  // Not closable mid-swap: the outcome would be lost, and a Cancel button
  // would promise an interruption the backend does not offer.
  const busy = stage.kind === "replacing";
  const guardedClose = () => {
    if (!busy) onClose();
  };
  // The picker is the first thing the user sees; the dialog itself only
  // opens once there is something to show — and takes focus then.
  const visible = isOpen && stage.kind !== "picking";
  const dialogRef = useModalA11y<HTMLDivElement>(visible, guardedClose);
  // A late answer for a dialog the user already closed is dropped.
  const session = useRef(0);

  useEffect(() => {
    if (track == null) return;
    const mine = ++session.current;
    let cancelled = false;
    (async () => {
      // `i18n.t`, not `t`: the instance is stable, so a language change
      // does not re-run this effect and reopen the picker mid-dialog.
      const source = await pickFile(
        AUDIO_EXTENSIONS,
        i18n.t("replaceFile.pickTitle"),
      ).catch(() => null);
      if (cancelled || mine !== session.current) return;
      if (source == null) {
        onClose();
        return;
      }
      setStage({ kind: "checking" });
      try {
        const check = await checkTrackReplacement(track.id, source);
        if (cancelled || mine !== session.current) return;
        setStage({ kind: "ready", check, source });
      } catch (err) {
        if (cancelled || mine !== session.current) return;
        setStage({ kind: "error", message: String(err) });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [track, onClose, i18n]);

  const handleReplace = async () => {
    if (track == null || stage.kind !== "ready") return;
    const mine = session.current;
    setStage({ kind: "replacing", check: stage.check, source: stage.source });
    try {
      const outcome = await replaceTrackFile(track.id, stage.source);
      if (mine !== session.current) return;
      setStage(
        outcome.kind === "replaced"
          ? { kind: "done", outcome }
          : { kind: "refused", refusal: outcome.refusal },
      );
    } catch (err) {
      if (mine !== session.current) return;
      setStage({ kind: "error", message: String(err) });
    }
  };

  const describe = (facts: FileFacts) => {
    const parts = [facts.codec ?? "?"];
    if (facts.bit_depth != null) {
      parts.push(t("replaceFile.bits", { value: facts.bit_depth }));
    }
    if (facts.sample_rate != null) {
      parts.push(`${facts.sample_rate / 1000} kHz`);
    }
    parts.push(formatDuration(facts.duration_ms));
    return parts.join(" · ");
  };

  const refusalText = (refusal: ReplaceRefusal) => {
    switch (refusal.kind) {
      case "playing":
        return t("replaceFile.refusals.playing");
      case "same_file":
        return t("replaceFile.refusals.sameFile");
      case "not_audio":
        return t("replaceFile.refusals.notAudio");
      case "duration_mismatch":
        return t("replaceFile.refusals.durationMismatch", {
          current: formatDuration(refusal.current_ms),
          replacement: formatDuration(refusal.replacement_ms),
        });
      case "target_taken":
        return t("replaceFile.refusals.targetTaken", { path: refusal.path });
    }
  };

  const check =
    stage.kind === "ready" || stage.kind === "replacing" ? stage.check : null;

  return (
    <AnimatedModalShell isOpen={visible} onBackdropClick={guardedClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="replace-file-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-xl flex flex-col overflow-hidden"
      >
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2 min-w-0">
            <FileAudio size={18} className="text-emerald-500" />
            <h2 id="replace-file-title" className="text-lg font-semibold">
              {t("replaceFile.title")}
            </h2>
          </div>
          <button
            type="button"
            onClick={guardedClose}
            disabled={busy}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>

        <div className="p-6 space-y-4 text-sm">
          {stage.kind === "checking" && (
            <div className="flex items-center gap-2 text-zinc-500">
              <Loader2 size={16} className="animate-spin" />
              {t("replaceFile.checking")}
            </div>
          )}
          {check && (
            <>
              <dl className="space-y-3">
                <div>
                  <dt className="text-xs uppercase tracking-wide text-zinc-500">
                    {t("replaceFile.current")}
                  </dt>
                  <dd className="font-medium">{describe(check.current)}</dd>
                  <dd className="text-xs font-mono text-zinc-500 break-all">
                    {check.current.path}
                  </dd>
                </div>
                {check.replacement && (
                  <div>
                    <dt className="text-xs uppercase tracking-wide text-zinc-500">
                      {t("replaceFile.replacement")}
                    </dt>
                    <dd className="font-medium">
                      {describe(check.replacement)}
                    </dd>
                    <dd className="text-xs font-mono text-zinc-500 break-all">
                      {check.replacement.path}
                    </dd>
                  </div>
                )}
                <div>
                  <dt className="text-xs uppercase tracking-wide text-zinc-500">
                    {t("replaceFile.target")}
                  </dt>
                  <dd className="text-xs font-mono text-zinc-500 break-all">
                    {check.target_path}
                  </dd>
                </div>
              </dl>
              {check.refusal ? (
                <p role="alert" className="text-red-500">
                  {refusalText(check.refusal)}
                </p>
              ) : (
                <>
                  <p className="text-zinc-600 dark:text-zinc-300">
                    {t("replaceFile.keeps")}
                  </p>
                  {check.adopts && (
                    <p className="text-zinc-600 dark:text-zinc-300">
                      {t("replaceFile.adopts")}
                    </p>
                  )}
                </>
              )}
            </>
          )}
          {stage.kind === "done" && (
            <p role="status" className="font-medium">
              {stage.outcome.old_file.kind === "trashed"
                ? t("replaceFile.done")
                : t("replaceFile.keptAside", {
                    path: stage.outcome.old_file.path,
                  })}
            </p>
          )}
          {stage.kind === "refused" && (
            <p role="alert" className="text-red-500">
              {refusalText(stage.refusal)}
            </p>
          )}
          {stage.kind === "error" && (
            <p role="alert" className="text-red-500 break-words">
              {stage.message}
            </p>
          )}
        </div>

        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <button
            type="button"
            onClick={guardedClose}
            disabled={busy}
            className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            {stage.kind === "done" ||
            stage.kind === "error" ||
            stage.kind === "refused"
              ? t("common.close")
              : t("common.cancel")}
          </button>
          {check && (
            <button
              type="button"
              onClick={handleReplace}
              disabled={check.refusal != null || stage.kind === "replacing"}
              className="px-5 py-2 rounded-full bg-emerald-500 text-white text-sm font-medium hover:bg-emerald-600 disabled:opacity-50 transition-colors flex items-center gap-2"
            >
              {stage.kind === "replacing" && (
                <Loader2 size={14} className="animate-spin" />
              )}
              {stage.kind === "replacing"
                ? t("replaceFile.replacing")
                : t("replaceFile.confirm")}
            </button>
          )}
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
