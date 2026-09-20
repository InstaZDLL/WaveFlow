import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderOpen, ImageIcon, Loader2, Trash2 } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  clearArtistBackground,
  getArtistBackdropCandidates,
  setArtistBackgroundFromFile,
  setArtistBackgroundFromUrl,
} from "../../lib/tauri/deezer";
import { pickFile } from "../../lib/tauri/dialog";
import { getOfflineMode } from "../../lib/tauri/offline";

interface ArtistBackdropPickerModalProps {
  artistId: number;
  /** Whether a backdrop was already chosen — drives the "remove" action,
   *  which means "back to the automatic one", not "no backdrop". */
  hasCustomBackground: boolean;
  isOpen: boolean;
  onClose: () => void;
  onSuccess: () => void;
}

type Tab = "candidates" | "file";

/**
 * Choose the wide image behind the artist header — issue #693.
 *
 * The sibling of [`ArtistImagePickerModal`](./ArtistImagePickerModal.tsx),
 * which does the same for the round photo, and deliberately shaped like
 * it: two tabs, a remove action in the footer.
 *
 * What differs is the first tab. There is nothing to search: TheAudioDB
 * returns up to four fanarts for an artist in one lookup, and the app
 * used to paint whichever came first. They differ enormously in framing,
 * so the whole point is to see them side by side, in the aspect ratio
 * the hero will actually crop them to.
 */
export function ArtistBackdropPickerModal({
  artistId,
  hasCustomBackground,
  isOpen,
  onClose,
  onSuccess,
}: ArtistBackdropPickerModalProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("candidates");
  // Tagged with the artist it describes, so reopening the modal on
  // another artist shows the loader rather than the previous artist's
  // suggestions — and needs no reset inside the effect.
  const [loaded, setLoaded] = useState<{
    artistId: number;
    urls: string[];
    offline: boolean;
  } | null>(null);
  const current = loaded?.artistId === artistId ? loaded : null;
  const candidates = current?.urls ?? null;
  // Offline mode is part of the same answer rather than its own state:
  // an empty list means "none" or "offline", and reading the two from
  // separate cycles could pair this artist's list with the previous
  // visit's connectivity.
  const offline = current?.offline ?? false;
  const [isApplying, setIsApplying] = useState(false);
  // Tagged like the candidates: a failure from a previous visit must
  // not sit above another artist's fresh list. (Setting it to null in
  // the effect would be a synchronous setState there, which the linter
  // refuses — and rightly, it is a cascading render.)
  const [failure, setFailure] = useState<{
    artistId: number;
    message: string;
  } | null>(null);
  const error = failure?.artistId === artistId ? failure.message : null;
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    // Offline mode refuses the candidates on purpose (they are remote
    // URLs the picker would paint), so the modal has to be able to tell
    // that apart from an artist who simply has none.
    Promise.all([
      getArtistBackdropCandidates(artistId),
      getOfflineMode().catch(() => false),
    ])
      .then(([urls, offline]) => {
        if (!cancelled) setLoaded({ artistId, urls, offline });
      })
      .catch((err) => {
        console.error("[ArtistBackdropPicker] candidates failed", err);
        if (!cancelled) setLoaded({ artistId, urls: [], offline: false });
      });
    return () => {
      cancelled = true;
    };
  }, [isOpen, artistId]);

  // One shape for the three actions: they all end the same way, and a
  // failure has to leave the modal open with the reason on screen.
  const apply = useCallback(
    async (action: () => Promise<void>, label: string) => {
      setIsApplying(true);
      setFailure(null);
      try {
        await action();
        onSuccess();
        onClose();
      } catch (err) {
        console.error(`[ArtistBackdropPicker] ${label} failed`, err);
        setFailure({ artistId, message: String(err) });
      } finally {
        setIsApplying(false);
      }
    },
    [artistId, onClose, onSuccess],
  );

  const handlePickFile = useCallback(async () => {
    const path = await pickFile(["jpg", "jpeg", "png", "webp"]);
    if (!path) return;
    await apply(() => setArtistBackgroundFromFile(artistId, path), "file");
  }, [apply, artistId]);

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="artist-backdrop-picker-title"
        className="relative w-full max-w-2xl rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated max-h-[90vh] overflow-hidden flex flex-col"
      >
        <h2
          id="artist-backdrop-picker-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-4"
        >
          {t("artistBackdropPicker.title")}
        </h2>

        <div className="flex space-x-2 border-b border-zinc-100 dark:border-zinc-800 mb-4">
          <button
            type="button"
            onClick={() => setTab("candidates")}
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              tab === "candidates"
                ? "border-emerald-500 text-emerald-600 dark:text-emerald-400"
                : "border-transparent text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
            }`}
          >
            {t("artistBackdropPicker.suggestions")}
          </button>
          <button
            type="button"
            onClick={() => setTab("file")}
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              tab === "file"
                ? "border-emerald-500 text-emerald-600 dark:text-emerald-400"
                : "border-transparent text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
            }`}
          >
            {t("library.localFile")}
          </button>
        </div>

        {error && <div className="mb-3 text-xs text-red-500 px-2">{error}</div>}

        {tab === "candidates" ? (
          <div className="flex-1 overflow-y-auto">
            {candidates === null ? (
              <div className="flex items-center justify-center py-12 text-zinc-400">
                <Loader2 size={20} className="animate-spin" />
              </div>
            ) : candidates.length === 0 ? (
              <div className="text-xs text-zinc-400 text-center py-10 px-6">
                {offline
                  ? t("artistBackdropPicker.offline")
                  : t("artistBackdropPicker.noneFound")}
              </div>
            ) : (
              <div className="grid grid-cols-2 gap-3">
                {candidates.map((url, index) => (
                  <button
                    key={url}
                    type="button"
                    disabled={isApplying}
                    // The thumbnail is the whole button and carries no
                    // text; the index is what tells the four apart.
                    aria-label={t("artistBackdropPicker.suggestionLabel", {
                      index: index + 1,
                    })}
                    onClick={() =>
                      apply(
                        () => setArtistBackgroundFromUrl(artistId, url),
                        "url",
                      )
                    }
                    className="group relative rounded-xl overflow-hidden border border-zinc-200 dark:border-zinc-700 hover:border-emerald-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {/* The hero's own proportions, so what the thumbnail
                        cuts off is what the header will cut off. */}
                    <img
                      src={url}
                      alt=""
                      loading="lazy"
                      className="w-full aspect-[16/6] object-cover"
                      style={{ objectPosition: "center 30%" }}
                    />
                  </button>
                ))}
              </div>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-center justify-center py-12 space-y-4">
            <div className="w-16 h-16 rounded-2xl bg-zinc-100 dark:bg-zinc-800 flex items-center justify-center text-zinc-400">
              <ImageIcon size={32} />
            </div>
            <button
              type="button"
              onClick={handlePickFile}
              disabled={isApplying}
              className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <FolderOpen size={16} />
              <span>{t("library.localFile")}</span>
            </button>
            <p className="text-xs text-zinc-400 text-center max-w-sm">
              {t("artistBackdropPicker.fileHint")}
            </p>
          </div>
        )}

        <div className="mt-4 flex items-center justify-between pt-3 border-t border-zinc-100 dark:border-zinc-800">
          {hasCustomBackground ? (
            <button
              type="button"
              onClick={() =>
                apply(() => clearArtistBackground(artistId), "clear")
              }
              disabled={isApplying}
              className="px-4 py-2 rounded-xl text-sm font-medium text-red-500 hover:bg-red-50 dark:hover:bg-red-950/30 transition-colors flex items-center space-x-2 disabled:opacity-50"
            >
              <Trash2 size={14} />
              <span>{t("artistBackdropPicker.removeAction")}</span>
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
