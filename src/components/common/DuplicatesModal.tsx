import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Loader2, Trash2, Copy } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  findDuplicates,
  moveTracksToTrash,
  type DuplicateGroup,
  type TrashGroup,
  type TrashReport,
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
 * "Move to the trash" lists the files it is about to move and asks
 * first, then sends every other copy to the system's trash and drops
 * the rows of those that left (#878). What happened stays on screen,
 * refusals included, rather than in a toast that fades.
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
  // The copies the user is being asked about, between the click and
  // the confirmation; and what the last move did.
  const [pending, setPending] = useState<{
    copies: { id: number; path: string }[];
    groups: TrashGroup[];
  } | null>(null);
  const [report, setReport] = useState<TrashReport | null>(null);
  // The footer swaps buttons when the confirmation opens and closes;
  // without moving focus it would fall back to the document.
  // After a confirm the trash button stays disabled through the move
  // and the rescan, so the focus waits for it; if it never comes back
  // (nothing left to trash), Close takes it.
  const cancelRef = useRef<HTMLButtonElement>(null);
  const trashRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const restoreFocus = useRef(false);
  useEffect(() => {
    if (pending) {
      cancelRef.current?.focus();
      restoreFocus.current = true;
      return;
    }
    if (!restoreFocus.current || isDeleting || isScanning) return;
    restoreFocus.current = false;
    if (trashRef.current && !trashRef.current.disabled) {
      trashRef.current.focus();
    } else {
      closeRef.current?.focus();
    }
  }, [pending, isDeleting, isScanning]);
  // Two independent staleness tokens:
  //   - `scanSeqRef` is bumped by every `refresh()` invocation so two
  //     overlapping scans don't race on the setters.
  //   - `sessionRef` is bumped only when the modal closes, so async
  //     handlers (`handleDeleteOthers`) can detect a mid-flight close
  //     without being thrown off by `refresh()` bumping a shared
  //     counter.
  // Splitting them resolves a deadlock between two CodeRabbit findings:
  // (a) the delete spinner must clear on the happy path, and (b) the
  // Delete-others button must stay disabled across the post-delete
  // rescan so a second click can't fire against stale `groups`.
  const scanSeqRef = useRef(0);
  const sessionRef = useRef(0);

  const refresh = useCallback(async () => {
    const seq = ++scanSeqRef.current;
    setIsScanning(true);
    setError(null);
    try {
      const result = await findDuplicates();
      if (seq !== scanSeqRef.current) return;
      setGroups(result);
      const next = new Map<string, number>();
      for (const g of result) {
        if (g.tracks.length > 0) next.set(g.file_hash, g.tracks[0].id);
      }
      setKeepIds(next);
    } catch (err) {
      if (seq !== scanSeqRef.current) return;
      console.error("[DuplicatesModal] scan failed", err);
      setError(String(err));
    } finally {
      if (seq === scanSeqRef.current) {
        setIsScanning(false);
      }
    }
  }, []);

  useEffect(() => {
    if (!isOpen) {
      // Invalidate any in-flight scan AND any in-flight delete — their
      // setters short-circuit when their captured token no longer
      // matches the latest ref, so the closed modal can't end up with
      // stale `groups` / `keepIds` / spinner state after re-open.
      scanSeqRef.current++;
      sessionRef.current++;
      /* eslint-disable react-hooks/set-state-in-effect */
      setGroups([]);
      setError(null);
      setIsScanning(false);
      // Both spinners need clearing here: the guarded finally in
      // `refresh()` / `handleDeleteOthers` short-circuits on staleness
      // and can't be relied on once we've invalidated the tokens.
      setIsDeleting(false);
      setPending(null);
      setReport(null);
      /* eslint-enable react-hooks/set-state-in-effect */
      return;
    }
    refresh();
  }, [isOpen, refresh]);

  const totalDuplicates = useMemo(
    () => groups.reduce((sum, g) => sum + g.tracks.length - 1, 0),
    [groups],
  );

  const askToTrash = () => {
    const copies: { id: number; path: string }[] = [];
    const trashGroups: TrashGroup[] = [];
    for (const g of groups) {
      const keep = keepIds.get(g.file_hash);
      if (keep == null) continue;
      const others = g.tracks.filter((t) => t.id !== keep);
      if (others.length === 0) continue;
      for (const t of others) copies.push({ id: t.id, path: t.file_path });
      trashGroups.push({ keep_id: keep, trash_ids: others.map((t) => t.id) });
    }
    if (copies.length === 0) return;
    setReport(null);
    setPending({ copies, groups: trashGroups });
  };

  const handleDeleteOthers = async () => {
    const toTrash = pending?.groups ?? [];
    setPending(null);
    if (toTrash.length === 0) return;
    // Snapshot the modal session — only the `!isOpen` reset bumps it,
    // so the guard remains valid across the embedded `refresh()` call
    // (which bumps a *different* token, `scanSeqRef`). That lets the
    // button stay disabled across the whole delete-then-rescan flow
    // while still clearing correctly on success.
    const session = sessionRef.current;
    setIsDeleting(true);
    try {
      const result = await moveTracksToTrash(toTrash);
      if (session !== sessionRef.current) return;
      setReport(result);
      // Run the rescan with the spinner still on — clicking Delete
      // mid-rescan would target the pre-delete `groups` snapshot and
      // hit the backend with already-removed track ids.
      await refresh();
    } catch (err) {
      if (session !== sessionRef.current) return;
      console.error("[DuplicatesModal] delete failed", err);
      setError(String(err));
    } finally {
      if (session === sessionRef.current) {
        setIsDeleting(false);
      }
    }
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="duplicates-modal-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-3xl max-h-[90vh] flex flex-col overflow-hidden"
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
          {report && !pending && (
            <div
              role="status"
              className="rounded-xl border border-zinc-200 dark:border-zinc-800 px-4 py-3 text-sm space-y-2"
            >
              <p className="font-medium">
                {t("duplicates.trashed", { count: report.trashed })}
              </p>
              {report.failed.length > 0 && (
                <div className="space-y-1">
                  <p className="text-red-500">
                    {t("duplicates.notTrashed", {
                      count: report.failed.length,
                    })}
                  </p>
                  <ul className="text-xs text-zinc-500 dark:text-zinc-400 space-y-1">
                    {report.failed.map((f) => (
                      <li key={f.track_id} className="font-mono break-all">
                        {f.file_path}
                        {" — "}
                        {f.reason.kind === "playing"
                          ? t("duplicates.reasonPlaying")
                          : f.reason.kind === "kept_copy_gone"
                            ? t("duplicates.reasonKeptCopyGone")
                            : f.reason.kind === "row_kept"
                              ? t("duplicates.reasonRowKept", {
                                  message: f.reason.message,
                                })
                              : f.reason.message}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </div>
          )}
          {pending ? (
            <div className="space-y-3">
              <h3 className="text-base font-semibold">
                {t("duplicates.confirmTitle")}
              </h3>
              <p className="text-sm text-zinc-600 dark:text-zinc-300">
                {t("duplicates.confirmBody")}
              </p>
              <ul className="rounded-xl border border-zinc-200 dark:border-zinc-800 divide-y divide-zinc-100 dark:divide-zinc-800 text-xs font-mono text-zinc-600 dark:text-zinc-300">
                {pending.copies.map((c) => (
                  <li key={c.id} className="px-4 py-2 break-all">
                    {c.path}
                  </li>
                ))}
              </ul>
            </div>
          ) : isScanning ? (
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
        {pending ? (
          <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
            <button
              ref={cancelRef}
              type="button"
              onClick={() => setPending(null)}
              className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              onClick={handleDeleteOthers}
              className="px-5 py-2 rounded-full bg-red-500 text-white text-sm font-medium hover:bg-red-600 transition-colors flex items-center gap-2"
            >
              <Trash2 size={14} />
              {t("duplicates.confirmAction")}
            </button>
          </div>
        ) : (
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
                ref={closeRef}
                type="button"
                onClick={onClose}
                className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
              >
                {t("common.close")}
              </button>
              <button
                ref={trashRef}
                type="button"
                onClick={askToTrash}
                disabled={isDeleting || isScanning || groups.length === 0}
                className="px-5 py-2 rounded-full bg-red-500 text-white text-sm font-medium hover:bg-red-600 disabled:opacity-50 transition-opacity flex items-center gap-2"
              >
                {isDeleting ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <Trash2 size={14} />
                )}
                {t("duplicates.trashOthers", { count: totalDuplicates })}
              </button>
            </div>
          </div>
        )}
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
