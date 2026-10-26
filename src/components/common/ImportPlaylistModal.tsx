import { useCallback, useId, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { FileUp, Link2, ListMusic, Loader2, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { pickFile } from "../../lib/tauri/dialog";
import {
  importPlaylistDeezer,
  importPlaylistFile,
  playlistImportErrorKey,
  type ImportPlaylistResult,
} from "../../lib/tauri/playlist";

interface ImportPlaylistModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** A playlist was created; the parent refreshes its list and opens it. */
  onImported: (result: ImportPlaylistResult) => void;
}

/**
 * Bring a playlist in from elsewhere: a file (M3U, CSV, JSON) or a public
 * Deezer playlist's link. What the library holds becomes the playlist's
 * tracks; the rest is kept in its place as entries waiting for their track,
 * which a later scan fills in.
 *
 * The dialog stays open on the outcome so the user reads how much came in
 * before going to the playlist.
 */
export function ImportPlaylistModal({
  isOpen,
  onClose,
  onImported,
}: ImportPlaylistModalProps) {
  const { t } = useTranslation();
  const [link, setLink] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<ImportPlaylistResult | null>(null);
  const linkId = useId();

  const reset = useCallback(() => {
    setLink("");
    setError(null);
    setResult(null);
  }, []);
  // Not while an import runs: it would still land, and create a playlist
  // the user had walked away from without telling them.
  const close = useCallback(() => {
    if (busy) return;
    reset();
    onClose();
  }, [busy, onClose, reset]);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, close);

  const run = useCallback(
    async (action: () => Promise<ImportPlaylistResult>) => {
      setBusy(true);
      setError(null);
      setResult(null);
      try {
        const imported = await action();
        setResult(imported);
        onImported(imported);
      } catch (err) {
        const key = playlistImportErrorKey(err);
        setError(key ? t(key) : String(err));
      } finally {
        setBusy(false);
      }
    },
    [onImported, t],
  );

  const handleFile = useCallback(async () => {
    if (busy) return;
    const path = await pickFile(
      ["m3u8", "m3u", "csv", "tsv", "txt", "json"],
      t("playlistImport.fileDialogTitle"),
    );
    if (!path) return;
    await run(() => importPlaylistFile(path));
  }, [busy, run, t]);

  const handleLink = useCallback(async () => {
    if (busy || link.trim() === "") return;
    await run(() => importPlaylistDeezer(link.trim()));
  }, [busy, link, run]);

  if (!isOpen) return null;

  const inputClass =
    "w-full px-3 py-2 rounded-lg text-sm bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white focus:outline-none focus:ring-2 focus:ring-emerald-500";

  // Portalled: see docs/architecture/invariants.md#overlays-must-be-portalled.
  return createPortal(
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.18, ease: "easeOut" }}
      className="fixed inset-0 z-100 bg-black/80 backdrop-blur-md flex items-center justify-center p-4"
      onClick={close}
    >
      <motion.div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="import-playlist-title"
        initial={{ opacity: 0, scale: 0.95, y: 8 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 380, damping: 28, mass: 0.6 }}
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4 mb-5">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-emerald-500/10 text-emerald-500 flex items-center justify-center">
              <ListMusic size={18} aria-hidden="true" />
            </div>
            <h2
              id="import-playlist-title"
              className="text-lg font-bold text-zinc-900 dark:text-white"
            >
              {t("playlistImport.title")}
            </h2>
          </div>
          <button
            type="button"
            onClick={close}
            disabled={busy}
            aria-label={t("common.close")}
            className="p-2 rounded-full text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4">
          {t("playlistImport.help")}
        </p>

        <button
          type="button"
          onClick={() => void handleFile()}
          disabled={busy}
          className="w-full flex items-center gap-3 px-4 py-3 rounded-xl border border-zinc-200 dark:border-zinc-700 text-left hover:border-emerald-500 hover:bg-emerald-500/5 transition-colors disabled:opacity-50"
        >
          <FileUp size={18} className="text-emerald-500 shrink-0" aria-hidden="true" />
          <span className="min-w-0">
            <span className="block text-sm font-semibold text-zinc-900 dark:text-white">
              {t("playlistImport.fromFile")}
            </span>
            <span className="block text-xs text-zinc-500 dark:text-zinc-400">
              {t("playlistImport.fromFileHint")}
            </span>
          </span>
        </button>

        <form
          noValidate
          className="mt-4"
          onSubmit={(e) => {
            e.preventDefault();
            void handleLink();
          }}
        >
          <label
            htmlFor={linkId}
            className="block text-xs font-semibold tracking-wider uppercase text-zinc-500 mb-1"
          >
            {t("playlistImport.linkLabel")}
          </label>
          <div className="flex gap-2">
            <input
              id={linkId}
              name="link"
              type="url"
              value={link}
              onChange={(e) => setLink(e.currentTarget.value)}
              placeholder="https://www.deezer.com/playlist/…"
              spellCheck={false}
              disabled={busy}
              className={inputClass}
            />
            <button
              type="submit"
              disabled={busy || link.trim() === ""}
              className="px-4 py-2 rounded-xl text-sm font-semibold text-white bg-emerald-500 hover:bg-emerald-600 transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
            >
              <Link2 size={14} aria-hidden="true" />
              <span>{t("playlistImport.importLink")}</span>
            </button>
          </div>
          <p className="mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            {t("playlistImport.linkHint")}
          </p>
        </form>

        {busy && (
          <div
            role="status"
            className="mt-4 flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400"
          >
            <Loader2 size={14} className="animate-spin" aria-hidden="true" />
            {t("playlistImport.working")}
          </div>
        )}

        {error && (
          <div
            role="alert"
            className="mt-4 px-3 py-2 rounded-lg bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 border border-red-200 dark:border-red-900"
          >
            {error}
          </div>
        )}

        {result && (
          <div
            role="status"
            className="mt-4 px-3 py-2 rounded-lg bg-emerald-50 dark:bg-emerald-950/30 text-xs text-emerald-800 dark:text-emerald-200 border border-emerald-200 dark:border-emerald-900 space-y-0.5"
          >
            <p className="font-semibold">{result.name}</p>
            <p>{t("playlistImport.result.imported", { count: result.imported })}</p>
            {result.placeholders > 0 && (
              <p>
                {t("playlistImport.result.placeholders", {
                  count: result.placeholders,
                })}
              </p>
            )}
            {result.duplicates > 0 && (
              <p>
                {t("playlistImport.result.duplicates", {
                  count: result.duplicates,
                })}
              </p>
            )}
          </div>
        )}

        <div className="flex items-center justify-end gap-2 mt-5">
          <button
            type="button"
            onClick={close}
            disabled={busy}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            {result ? t("common.close") : t("common.cancel")}
          </button>
        </div>
      </motion.div>
    </motion.div>,
    document.body,
  );
}
