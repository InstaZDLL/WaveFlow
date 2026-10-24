import { useCallback, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { FileAudio, Loader2, Radio, Save, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { pickFile } from "../../lib/tauri/dialog";
import type { PluginFavorite } from "../../lib/tauri/plugins";
import {
  probeRadioStation,
  radioStationErrorKey,
  readRadioPlaylistFile,
} from "../../lib/tauri/webRadioCustom";

interface RadioStationModalProps {
  /** `"add"` for a new station; a saved station to rename it. `null`
   *  keeps the modal closed. */
  target: "add" | PluginFavorite | null;
  onClose: () => void;
  /** A new station, checked and ready to be saved as a favorite.
   *  `false` when it could not be (the favorites have not loaded). */
  onAdd: (station: PluginFavorite) => boolean;
  onRename: (id: string, title: string) => boolean;
}

/** The host of a stream URL, credentials left out — the subtitle a
 *  station added by hand shows where the directory shows a country. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return "";
  }
}

/**
 * Add a Web Radio station the directory does not list — by pasting its
 * stream (or the `.m3u` / `.pls` its site links to), or by opening such a
 * file — or rename a saved one.
 *
 * Nothing is saved until the backend has opened the URL and found a
 * stream behind it (`web_radio_probe_url`): a station that does not play
 * is not one to keep.
 */
export function RadioStationModal({
  target,
  onClose,
  onAdd,
  onRename,
}: RadioStationModalProps) {
  const { t } = useTranslation();
  const isOpen = target != null;
  const renaming = target != null && target !== "add" ? target : null;
  const [url, setUrl] = useState("");
  // Seeded from the station being renamed: the parent mounts this dialog
  // anew for every target, so the initial state is the one to edit.
  const [name, setName] = useState(renaming?.title ?? "");
  const [busy, setBusy] = useState(false);
  // Not while the station is being checked: the check would still land
  // and save a station the user had walked away from.
  const close = useCallback(() => {
    if (!busy) onClose();
  }, [busy, onClose]);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, close);
  const [error, setError] = useState<string | null>(null);

  const showError = useCallback(
    (err: unknown) => {
      const key = radioStationErrorKey(err);
      setError(key ? t(key) : String(err));
    },
    [t],
  );

  const handleOpenFile = useCallback(async () => {
    setError(null);
    try {
      const path = await pickFile(
        ["m3u", "m3u8", "pls"],
        t("webRadio.addStation.openFile"),
      );
      if (!path) return;
      const entries = await readRadioPlaylistFile(path);
      const first = entries[0];
      setUrl(first.url);
      if (first.title && name.trim() === "") setName(first.title);
    } catch (err) {
      showError(err);
    }
  }, [name, showError, t]);

  const handleSubmit = useCallback(async () => {
    if (busy) return;
    if (renaming) {
      const title = name.trim();
      if (title === "") return;
      if (onRename(renaming.id, title)) {
        onClose();
      } else {
        setError(t("webRadio.addStation.errors.notSaved"));
      }
      return;
    }
    if (url.trim() === "") return;
    setBusy(true);
    setError(null);
    try {
      const station = await probeRadioStation(url.trim());
      const saved = onAdd({
        // The id the plugin plays without the network, and the one the
        // player bar's star builds from a playing stream — so the star
        // lights up on this station like on any other.
        id: `url:${station.stream_url}`,
        title:
          name.trim() || station.name || hostOf(station.stream_url) || url,
        artist: hostOf(station.stream_url),
        album: null,
        artworkUrl: null,
      });
      if (saved) {
        onClose();
      } else {
        setError(t("webRadio.addStation.errors.notSaved"));
      }
    } catch (err) {
      showError(err);
    } finally {
      setBusy(false);
    }
  }, [busy, name, onAdd, onClose, onRename, renaming, showError, t, url]);

  if (!isOpen) return null;

  const canSubmit = renaming ? name.trim() !== "" : url.trim() !== "";
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
        aria-labelledby="radio-station-title"
        initial={{ opacity: 0, scale: 0.95, y: 8 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 380, damping: 28, mass: 0.6 }}
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
        onClick={(e) => e.stopPropagation()}
      >
        {/* `noValidate`: a URL without a scheme is refused by the backend
            with a translated reason, not by the webview's own bubble. */}
        <form
          noValidate
          onSubmit={(e) => {
            e.preventDefault();
            void handleSubmit();
          }}
        >
          <div className="flex items-start justify-between gap-4 mb-5">
            <div className="flex items-center gap-3">
              <div className="w-10 h-10 rounded-xl bg-emerald-500/10 text-emerald-500 flex items-center justify-center">
                <Radio size={18} aria-hidden="true" />
              </div>
              <h2
                id="radio-station-title"
                className="text-lg font-bold text-zinc-900 dark:text-white"
              >
                {renaming
                  ? t("webRadio.addStation.renameTitle")
                  : t("webRadio.addStation.title")}
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

          {!renaming && (
            <>
              <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-3">
                {t("webRadio.addStation.help")}
              </p>
              <label className="block text-xs font-semibold tracking-wider uppercase text-zinc-500 mb-1">
                {t("webRadio.addStation.urlLabel")}
              </label>
              <input
                type="url"
                value={url}
                onChange={(e) => setUrl(e.currentTarget.value)}
                placeholder="https://"
                spellCheck={false}
                autoFocus
                className={inputClass}
              />
              <button
                type="button"
                onClick={() => void handleOpenFile()}
                disabled={busy}
                className="mt-2 inline-flex items-center gap-1.5 text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:underline disabled:opacity-50"
              >
                <FileAudio size={14} aria-hidden="true" />
                {t("webRadio.addStation.openFile")}
              </button>
            </>
          )}

          <label className="block text-xs font-semibold tracking-wider uppercase text-zinc-500 mt-4 mb-1">
            {t("webRadio.addStation.nameLabel")}
          </label>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            placeholder={
              renaming ? undefined : t("webRadio.addStation.namePlaceholder")
            }
            autoFocus={renaming != null}
            className={inputClass}
          />

          {error && (
            <div
              role="alert"
              className="mt-4 px-3 py-2 rounded-lg bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 border border-red-200 dark:border-red-900"
            >
              {error}
            </div>
          )}

          <div className="flex items-center justify-end gap-2 mt-5">
            <button
              type="button"
              onClick={close}
              disabled={busy}
              className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors disabled:opacity-50"
            >
              {t("common.cancel")}
            </button>
            <button
              type="submit"
              disabled={busy || !canSubmit}
              className="px-5 py-2 rounded-xl text-sm font-semibold text-white bg-emerald-500 hover:bg-emerald-600 shadow-lg transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {busy ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Save size={14} />
              )}
              <span>
                {busy
                  ? t("webRadio.addStation.checking")
                  : renaming
                    ? t("webRadio.addStation.save")
                    : t("webRadio.addStation.add")}
              </span>
            </button>
          </div>
        </form>
      </motion.div>
    </motion.div>,
    document.body,
  );
}
