import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useLibrary } from "../../hooks/useLibrary";
import {
  DROP_IMPORT_EVENT,
  type DropImportProgress,
} from "../../hooks/useDragDropImport";
import { parseFolderOverlap, scanFolder } from "../../lib/tauri/library";

/** The fields of a `scan:progress` tick the strip reads. */
interface ScanTick {
  folder_id: number;
  current: number;
  total: number;
  added: number;
  done: boolean;
  cancelled?: boolean;
}

export type OnboardingScan =
  | { kind: "idle" }
  /** The folder is registered (or a drop is being copied) and no count
   *  has arrived yet. */
  | { kind: "preparing" }
  | { kind: "running"; current: number; total: number; added: number }
  | { kind: "done"; added: number }
  /** Stopped by the user from the task bar: what it added stays. */
  | { kind: "stopped"; added: number }
  | { kind: "error"; message: string };

/**
 * The first library, built while the onboarding goes on.
 *
 * The folder is registered first, so a refusal (an overlap) comes back
 * to the folder step at once; the scan then runs in the background and
 * the rest of the journey shows its `scan:progress` ticks. A drop goes
 * through the layout's own drop handler (`useDragDropImport`, which
 * copies into the managed folder), and is followed here through
 * `DROP_IMPORT_EVENT`.
 */
export function useOnboardingScan() {
  const { t } = useTranslation();
  const { libraries, createLibrary, addFolder, refresh } = useLibrary();
  const [scan, setScan] = useState<OnboardingScan>({ kind: "idle" });
  // The folder whose ticks are ours; `null` for a drop, whose managed
  // folder is not known before its scan reports.
  const folderIdRef = useRef<number | null>(null);
  const followingRef = useRef(false);
  // The last tick said the user stopped the scan; its summary, which
  // arrives after it, does not say so and must not turn it into "done".
  const stoppedRef = useRef(false);
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    listen<ScanTick>("scan:progress", (event) => {
      if (!followingRef.current) return;
      const tick = event.payload;
      if (folderIdRef.current != null && tick.folder_id !== folderIdRef.current)
        return;
      if (tick.done) {
        stoppedRef.current = !!tick.cancelled;
        setScan({
          kind: tick.cancelled ? "stopped" : "done",
          added: tick.added,
        });
        return;
      }
      setScan({
        kind: "running",
        current: tick.current,
        total: tick.total,
        added: tick.added,
      });
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch((err) => console.error("[Onboarding] listen scan failed", err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    const onDrop = (event: Event) => {
      const progress = (event as CustomEvent<DropImportProgress>).detail;
      if (!progress) return;
      if (progress.phase === "started") {
        folderIdRef.current = null;
        followingRef.current = true;
        setScan({ kind: "preparing" });
      } else if (progress.phase === "finished") {
        followingRef.current = false;
        setScan({
          kind: progress.summary.cancelled ? "stopped" : "done",
          added: progress.summary.scan?.added ?? 0,
        });
      } else {
        followingRef.current = false;
        const overlap = parseFolderOverlap(progress.error);
        setScan({
          kind: "error",
          message: overlap
            ? t(`importNotice.overlap.${overlap.relation}`, {
                folder: overlap.folder,
              })
            : progress.error,
        });
      }
    };
    window.addEventListener(DROP_IMPORT_EVENT, onDrop);
    return () => window.removeEventListener(DROP_IMPORT_EVENT, onDrop);
  }, [t]);

  /**
   * Register `path` and start its scan without waiting for it. Resolves
   * once the folder is registered, with `null`, or with the message to
   * show when it was refused — the journey then stays on the folder step.
   */
  const startFolder = useCallback(
    async (path: string): Promise<string | null> => {
      setScan({ kind: "preparing" });
      let folderId: number;
      try {
        let libraryId = libraries[0]?.id;
        if (libraryId == null) {
          const created = await createLibrary({
            name: t("onboarding.defaultLibraryName"),
          });
          libraryId = created.id;
        }
        folderId = await addFolder(libraryId, path);
      } catch (err) {
        setScan({ kind: "idle" });
        const overlap = parseFolderOverlap(err);
        return overlap
          ? t(`importNotice.overlap.${overlap.relation}`, {
              folder: overlap.folder,
            })
          : String(err);
      }
      folderIdRef.current = folderId;
      followingRef.current = true;
      stoppedRef.current = false;
      scanFolder(folderId)
        .then((summary) => {
          if (!mountedRef.current) return;
          setScan({
            kind: stoppedRef.current ? "stopped" : "done",
            added: summary.added,
          });
        })
        .catch((err) => {
          console.error("[Onboarding] scan failed", err);
          if (!mountedRef.current) return;
          setScan({ kind: "error", message: String(err) });
        })
        .finally(() => {
          followingRef.current = false;
          // The library's counts, for the Home behind the modal and the
          // final screen; the context outlives the modal.
          void refresh();
        });
      return null;
    },
    [libraries, createLibrary, addFolder, refresh, t],
  );

  return { scan, startFolder };
}
