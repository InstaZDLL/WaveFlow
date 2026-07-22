import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Tags, FolderOpen, Trash2 } from "lucide-react";

import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import { clearGenreArtwork, setGenreArtworkFromFile } from "../../lib/tauri/browse";
import { pickFile } from "../../lib/tauri/dialog";

interface GenreImagePickerModalProps {
  genreId: number;
  hasArtwork: boolean;
  isOpen: boolean;
  onClose: () => void;
  onSuccess: () => void;
}

/**
 * Set or clear a genre's manual picture (issue #424) — a local
 * jpg/png/webp the user picks themselves. File-only: unlike
 * `ArtistImagePickerModal` there is no remote catalogue tab, since
 * genres have no automatic artwork source to browse.
 */
export function GenreImagePickerModal({
  genreId,
  hasArtwork,
  isOpen,
  onClose,
  onSuccess,
}: GenreImagePickerModalProps) {
  const { t } = useTranslation();
  const [isApplying, setIsApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setError(null);
    }
  }, [isOpen]);

  const handlePickFile = async () => {
    if (isApplying) return;
    try {
      const path = await pickFile(
        ["jpg", "jpeg", "png", "webp"],
        t("genreImagePicker.title"),
      );
      if (!path) return;
      setIsApplying(true);
      setError(null);
      await setGenreArtworkFromFile(genreId, path);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[GenreImagePickerModal] set file failed", err);
      setError(String(err));
    } finally {
      setIsApplying(false);
    }
  };

  const handleClear = async () => {
    if (isApplying) return;
    setIsApplying(true);
    setError(null);
    try {
      await clearGenreArtwork(genreId);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[GenreImagePickerModal] clear failed", err);
      setError(String(err));
    } finally {
      setIsApplying(false);
    }
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="genre-image-picker-title"
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
      >
        <h2
          id="genre-image-picker-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-1"
        >
          {t("genreImagePicker.title")}
        </h2>
        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4">
          {t("genreImagePicker.subtitle")}
        </p>

        {error && (
          <div className="mb-3 text-xs text-red-500 px-2">{error}</div>
        )}

        <div className="flex flex-col items-center justify-center py-8 space-y-4">
          <div className="w-16 h-16 rounded-2xl bg-zinc-100 dark:bg-zinc-800 flex items-center justify-center text-zinc-400">
            <Tags size={32} />
          </div>
          <button
            type="button"
            onClick={handlePickFile}
            disabled={isApplying}
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
          >
            <FolderOpen size={16} />
            <span>{t("genreImagePicker.pickFile")}</span>
          </button>
        </div>

        <div className="mt-2 flex items-center justify-between pt-3 border-t border-zinc-100 dark:border-zinc-800">
          {hasArtwork ? (
            <button
              type="button"
              onClick={handleClear}
              disabled={isApplying}
              className="px-4 py-2 rounded-xl text-sm font-medium text-red-500 hover:bg-red-50 dark:hover:bg-red-950/30 transition-colors flex items-center space-x-2 disabled:opacity-50"
            >
              <Trash2 size={14} />
              <span>{t("genreImagePicker.removeAction")}</span>
            </button>
          ) : (
            <span />
          )}
          <button
            type="button"
            onClick={onClose}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors"
          >
            {t("common.cancel")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
