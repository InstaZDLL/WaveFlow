import type { KeyboardEvent as ReactKeyboardEvent } from "react";

import type { ContextMenuPoint } from "../components/common/ContextMenu";

/**
 * Shared keyboard contract for opening a context menu.
 *
 * Right-click has no keyboard equivalent unless one is wired by hand, so
 * every surface that shows a track context menu answers the same two
 * keys — the ones every desktop environment already trains people on:
 *
 * - **Menu** (the "application" key, `event.key === "ContextMenu"`)
 * - **Shift+F10**, its universal stand-in on keyboards without that key
 *   (laptops, most Mac layouts, TKL boards)
 *
 * Kept in one module rather than repeated per view: issue #436 exists
 * because fixing this on a single surface would have produced exactly
 * the inconsistency it set out to remove.
 */
export function isContextMenuKey(event: ReactKeyboardEvent): boolean {
  return event.key === "ContextMenu" || (event.shiftKey && event.key === "F10");
}

/**
 * Where to anchor a keyboard-opened menu.
 *
 * A mouse-opened menu lands at the pointer; a keyboard one has no
 * pointer, so it anchors to the focused row the way the OS does — just
 * inside its bottom-left corner, so the menu reads as belonging to that
 * row and never covers the row it acts on.
 *
 * Coordinates are viewport-relative because `ContextMenu` is
 * `position: fixed`, and it does its own flipping when the menu would
 * overflow the viewport, so a row near the bottom edge still works.
 */
export function menuAnchorForElement(element: HTMLElement): ContextMenuPoint {
  const rect = element.getBoundingClientRect();
  // Inset a few pixels so the menu overlaps the row's edge rather than
  // floating detached from it.
  return { x: rect.left + 8, y: rect.bottom - 4 };
}
