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

/** Everything a canvas measurement needs to reproduce a rendered run
 *  of text. */
export interface TextMetricsStyle {
  /** A `CanvasRenderingContext2D.font` shorthand. */
  font: string;
  /** Extra pixels per character. Canvas has no `letterSpacing` on every
   *  engine we ship on, so it is added by hand. */
  letterSpacing: number;
  /** What `text-transform` does to the string before it is drawn. */
  transform: "none" | "uppercase" | "lowercase" | "capitalize";
}

const FALLBACK: TextMetricsStyle = {
  font: "14px system-ui, sans-serif",
  letterSpacing: 0,
  transform: "none",
};

/**
 * The typography a measurement has to reproduce.
 *
 * Read off a live element rather than hardcoded, and not only for the
 * family: the skins swap the body face (Playfair, Space Grotesk, DM
 * Sans), and the table header is uppercase and letter-spaced where the
 * cells are neither. Measuring a header label as lower-case, unspaced
 * text under-measures it by a wide margin, and the column it sizes then
 * truncates its own title.
 */
export function styleOf(element: Element | null): TextMetricsStyle {
  if (!element) return FALLBACK;
  const style = window.getComputedStyle(element);
  const size = style.fontSize || "14px";
  const family = style.fontFamily || "system-ui, sans-serif";
  const weight = style.fontWeight || "400";
  const spacing = Number.parseFloat(style.letterSpacing);
  const transform = style.textTransform;
  return {
    font: `${weight} ${size} ${family}`,
    // `normal` parses to NaN, which is the common case.
    letterSpacing: Number.isFinite(spacing) ? spacing : 0,
    transform:
      transform === "uppercase" ||
      transform === "lowercase" ||
      transform === "capitalize"
        ? transform
        : "none",
  };
}

// Plain `toUpperCase`, not the locale-aware pair, even though CSS
// `text-transform` does honour the element's language (Turkish `i` to
// `İ`, and `I` back to `ı`). The difference here is a measurement of
// width, and those glyphs carry the same advance as the ASCII ones --
// the dot is above the x-height, the missing dot changes nothing
// horizontally. Threading the document language into every
// `TextMetricsStyle` would widen the contract for a column fit nobody
// could see move.
function applyTransform(text: string, style: TextMetricsStyle): string {
  switch (style.transform) {
    case "uppercase":
      return text.toUpperCase();
    case "lowercase":
      return text.toLowerCase();
    case "capitalize":
      return text.replace(/\b\p{L}/gu, (c) => c.toUpperCase());
    default:
      return text;
  }
}

/** Width of one string, in CSS pixels, or `null` when unmeasurable. */
export function measureText(
  text: string,
  style: TextMetricsStyle,
): number | null {
  const c = ctx();
  if (!c) return null;
  c.font = style.font;
  const drawn = applyTransform(text, style);
  // Every character, the last one included — not `n - 1` gaps. CSS
  // letter-spacing is specified as space added *after* each character,
  // and Chromium and WebKit both include the trailing one in the
  // element's content width. Measuring `n - 1` would come out narrower
  // than the box the browser actually lays out, and a column fitted
  // from it would truncate by exactly that much.
  return c.measureText(drawn).width + drawn.length * style.letterSpacing;
}

/**
 * The width a column needs to show its content without truncating.
 *
 * `headerLabel` is measured too, and that is not a detail: a column
 * fitted to short content shows a truncated *title*, which reads as the
 * fit having failed. It is measured in the header's own style, which is
 * uppercase, bold and letter-spaced where the cells are none of those.
 */
export function fitWidth(options: {
  values: string[];
  headerLabel: string;
  cell: TextMetricsStyle;
  header: TextMetricsStyle;
  /** Cell padding plus whatever the header adds (a sort caret, a resize
   *  handle) — added to the widest measurement. */
  padding: number;
  min: number;
  max: number;
}): number | null {
  const { values, headerLabel, cell, header, padding, min, max } = options;
  const headerWidth = measureText(headerLabel, header);
  if (headerWidth === null) return null;
  let widest = headerWidth;
  for (const value of values) {
    if (!value) continue;
    const width = measureText(value, cell);
    if (width !== null && width > widest) widest = width;
  }
  return Math.min(max, Math.max(min, Math.ceil(widest + padding)));
}
