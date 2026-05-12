import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Loader2, Trash2, Copy } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  deleteTracks,
  findDuplicates,
  type DuplicateGroup,
} from "../../lib/tauri/duplicates";
import { formatDuration } from "../../lib/tauri/track";

interface DuplicatesModalProps {
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Spotify/foobar-style duplicate cleanup. Tracks are grouped by
 * blake3 file_hash so byte-identical copies in different folders
 * fall into the same bucket regardless of metadata. Re-encodes
 * (different bitrate / format of the same source) WON'T match —
 * that's a fingerprinting problem out of scope for this MVP.
 *
 * UX: each group lets the user pick which copy to keep (radio).
 * "Delete others" wipes every other entry from the database (the
 * audio files on disk stay — the user can clean those up via
 * the OS).
 */
export function DuplicatesModal({ isOpen, onClose }: DuplicatesModalProps) {
  const { t } = useTranslation();
  const [groups, setGroups] = useState<DuplicateGroup[]>([]);
  const [isScanning, setIsScanning] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  // Per-group "keep" selection — the track id to preserve. Defaults
  // to the first (oldest) track, which the backend orders by
  // added_at ASC.
  const [keepIds, setKeepIds] = useState<Map<string, number>>(new Map());

  const refresh = async () => {
    setIsScanning(true);
    setError(null);
    try {
      const result = await findDuplicates();
      setGroups(result);
      const next = new Map<string, number>();
      for (const g of result) {
        if (g.tracks.length > 0) next.set(g.file_hash, g.tracks[0].id);
      }
      setKeepIds(next);
    } catch (err) {
      console.error("[DuplicatesModal] scan failed", err);
      setError(String(err));
    } finally {
      setIsScanning(false);
    }
  };

  useEffect(() => {
    if (!isOpen) {
      /* eslint-disable react-hooks/set-state-in-effect */
      setGroups([]);
      setError(null);
      /* eslint-enable react-hooks/set-state-in-effect */
      return;
    }
    refresh();
  }, [isOpen]);

  const totalDuplicates = useMemo(
    () => groups.reduce((sum, g) => sum + g.tracks.length - 1, 0),
    [groups],
  );

  const handleDeleteOthers = async () => {
    const toDelete: number[] = [];
    for (const g of groups) {
      const keep = keepIds.get(g.file_hash);
      if (keep == null) continue;
      for (const t of g.tracks) {
        if (t.id !== keep) toDelete.push(t.id);
      }
    }
    if (toDelete.length === 0) return;
    setIsDeleting(true);
    try {
      await deleteTracks(toDelete);
      await refresh();
    } catch (err) {
      console.error("[DuplicatesModal] delete failed", err);
      setError(String(err));
    } finally {
      setIsDeleting(false);
    }
  };

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
        aria-labelledby="duplicates-modal-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-3xl max-h-[90vh] flex flex-col overflow-hidden animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2 min-w-0">
            <Copy size={18} className="text-amber-500" />
            <div>
              <h2 id="duplicates-modal-title" className="text-lg font-semibold">
                {t("duplicates.title")}
              </h2>
              {!isScanning && (
                <p className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("duplicates.summary", {
                    groups: groups.length,
                    duplicates: totalDuplicates,
                  })}
                </p>
              )}
            </div>
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

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-6 space-y-4 min-h-75">
          {isScanning ? (
            <div className="flex flex-col items-center justify-center py-12 text-zinc-500">
              <Loader2 size={28} className="animate-spin mb-3" />
              <p className="text-sm">{t("duplicates.scanning")}</p>
            </div>
          ) : error ? (
            <div className="text-sm text-red-500">{error}</div>
          ) : groups.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-zinc-500">
              <Copy size={40} className="mb-3 opacity-40" />
              <p className="text-sm font-medium">{t("duplicates.empty")}</p>
              <p className="text-xs mt-1">{t("duplicates.emptyHint")}</p>
            </div>
          ) : (
            groups.map((g) => (
              <div
                key={g.file_hash}
                className="rounded-xl border border-zinc-200 dark:border-zinc-800 overflow-hidden"
              >
                <div className="px-4 py-2 bg-zinc-50 dark:bg-zinc-800/40 text-xs text-zinc-500 dark:text-zinc-400 font-mono truncate">
                  {g.file_hash}
                </div>
                <ul className="divide-y divide-zinc-100 dark:divide-zinc-800">
                  {g.tracks.map((track) => {
                    const isKept = keepIds.get(g.file_hash) === track.id;
                    return (
                      <li
                        key={track.id}
                        className={`px-4 py-3 flex items-start gap-3 ${
                          isKept
                            ? "bg-emerald-50 dark:bg-emerald-950/20"
                            : "hover:bg-zinc-50 dark:hover:bg-zinc-800/30"
                        }`}
                      >
                        <input
                          type="radio"
                          name={`keep-${g.file_hash}`}
                          checked={isKept}
                          onChange={() => {
                            setKeepIds((prev) =>
                              new Map(prev).set(g.file_hash, track.id),
                            );
                          }}
                          className="mt-1 accent-emerald-500"
                          aria-label={t("duplicates.keepAria", {
                            title: track.title,
                          })}
                        />
                        <div className="flex-1 min-w-0">
                          <div className="text-sm font-medium truncate">
                            {track.title}
                            {track.artist_name && (
                              <span className="font-normal text-zinc-500 dark:text-zinc-400">
                                {" — "}
                                {track.artist_name}
                              </span>
                            )}
                          </div>
                          <div className="text-xs text-zinc-500 dark:text-zinc-400 truncate font-mono">
                            {track.file_path}
                          </div>
                          <div className="text-xs text-zinc-400 mt-1 flex gap-3">
                            <span>{formatDuration(track.duration_ms)}</span>
                            <span>{formatBytes(track.file_size)}</span>
                            {track.bitrate && (
                              <span>
                                {Math.round(track.bitrate / 1000)} kbps
                              </span>
                            )}
                          </div>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              </div>
            ))
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between gap-3 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <button
            type="button"
            onClick={refresh}
            disabled={isScanning || isDeleting}
            className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
          >
            {t("duplicates.rescan")}
          </button>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              {t("common.close")}
            </button>
            <button
              type="button"
              onClick={handleDeleteOthers}
              disabled={isDeleting || groups.length === 0}
              className="px-5 py-2 rounded-full bg-red-500 text-white text-sm font-medium hover:bg-red-600 disabled:opacity-50 transition-opacity flex items-center gap-2"
            >
              {isDeleting ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <Trash2 size={14} />
              )}
              {t("duplicates.deleteOthers", { count: totalDuplicates })}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
