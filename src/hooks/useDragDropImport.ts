import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useLibrary } from "./useLibrary";
import {
  announceImport,
  importDropped,
  parseFolderOverlap,
} from "../lib/tauri/library";
import { playWithoutImporting } from "../lib/openToPlay";

/** Subset of `tauri-apps`' DragDropEvent we actually consume. */
type DragEventKind = "enter" | "over" | "drop" | "leave";
interface DragPayload {
  type: DragEventKind;
  paths?: string[];
  /** Physical pixels, from the webview's top-left corner. */
  position?: { x: number; y: number };
}

/** The two things a drop can do. */
export type DropTarget = "play" | "import";

/** Where the two targets are on screen, filled in by the overlay. */
export type DropTargetElements = Partial<
  Record<DropTarget, HTMLElement | null>
>;

/**
 * The target under `point` (CSS pixels), or the nearer one when the point
 * is in neither — the gap between them, the overlay's margin. Read from the
 * targets' own boxes, so a right-to-left layout that mirrors them is right
 * without knowing it.
 */
function targetAt(
  elements: DropTargetElements,
  point: { x: number; y: number },
): DropTarget | null {
  let nearest: DropTarget | null = null;
  let nearestDistance = Infinity;
  for (const target of ["play", "import"] as const) {
    const box = elements[target]?.getBoundingClientRect();
    if (!box) continue;
    if (
      point.x >= box.left &&
      point.x <= box.right &&
      point.y >= box.top &&
      point.y <= box.bottom
    ) {
      return target;
    }
    const distance = Math.hypot(
      point.x - (box.left + box.width / 2),
      point.y - (box.top + box.height / 2),
    );
    if (distance < nearestDistance) {
      nearestDistance = distance;
      nearest = target;
    }
  }
  return nearest;
}

/**
 * Wires Tauri's window-level drag-drop signal to its two targets: play the
 * files now without importing them, or add them to the library through
 * `import_dropped`, which copies them into the folder WaveFlow manages
 * under the user's music and scans it. What it did is announced for the
 * import toast.
 *
 * The overlay registers where the targets are in `targetsRef`; the hook
 * follows the pointer across them and reports the one under it, which is
 * where a drop goes.
 */
export function useDragDropImport(): {
  isDraggingOver: boolean;
  lastError: string | null;
  /** The target the pointer is over while dragging. */
  hoveredTarget: DropTarget | null;
  targetsRef: React.RefObject<DropTargetElements>;
} {
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    refresh,
  } = useLibrary();
  const { t } = useTranslation();
  const [isDraggingOver, setIsDraggingOver] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [hoveredTarget, setHoveredTarget] = useState<DropTarget | null>(null);
  const targetsRef = useRef<DropTargetElements>({});

  const handleImport = useCallback(
    async (paths: string[]) => {
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
            const lib = await createLibrary({
              name: t("onboarding.defaultLibraryName"),
            });
            libId = lib.id;
            selectLibrary(libId);
          }
        }
        const summary = await importDropped(libId, paths);
        announceImport({ kind: "dropped", summary });
        await refresh();
      } catch (err) {
        console.error("[useDragDropImport] import failed", err);
        const overlap = parseFolderOverlap(err);
        if (overlap) announceImport({ kind: "overlap", overlap });
        setLastError(String(err));
      }
    },
    [libraries, selectedLibraryId, selectLibrary, createLibrary, refresh, t],
  );

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    // Where the pointer is, in CSS pixels, when the event says.
    const pointOf = (payload: DragPayload) =>
      payload.position
        ? {
            x: payload.position.x / window.devicePixelRatio,
            y: payload.position.y / window.devicePixelRatio,
          }
        : null;
    let hovered: DropTarget | null = null;
    const hover = (next: DropTarget | null) => {
      if (next === hovered) return;
      hovered = next;
      setHoveredTarget(next);
    };

    (async () => {
      const webview = getCurrentWebview();
      const off = await webview.onDragDropEvent((event) => {
        const payload = event.payload as DragPayload;
        switch (payload.type) {
          case "enter":
          case "over": {
            setIsDraggingOver(true);
            const point = pointOf(payload);
            if (point) hover(targetAt(targetsRef.current, point));
            return;
          }
          case "leave":
            setIsDraggingOver(false);
            hover(null);
            return;
          case "drop": {
            setIsDraggingOver(false);
            const point = pointOf(payload);
            const target =
              (point && targetAt(targetsRef.current, point)) ?? hovered;
            hover(null);
            const paths = payload.paths ?? [];
            if (paths.length === 0) return;
            if (target === "play") {
              void playWithoutImporting(paths);
            } else {
              void handleImport(paths);
            }
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
  }, [handleImport]);

  return { isDraggingOver, lastError, hoveredTarget, targetsRef };
}
