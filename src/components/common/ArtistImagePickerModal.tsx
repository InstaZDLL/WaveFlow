import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  FolderOpen,
  ImageIcon,
  Loader2,
  Search,
  Trash2,
  User,
} from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  clearArtistArtwork,
  searchArtistsDeezer,
  setArtistArtworkFromDeezer,
  setArtistArtworkFromFile,
  type DeezerArtistLite,
} from "../../lib/tauri/deezer";
import { pickFile } from "../../lib/tauri/dialog";

interface ArtistImagePickerModalProps {
  artistId: number;
  artistName: string;
  /** Whether the artist currently has a non-null artwork_id. Drives the
   *  visibility of the "remove image" action. */
  hasArtwork: boolean;
  isOpen: boolean;
  onClose: () => void;
  onSuccess: () => void;
}

type Tab = "deezer" | "file";

/**
 * Mirror of [`CoverPickerModal`](./CoverPickerModal.tsx) but bound to the
 * `artist` table instead of `album`. Three actions, in order of expected
 * frequency: search Deezer, upload a local file, or clear the current
 * image so the resolution chain falls back to the Deezer cache.
 */
export function ArtistImagePickerModal({
  artistId,
  artistName,
  hasArtwork,
  isOpen,
  onClose,
  onSuccess,
}: ArtistImagePickerModalProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("deezer");
  const [query, setQuery] = useState(artistName);
  const [results, setResults] = useState<DeezerArtistLite[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [isApplying, setIsApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);
  // Monotonic counter so a slow earlier response can't overwrite the
  // results of a newer query — the user typing fast was producing
  // visible "old results flicker" when the network was slow.
  const requestIdRef = useRef(0);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setQuery(artistName);
      setResults([]);
      setError(null);
      setTab("deezer");
    }
  }, [isOpen, artistName]);

  useEffect(() => {
    if (!isOpen || tab !== "deezer") {
      // Abandoning the search — bump the request id so any in-flight
      // Deezer call's `.then()` is treated as stale and skipped.
      requestIdRef.current++;
      return;
    }
    if (debounceRef.current != null) {
      window.clearTimeout(debounceRef.current);
    }
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      requestIdRef.current++;
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setResults([]);
      setIsSearching(false);
      setError(null);
      return;
    }
    debounceRef.current = window.setTimeout(() => {
      const requestId = ++requestIdRef.current;
      setIsSearching(true);
      setError(null);
      searchArtistsDeezer(trimmed)
        .then((res) => {
          if (requestId !== requestIdRef.current) return;
          setResults(res);
        })
        .catch((err) => {
          if (requestId !== requestIdRef.current) return;
          console.error("[ArtistImagePickerModal] search failed", err);
          setError(String(err));
        })
        .finally(() => {
          if (requestId !== requestIdRef.current) return;
          setIsSearching(false);
        });
    }, 300);
    return () => {
      if (debounceRef.current != null) {
        window.clearTimeout(debounceRef.current);
      }
    };
  }, [query, tab, isOpen]);

  if (!isOpen) return null;

  const handlePickDeezer = async (hit: DeezerArtistLite) => {
    if (isApplying) return;
    setIsApplying(true);
    setError(null);
    try {
      await setArtistArtworkFromDeezer(artistId, hit.deezer_id);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[ArtistImagePickerModal] set deezer image failed", err);
      setError(String(err));
    } finally {
      setIsApplying(false);
    }
  };

  const handlePickFile = async () => {
    if (isApplying) return;
    try {
      const path = await pickFile(
        ["jpg", "jpeg", "png", "webp"],
        t("artistImagePicker.title"),
      );
      if (!path) return;
      setIsApplying(true);
      setError(null);
      await setArtistArtworkFromFile(artistId, path);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[ArtistImagePickerModal] set file image failed", err);
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
      await clearArtistArtwork(artistId);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[ArtistImagePickerModal] clear image failed", err);
      setError(String(err));
    } finally {
      setIsApplying(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="artist-image-picker-title"
        className="relative w-full max-w-2xl rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in max-h-[90vh] overflow-hidden flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        <h2
          id="artist-image-picker-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-4"
        >
          {t("artistImagePicker.title")}
        </h2>

        <div className="flex space-x-2 border-b border-zinc-100 dark:border-zinc-800 mb-4">
          <button
            type="button"
            onClick={() => setTab("deezer")}
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              tab === "deezer"
                ? "border-emerald-500 text-emerald-600 dark:text-emerald-400"
                : "border-transparent text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
            }`}
          >
            {t("library.searchDeezer")}
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

        {tab === "deezer" ? (
          <div className="flex-1 overflow-hidden flex flex-col">
            <div className="relative mb-4">
              <Search
                size={16}
                className="absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
              />
              <input
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("library.searchDeezer")}
                autoFocus
                className="w-full pl-9 pr-4 py-2.5 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-sm text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
              />
              {isSearching && (
                <Loader2
                  size={16}
                  className="absolute right-3 top-1/2 -translate-y-1/2 text-zinc-400 animate-spin"
                />
              )}
            </div>
            <div className="flex-1 overflow-y-auto">
              {results.length === 0 ? (
                <div className="text-xs text-zinc-400 text-center py-8">
                  {query.trim().length < 2
                    ? t("library.searchDeezer")
                    : isSearching
                      ? "..."
                      : ""}
                </div>
              ) : (
                <div className="grid grid-cols-3 gap-3">
                  {results.map((hit) => (
                    <button
                      key={hit.deezer_id}
                      type="button"
                      disabled={isApplying}
                      onClick={() => handlePickDeezer(hit)}
                      className="group flex flex-col items-center text-center rounded-xl p-2 hover:bg-zinc-50 dark:hover:bg-zinc-800/40 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <div className="w-24 h-24 rounded-full overflow-hidden bg-zinc-100 dark:bg-zinc-800 mb-2 shadow-sm">
                        {hit.picture_url ? (
                          <img
                            src={hit.picture_url}
                            alt={hit.name}
                            loading="lazy"
                            className="w-full h-full object-cover"
                          />
                        ) : (
                          <div className="w-full h-full flex items-center justify-center text-zinc-400">
                            <User size={32} />
                          </div>
                        )}
                      </div>
                      <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate w-full">
                        {hit.name}
                      </div>
                      {hit.nb_fan != null && (
                        <div className="text-xs text-zinc-500">
                          {t("artistImagePicker.fansCount", {
                            count: hit.nb_fan,
                            display:
                              hit.nb_fan >= 1_000_000
                                ? `${(hit.nb_fan / 1_000_000).toFixed(1)}M`
                                : hit.nb_fan >= 1_000
                                  ? `${(hit.nb_fan / 1_000).toFixed(0)}K`
                                  : String(hit.nb_fan),
                          })}
                        </div>
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
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
          </div>
        )}

        <div className="mt-4 flex items-center justify-between pt-3 border-t border-zinc-100 dark:border-zinc-800">
          {hasArtwork ? (
            <button
              type="button"
              onClick={handleClear}
              disabled={isApplying}
              className="px-4 py-2 rounded-xl text-sm font-medium text-red-500 hover:bg-red-50 dark:hover:bg-red-950/30 transition-colors flex items-center space-x-2 disabled:opacity-50"
            >
              <Trash2 size={14} />
              <span>{t("artistImagePicker.removeAction")}</span>
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
      </div>
    </div>
  );
}
