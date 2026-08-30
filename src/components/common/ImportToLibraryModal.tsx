import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { FolderDown, Check, AlertTriangle } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useProfileSetting } from "../../hooks/useProfileSetting";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  remoteImportFolders,
  remoteImportTracks,
  type ImportFolder,
  type ImportOutcome,
  type ImportProgress,
} from "../../lib/tauri/remoteServer";

interface ImportToLibraryModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** Server track ids to copy in. */
  trackIds: string[];
  /** What to call them in the heading — one title, or a count. */
  label: string;
  /** Fired once an import finishes with at least one file written, so the
   *  caller can refresh the list it is showing. */
  onImported?: (outcome: ImportOutcome) => void;
}

const SETTING = {
  key: "remote.import_folder_id",
  defaultValue: null as number | null,
  parse: (raw: string | null) => {
    const parsed = raw === null ? Number.NaN : Number(raw);
    return Number.isFinite(parsed) ? parsed : null;
  },
  serialize: (value: number | null) => (value === null ? "" : String(value)),
  valueType: "int" as const,
  event: "waveflow:remote-import-folder-changed",
  label: "ImportToLibraryModal",
};

/**
 * Imports server tracks into a selected scanned library folder.
 *
 * @param label - Display name used to identify the tracks in the modal
 * @param onImported - Optional callback invoked when one or more tracks are imported
 */
export function ImportToLibraryModal({
  isOpen,
  onClose,
  trackIds,
  label,
  onImported,
}: ImportToLibraryModalProps) {
  const { t } = useTranslation();
  const [running, setRunning] = useState(false);
  // Escape must obey the same guard as the backdrop and the Cancel button:
  // closing mid-import does not stop the transfer, it only takes away the one
  // place its outcome is reported.
  const closeIfIdle = useCallback(() => {
    if (!running) onClose();
  }, [running, onClose]);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, closeIfIdle);
  const [folders, setFolders] = useState<ImportFolder[] | null>(null);
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [outcome, setOutcome] = useState<ImportOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const {
    value: rememberedFolderId,
    setValue: rememberFolderId,
  } = useProfileSetting<number | null>(SETTING);
  const [picked, setPicked] = useState<number | null>(null);
  // Latest-value ref so the progress listener below can be registered once
  // per open rather than re-subscribing on every byte that moves the bar.
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setOutcome(null);
    setError(null);
    setProgress(null);
    let cancelled = false;
    void (async () => {
      try {
        const list = await remoteImportFolders();
        if (!cancelled) setFolders(list);
      } catch (err) {
        console.error("[ImportToLibraryModal] folders failed", err);
        if (!cancelled) setFolders([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen || !running) return;
    const unlisten = listen<ImportProgress>("remote:import-progress", (event) => {
      if (mountedRef.current) setProgress(event.payload);
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, [isOpen, running]);

  const usable = (folders ?? []).filter((folder) => folder.exists);
  const selected =
    picked ??
    (usable.some((folder) => folder.folder_id === rememberedFolderId)
      ? rememberedFolderId
      : (usable[0]?.folder_id ?? null));

  const run = useCallback(async () => {
    if (selected === null || trackIds.length === 0) return;
    setRunning(true);
    setError(null);
    try {
      const result = await remoteImportTracks(trackIds, selected);
      if (!mountedRef.current) return;
      setOutcome(result);
      rememberFolderId(selected);
      if (result.imported.length > 0) onImported?.(result);
    } catch (err) {
      console.error("[ImportToLibraryModal] import failed", err);
      if (mountedRef.current) setError(String(err));
    } finally {
      if (mountedRef.current) {
        setRunning(false);
        setProgress(null);
      }
    }
  }, [selected, trackIds, rememberFolderId, onImported]);

  const pct =
    progress && progress.total
      ? Math.min(100, Math.round((progress.received / progress.total) * 100))
      : null;

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={closeIfIdle}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="remote-import-title"
        className="relative w-full max-w-lg rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
      >
        <h2
          id="remote-import-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-1"
        >
          {t("remote.import.title")}
        </h2>
        <p className="text-sm text-zinc-500 mb-5">
          {t("remote.import.subtitle", { name: label })}
        </p>

        {outcome ? (
          <ImportSummary outcome={outcome} />
        ) : (
          <>
            <div className="mb-2 text-[10px] font-bold tracking-widest text-zinc-500 uppercase">
              {t("remote.import.destination")}
            </div>
            {folders === null ? (
              <p className="text-sm text-zinc-500 py-4">{t("common.loading")}</p>
            ) : usable.length === 0 ? (
              <p className="text-sm text-zinc-500 py-4">
                {t("remote.import.noFolder")}
              </p>
            ) : (
              <ul className="space-y-1 max-h-56 overflow-y-auto mb-4">
                {(folders ?? []).map((folder) => {
                  const isSelected = folder.folder_id === selected;
                  return (
                    <li key={folder.folder_id}>
                      <button
                        type="button"
                        disabled={!folder.exists || running}
                        onClick={() => setPicked(folder.folder_id)}
                        className={`w-full flex items-center gap-3 px-3 py-2 rounded-xl text-left text-sm transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
                          isSelected
                            ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/25 dark:text-emerald-300"
                            : "hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-700 dark:text-zinc-300"
                        }`}
                      >
                        <FolderDown size={16} className="shrink-0" />
                        <span className="truncate flex-1" dir="ltr">
                          {folder.path}
                        </span>
                        {!folder.exists && (
                          <span className="shrink-0 text-xs text-amber-600 dark:text-amber-400">
                            {t("remote.import.unreachable")}
                          </span>
                        )}
                        {isSelected && folder.exists && (
                          <Check size={16} className="shrink-0" />
                        )}
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <p className="text-xs text-zinc-500 mb-4">
              {t("remote.import.explainer")}
            </p>
            {running && (
              <div className="mb-4">
                <div className="h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
                  <div
                    className="h-full bg-emerald-500 transition-[width]"
                    style={{ width: pct === null ? "35%" : `${pct}%` }}
                  />
                </div>
              </div>
            )}
            {error && (
              <p className="flex items-start gap-2 text-sm text-red-600 dark:text-red-400 mb-4">
                <AlertTriangle size={16} className="shrink-0 mt-0.5" />
                <span>{error}</span>
              </p>
            )}
          </>
        )}

        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            disabled={running}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800 disabled:opacity-40"
          >
            {outcome ? t("common.close") : t("common.cancel")}
          </button>
          {!outcome && (
            <button
              type="button"
              onClick={() => void run()}
              disabled={running || selected === null}
              className="px-4 py-2 rounded-xl text-sm font-semibold bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {running ? t("remote.import.running") : t("remote.import.submit")}
            </button>
          )}
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

/**
 * Displays the number of imported tracks and groups skipped tracks by refusal reason.
 *
 * @param outcome - The completed import result to summarize
 */
function ImportSummary({ outcome }: { outcome: ImportOutcome }) {
  const { t } = useTranslation();
  const byReason = new Map<string, number>();
  for (const skipped of outcome.skipped) {
    byReason.set(skipped.reason, (byReason.get(skipped.reason) ?? 0) + 1);
  }
  return (
    <div className="mb-5 space-y-2">
      <p className="text-sm text-zinc-800 dark:text-zinc-200">
        {t("remote.import.done", { count: outcome.imported.length })}
      </p>
      {[...byReason.entries()].map(([reason, count]) => (
        <p key={reason} className="text-sm text-zinc-500">
          {t(`remote.import.refusal.${reason}`, { count })}
        </p>
      ))}
    </div>
  );
}
