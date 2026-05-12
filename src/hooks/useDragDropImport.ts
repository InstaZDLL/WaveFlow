import { useCallback, useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useLibrary } from "./useLibrary";
import { importPaths } from "../lib/tauri/library";

/** Subset of `tauri-apps`' DragDropEvent we actually consume. */
type DragEventKind = "enter" | "over" | "drop" | "leave";
interface DragPayload {
  type: DragEventKind;
  paths?: string[];
}

/**
 * Wires Tauri's window-level drag-drop signal into the existing
 * library-import flow. Drops route through `import_paths` which
 * accepts a mix of folders and audio files (files contribute their
 * parent directory).
 *
 * Returns the active drag state so the caller can render an
 * overlay with a drop hint.
 */
export function useDragDropImport(): {
  isDraggingOver: boolean;
  isImporting: boolean;
  lastError: string | null;
} {
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    refresh,
  } = useLibrary();
  const [isDraggingOver, setIsDraggingOver] = useState(false);
  const [isImporting, setIsImporting] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);

  const handleDrop = useCallback(
    async (paths: string[]) => {
      setIsImporting(true);
      setLastError(null);
      try {
        // Resolve a target library — auto-create one if the profile
        // has none, matching the existing pickFolder import flow.
        let libId = selectedLibraryId;
        if (libId == null) {
          if (libraries.length > 0) {
            libId = libraries[0].id;
            selectLibrary(libId);
          } else {
            const lib = await createLibrary({ name: "Ma musique" });
            libId = lib.id;
            selectLibrary(libId);
          }
        }
        await importPaths(libId, paths);
        await refresh();
      } catch (err) {
        console.error("[useDragDropImport] import failed", err);
        setLastError(String(err));
      } finally {
        setIsImporting(false);
      }
    },
    [libraries, selectedLibraryId, selectLibrary, createLibrary, refresh],
  );

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    (async () => {
      const webview = getCurrentWebview();
      const off = await webview.onDragDropEvent((event) => {
        const payload = event.payload as DragPayload;
        switch (payload.type) {
          case "enter":
          case "over":
            setIsDraggingOver(true);
            return;
          case "leave":
            setIsDraggingOver(false);
            return;
          case "drop": {
            setIsDraggingOver(false);
            const paths = payload.paths ?? [];
            if (paths.length === 0) return;
            handleDrop(paths);
            return;
          }
        }
      });
      if (cancelled) off();
      else unlisten = off;
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [handleDrop]);

  return { isDraggingOver, isImporting, lastError };
}
