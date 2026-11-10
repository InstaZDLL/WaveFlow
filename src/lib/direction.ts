/**
 * The interface's reading direction, for the few places that place
 * things in JavaScript.
 *
 * Markup follows the direction on its own through logical properties
 * (`ms-*`, `start-*`, `text-start`…), but a popover positioned with a
 * computed `left` does not: it has to be told which side the reading
 * direction ends on. `i18n/index.ts` sets `dir` on `<html>` with the
 * language, so the answer is read when a popover is placed and a
 * language change applies from the next one opened.
 */
export function interfaceIsRtl(): boolean {
  return document.documentElement.dir === "rtl";
}

/** The horizontal edges of an anchor, viewport-relative (a `DOMRect` fits). */
export interface AnchorEdges {
  left: number;
  right: number;
}

/** Keep a box `width` wide, starting at `left`, `margin` inside the window. */
export function clampLeft(left: number, width: number, margin: number): number {
  const max = Math.max(margin, window.innerWidth - width - margin);
  return Math.min(Math.max(margin, left), max);
}

/**
 * Left edge of a popover opened at a point (a right-click): it extends
 * toward the reading direction's end, to the right of the point in a
 * left-to-right language and to its left in a right-to-left one, and is
 * pushed back inside the window when that side has no room.
 */
export function leftAtPoint(x: number, width: number, margin: number): number {
  return clampLeft(interfaceIsRtl() ? x - width : x, width, margin);
}

/**
 * Left edge of a popover opened beside an anchor (a submenu beside its
 * item): after the anchor in reading order, before it when that side
 * has no room, and inside the window either way.
 */
export function leftBeside(
  anchor: AnchorEdges,
  width: number,
  margin: number,
): number {
  const after = anchor.right;
  const before = anchor.left - width;
  const fitsAfter = after + width + margin <= window.innerWidth;
  const fitsBefore = before >= margin;
  const left = interfaceIsRtl()
    ? fitsBefore
      ? before
      : after
    : fitsAfter
      ? after
      : before;
  return clampLeft(left, width, margin);
}

/**
 * Left edge of a dropdown under or over its trigger, lined up with one
 * of the trigger's edges: `start`, where reading begins (its left edge
 * in a left-to-right language), or `end`. Inside the window either way.
 */
export function leftAligned(
  anchor: AnchorEdges,
  width: number,
  margin: number,
  align: "start" | "end",
): number {
  const startsLeft = (align === "start") !== interfaceIsRtl();
  return clampLeft(
    startsLeft ? anchor.left : anchor.right - width,
    width,
    margin,
  );
}

/**
 * The arrow key that moves forward along a row laid out in `el`:
 * ArrowRight, or ArrowLeft where the row runs right to left. Read from
 * the computed direction rather than the interface's, so a row kept left
 * to right on purpose (a tempo axis) keeps its keys in Arabic.
 */
export function forwardArrowKey(el: Element): "ArrowLeft" | "ArrowRight" {
  return getComputedStyle(el).direction === "rtl" ? "ArrowLeft" : "ArrowRight";
}

/**
 * The `transform-origin` of a popover anchored by its end edge (`end-0`),
 * so it grows out of the corner nearest its trigger: `top right` /
 * `bottom right`, or the left corner in a right-to-left language. CSS
 * has no logical keyword for it.
 */
export function endCornerOrigin(edge: "top" | "bottom"): string {
  return `${edge} ${interfaceIsRtl() ? "left" : "right"}`;
}
