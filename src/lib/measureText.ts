/**
 * Measure text without touching the DOM (#588).
 *
 * "Fit this column to its content" has to look at every row, and the
 * rows are virtualised — the ones outside the viewport have no computed
 * layout at all. Measuring the DOM would therefore size the column to
 * whatever happened to be on screen when the user double-clicked, which
 * is a different answer at every scroll position.
 *
 * A canvas gives the same answer regardless of scroll, and it does it
 * without a layout pass per row.
 */

let context: CanvasRenderingContext2D | null | undefined;

function ctx(): CanvasRenderingContext2D | null {
  // `undefined` means "not tried yet", `null` means "tried and there is
  // no canvas here" — a distinction worth keeping, so a headless or
  // locked-down environment is asked once rather than on every measure.
  if (context === undefined) {
    try {
      context = document.createElement("canvas").getContext("2d");
    } catch {
      context = null;
    }
  }
  return context;
}

/**
 * The font the measurement has to use.
 *
 * Read off a live element rather than hardcoded: the skins swap the
 * body family (Playfair, Space Grotesk, DM Sans), and measuring Inter
 * while the page renders Playfair gives a width that is wrong by up to
 * a fifth.
 */
export function fontOf(element: Element | null): string {
  if (!element) return "14px system-ui, sans-serif";
  const style = window.getComputedStyle(element);
  const size = style.fontSize || "14px";
  const family = style.fontFamily || "system-ui, sans-serif";
  const weight = style.fontWeight || "400";
  return `${weight} ${size} ${family}`;
}

/** Width of one string, in CSS pixels, or `null` when unmeasurable. */
export function measureText(text: string, font: string): number | null {
  const c = ctx();
  if (!c) return null;
  c.font = font;
  return c.measureText(text).width;
}

/**
 * The width a column needs to show its content without truncating.
 *
 * `headerLabel` is measured too, and that is not a detail: a column
 * fitted to short content shows a truncated *title*, which reads as the
 * fit having failed.
 *
 * `headerFont` is separate because the header is uppercase, bold and
 * letter-spaced where the cells are not — measuring the label in the
 * cell font under-measures it.
 */
export function fitWidth(options: {
  values: string[];
  headerLabel: string;
  cellFont: string;
  headerFont: string;
  /** Cell padding plus whatever the header adds (a sort caret, a resize
   *  handle) — added to the widest measurement. */
  padding: number;
  min: number;
  max: number;
}): number | null {
  const { values, headerLabel, cellFont, headerFont, padding, min, max } =
    options;
  const header = measureText(headerLabel, headerFont);
  if (header === null) return null;
  let widest = header;
  for (const value of values) {
    if (!value) continue;
    const width = measureText(value, cellFont);
    if (width !== null && width > widest) widest = width;
  }
  return Math.min(max, Math.max(min, Math.ceil(widest + padding)));
}
