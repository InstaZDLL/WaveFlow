import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ImageIcon, FolderOpen, Search, Loader2 } from "lucide-react";
import {
  searchAlbumsDeezer,
  setAlbumArtworkFromDeezer,
  setAlbumArtworkFromFile,
  type DeezerAlbumLite,
} from "../../lib/tauri/deezer";
import { pickFile } from "../../lib/tauri/dialog";

interface CoverPickerModalProps {
  albumId: number;
  initialQuery?: string;
  isOpen: boolean;
  onClose: () => void;
  onSuccess: () => void;
}

type Tab = "deezer" | "file";

export function CoverPickerModal({
  albumId,
  initialQuery,
  isOpen,
  onClose,
  onSuccess,
}: CoverPickerModalProps) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("deezer");
  const [query, setQuery] = useState(initialQuery ?? "");
  const [results, setResults] = useState<DeezerAlbumLite[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [isApplying, setIsApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);

  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setQuery(initialQuery ?? "");
      setResults([]);
      setError(null);
      setTab("deezer");
    }
  }, [isOpen, initialQuery]);

  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [isOpen, onClose]);

  useEffect(() => {
    if (!isOpen || tab !== "deezer") return;
    if (debounceRef.current != null) {
      window.clearTimeout(debounceRef.current);
    }
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setResults([]);
      return;
    }
    debounceRef.current = window.setTimeout(() => {
      setIsSearching(true);
      setError(null);
      searchAlbumsDeezer(trimmed)
        .then((res) => setResults(res))
        .catch((err) => {
          console.error("[CoverPickerModal] search failed", err);
          setError(String(err));
        })
        .finally(() => setIsSearching(false));
    }, 300);
    return () => {
      if (debounceRef.current != null) {
        window.clearTimeout(debounceRef.current);
      }
    };
  }, [query, tab, isOpen]);

  if (!isOpen) return null;

  const handlePickDeezer = async (album: DeezerAlbumLite) => {
    if (isApplying) return;
    setIsApplying(true);
    setError(null);
    try {
      await setAlbumArtworkFromDeezer(albumId, album.deezer_id);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[CoverPickerModal] set deezer cover failed", err);
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
        t("library.changeCover"),
      );
      if (!path) return;
      setIsApplying(true);
      setError(null);
      await setAlbumArtworkFromFile(albumId, path);
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[CoverPickerModal] set file cover failed", err);
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
        className="relative w-full max-w-2xl rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in max-h-[90vh] overflow-hidden flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-lg font-bold text-zinc-900 dark:text-white mb-4">
          {t("library.changeCover")}
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

        {error && (
          <div className="mb-3 text-xs text-red-500 px-2">{error}</div>
        )}

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
                  {results.map((album) => (
                    <button
                      key={album.deezer_id}
                      type="button"
                      disabled={isApplying}
                      onClick={() => handlePickDeezer(album)}
                      className="group flex flex-col text-left rounded-xl p-2 hover:bg-zinc-50 dark:hover:bg-zinc-800/40 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <div className="aspect-square w-full rounded-lg overflow-hidden bg-zinc-100 dark:bg-zinc-800 mb-2">
                        {album.cover_url ? (
                          <img
                            src={album.cover_url}
                            alt={album.title}
                            loading="lazy"
                            className="w-full h-full object-cover"
                          />
                        ) : (
                          <div className="w-full h-full flex items-center justify-center text-zinc-400">
                            <ImageIcon size={32} />
                          </div>
                        )}
                      </div>
                      <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                        {album.title}
                      </div>
                      <div className="text-xs text-zinc-500 truncate">
                        {album.artist}
                      </div>
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

        <div className="mt-4 flex items-center justify-end pt-3 border-t border-zinc-100 dark:border-zinc-800">
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
