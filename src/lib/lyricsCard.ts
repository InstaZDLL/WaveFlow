import { dominantColor } from "./dominantColor";
import type { LyricsLine, LyricsPayload } from "./tauri/lyrics";
import { drawBlurred } from "./precomputedBlur";
import {
  canvasToPng,
  cardEyebrow,
  cardFont,
  darken,
  drawCard,
  drawRoundedImage,
  tryLoadImage,
  truncate,
} from "./cardCanvas";

/**
 * A 1080×1080 share card for a few lines of a song's lyrics: the lines
 * large, the cover and the track below them. The same family as the Now
 * Playing and collection cards (`nowPlayingCard.ts`, `collectionCard.ts`)
 * — a blurred copy of the cover behind, an accent strip in its dominant
 * colour, the app name at the foot.
 *
 * The lines are laid out to fit, whatever the script: a line too wide
 * for the card wraps at word boundaries (`Intl.Segmenter`, which knows
 * where a Japanese or Thai line may break without spaces), a word wider
 * than the card breaks between characters, and the type shrinks until
 * the excerpt fits its block — down to a floor, past which the last row
 * that fits ends in an ellipsis. A right-to-left excerpt (Arabic,
 * Hebrew…) is right-aligned and mirrors the cover row.
 */
export interface LyricsCardOptions {
  /** The chosen lines, in song order. At most {@link MAX_LYRIC_LINES}
   *  are drawn. */
  lines: string[];
  title: string;
  artist: string | null;
  /** The cover, already resolved to something an `<img>` can load. */
  coverSrc: string | null;
  labels: {
    /** "Lyrics" — above the excerpt. */
    eyebrow: string;
    /** The whole footer line, app name included ("on WaveFlow"). */
    footer: string;
  };
}

/** How many lines an excerpt can hold. A card is a quote, not the song:
 *  past six lines the type would have to shrink below a size that reads
 *  on a phone. */
export const MAX_LYRIC_LINES = 6;

const SIZE = 1080;
const MARGIN = 96;
const BLOCK_TOP = 170;
const BLOCK_BOTTOM = 800;
const MAX_FONT = 64;
const MIN_FONT = 34;
/** Rows one lyric line may wrap to at the smallest type before it is cut
 *  short, so one line cannot crowd out the others. */
const MAX_ROWS_PER_LINE = 4;
const COVER = 132;

export async function renderLyricsCard(opts: LyricsCardOptions): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = SIZE;
  canvas.height = SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D context unavailable");
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";

  const cover = await tryLoadImage(opts.coverSrc);
  let accent = { r: 30, g: 30, b: 30 };
  if (cover && opts.coverSrc) {
    try {
      accent = await dominantColor(opts.coverSrc);
    } catch {
      // The neutral default still makes a readable card.
    }
  }

  // ----- Backdrop -----------------------------------------------------
  // Blurred once into a small canvas and stretched: `ctx.filter` does
  // nothing on WebKitGTK, where a filtered backdrop came out sharp.
  if (cover) {
    const blurred = document.createElement("canvas");
    drawBlurred(blurred, cover, SIZE, SIZE, {
      blur: 60,
      saturate: 1.2,
      brightness: 0.55,
      scale: 1.15,
    });
    ctx.drawImage(blurred, 0, 0, SIZE, SIZE);
    const wash = ctx.createLinearGradient(0, 0, 0, SIZE);
    wash.addColorStop(0, "rgba(0,0,0,0.35)");
    wash.addColorStop(1, `rgba(${darken(accent, 0.35)},0.85)`);
    ctx.fillStyle = wash;
    ctx.fillRect(0, 0, SIZE, SIZE);
  } else {
    const grad = ctx.createLinearGradient(0, 0, SIZE, SIZE);
    grad.addColorStop(0, `rgb(${accent.r},${accent.g},${accent.b})`);
    grad.addColorStop(1, "#0c0c0c");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, SIZE, SIZE);
  }

  const lines = opts.lines
    .map((line) => line.trim())
    .filter(Boolean)
    .slice(0, MAX_LYRIC_LINES);
  // The excerpt's direction is its first line's: the whole block aligns
  // one way, each line still shaped in its own direction.
  const rtl = lines.length > 0 && isRtl(lines[0]);
  const start = rtl ? SIZE - MARGIN : MARGIN;
  const align: CanvasTextAlign = rtl ? "right" : "left";
  const width = SIZE - MARGIN * 2;

  // ----- Eyebrow ------------------------------------------------------
  ctx.direction = rtl ? "rtl" : "ltr";
  ctx.textAlign = align;
  ctx.fillStyle = "rgba(255,255,255,0.7)";
  ctx.font = cardFont(600, 26);
  ctx.fillText(cardEyebrow(opts.labels.eyebrow), start, 112);

  // ----- Excerpt ------------------------------------------------------
  const layout = fitExcerpt(ctx, lines, width, BLOCK_BOTTOM - BLOCK_TOP);
  const rowHeight = Math.round(layout.font * 1.22);
  const gap = Math.round(layout.font * 0.45);
  const blockHeight =
    layout.rows.reduce((sum, rows) => sum + rows.length * rowHeight, 0) +
    gap * Math.max(0, layout.rows.length - 1);
  // Centred in its block, the first baseline one ascent below its top.
  let y =
    BLOCK_TOP +
    Math.max(0, (BLOCK_BOTTOM - BLOCK_TOP - blockHeight) / 2) +
    layout.font;
  ctx.fillStyle = "#fff";
  ctx.font = cardFont(700, layout.font);
  layout.rows.forEach((rows, i) => {
    ctx.direction = isRtl(lines[i]) ? "rtl" : "ltr";
    for (const row of rows) {
      ctx.fillText(row, start, y);
      y += rowHeight;
    }
    y += gap;
  });

  // ----- Cover + track ------------------------------------------------
  const coverY = 852;
  const coverX = rtl ? SIZE - MARGIN - COVER : MARGIN;
  ctx.save();
  ctx.shadowColor = "rgba(0,0,0,0.5)";
  ctx.shadowBlur = 24;
  ctx.shadowOffsetY = 8;
  drawCard(ctx, coverX, coverY, COVER, COVER, 18, "rgba(255,255,255,0.08)");
  ctx.restore();
  if (cover) {
    drawRoundedImage(ctx, cover, coverX, coverY, COVER, COVER, 18);
  } else {
    ctx.fillStyle = "rgba(255,255,255,0.3)";
    ctx.font = cardFont(900, 64);
    ctx.textAlign = "center";
    ctx.direction = "ltr";
    ctx.fillText("♪", coverX + COVER / 2, coverY + COVER / 2 + 22);
  }

  const textX = rtl ? coverX - 32 : coverX + COVER + 32;
  const textWidth = SIZE - MARGIN * 2 - COVER - 32;
  ctx.textAlign = align;
  ctx.direction = isRtl(opts.title) ? "rtl" : "ltr";
  ctx.fillStyle = "#fff";
  ctx.font = cardFont(800, 36);
  ctx.fillText(truncate(ctx, opts.title, textWidth), textX, coverY + 56);
  if (opts.artist) {
    ctx.direction = isRtl(opts.artist) ? "rtl" : "ltr";
    ctx.fillStyle = "rgba(255,255,255,0.75)";
    ctx.font = cardFont(500, 28);
    ctx.fillText(truncate(ctx, opts.artist, textWidth), textX, coverY + 100);
  }

  // ----- Accent strip + brand ----------------------------------------
  ctx.fillStyle = `rgb(${accent.r},${accent.g},${accent.b})`;
  ctx.fillRect(0, SIZE - 8, SIZE, 8);
  ctx.direction = "ltr";
  ctx.fillStyle = "rgba(255,255,255,0.5)";
  ctx.font = cardFont(600, 22);
  ctx.textAlign = "center";
  ctx.fillText(opts.labels.footer, SIZE / 2, SIZE - 28);

  return canvasToPng(canvas);
}

/** The lines a lyric can be quoted from, as the lyrics panel shows
 *  them: the synced lines' text, or the plain text split at its line
 *  breaks (a radio's with its stamps already stripped). Blank lines are
 *  kept, so an index still points at the line it did on screen; the
 *  picker skips them. */
export function shareableLyricLines(lyrics: {
  isSynced: boolean;
  lrcLines: LyricsLine[];
  isRadio: boolean;
  radioPlainText: string | null;
  payload: LyricsPayload | null;
}): string[] {
  if (lyrics.isSynced) return lyrics.lrcLines.map((line) => line.text);
  const text = lyrics.isRadio ? lyrics.radioPlainText : lyrics.payload?.content;
  return text ? text.split(/\r?\n/) : [];
}

/** Whether a string reads right to left: its first letter belongs to a
 *  right-to-left script. Digits and punctuation decide nothing. */
export function isRtl(text: string): boolean {
  const first = text.match(/\p{L}/u)?.[0];
  return (
    first != null &&
    /[\p{Script=Arabic}\p{Script=Hebrew}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Nko}]/u.test(
      first,
    )
  );
}

/** The largest type the excerpt fits its block at, and each line's rows
 *  at that size. At the floor, rows past the block are dropped and the
 *  last one kept ends in an ellipsis. */
function fitExcerpt(
  ctx: CanvasRenderingContext2D,
  lines: string[],
  width: number,
  height: number,
): { font: number; rows: string[][] } {
  // Rows are capped per line only at the floor: above it, a long line
  // makes the type shrink rather than lose its end.
  const measure = (font: number, rowsPerLine: number) => {
    ctx.font = cardFont(700, font);
    const rows = lines.map((line) =>
      capRows(ctx, wrapText(ctx, line, width), rowsPerLine, width),
    );
    const count = rows.reduce((sum, r) => sum + r.length, 0);
    const total =
      count * Math.round(font * 1.22) +
      Math.round(font * 0.45) * Math.max(0, rows.length - 1);
    return { rows, total };
  };
  for (let font = MAX_FONT; font > MIN_FONT; font -= 2) {
    const { rows, total } = measure(font, Infinity);
    if (total <= height) return { font, rows };
  }
  // At the floor: as it is when it fits, else capped per line, keeping
  // whole rows while they fit.
  const whole = measure(MIN_FONT, Infinity);
  if (whole.total <= height) return { font: MIN_FONT, rows: whole.rows };
  const { rows } = measure(MIN_FONT, MAX_ROWS_PER_LINE);
  const rowHeight = Math.round(MIN_FONT * 1.22);
  const gap = Math.round(MIN_FONT * 0.45);
  const kept: string[][] = [];
  let used = 0;
  for (const lineRows of rows) {
    const room = Math.floor(
      (height - used - (kept.length > 0 ? gap : 0)) / rowHeight,
    );
    if (room <= 0) break;
    if (room < lineRows.length) {
      kept.push(capRows(ctx, lineRows, room, width));
      break;
    }
    kept.push(lineRows);
    used += (kept.length > 1 ? gap : 0) + lineRows.length * rowHeight;
  }
  if (kept.length < rows.length && kept.length > 0) {
    // A line dropped whole: say the excerpt goes on.
    const last = kept[kept.length - 1];
    const row = last[last.length - 1];
    if (!row.endsWith("…")) last[last.length - 1] = ellipsize(ctx, row, width);
  }
  return { font: MIN_FONT, rows: kept };
}

/** At most `max` rows, the last one ending in an ellipsis when rows were
 *  cut. */
function capRows(
  ctx: CanvasRenderingContext2D,
  rows: string[],
  max: number,
  width: number,
): string[] {
  if (rows.length <= max) return rows;
  const kept = rows.slice(0, max);
  kept[max - 1] = ellipsize(ctx, kept[max - 1], width);
  return kept;
}

/** `text` followed by an ellipsis, shortened until both fit. */
function ellipsize(
  ctx: CanvasRenderingContext2D,
  text: string,
  width: number,
): string {
  const chars = graphemes(text.trimEnd());
  while (
    chars.length > 0 &&
    ctx.measureText(chars.join("") + "…").width > width
  ) {
    chars.pop();
  }
  return chars.join("").trimEnd() + "…";
}

/** Break `text` into rows no wider than `width`, at the places a line
 *  may break in its language; a single word wider than the row breaks
 *  between characters. */
export function wrapText(
  ctx: CanvasRenderingContext2D,
  text: string,
  width: number,
): string[] {
  const rows: string[] = [];
  let row = "";
  for (const segment of words(text)) {
    const candidate = row + segment;
    if (ctx.measureText(candidate.trimEnd()).width <= width) {
      row = candidate;
      continue;
    }
    if (row.trim()) rows.push(row.trimEnd());
    row = segment.trimStart();
    // A word that does not fit a row of its own.
    while (ctx.measureText(row).width > width) {
      const chars = graphemes(row);
      // One grapheme wider than the row (a long ZWJ emoji sequence)
      // cannot be split: it overflows its row rather than loop forever.
      if (chars.length <= 1) break;
      let cut = chars.length - 1;
      while (
        cut > 1 &&
        ctx.measureText(chars.slice(0, cut).join("")).width > width
      ) {
        cut--;
      }
      rows.push(chars.slice(0, cut).join(""));
      row = chars.slice(cut).join("");
    }
  }
  if (row.trim()) rows.push(row.trimEnd());
  return rows;
}

/** Word-sized pieces of `text`, spaces kept with the word before them.
 *  `Intl.Segmenter` finds the breaks of a script written without spaces;
 *  without it, spaces are the only breaks. */
function words(text: string): string[] {
  if (typeof Intl !== "undefined" && "Segmenter" in Intl) {
    const segmenter = new Intl.Segmenter(undefined, { granularity: "word" });
    const out: string[] = [];
    for (const { segment, isWordLike } of segmenter.segment(text)) {
      // Spaces and punctuation ride with the piece before them, so a
      // row never starts with a comma or a closing bracket.
      if (!isWordLike && out.length > 0) out[out.length - 1] += segment;
      else out.push(segment);
    }
    return out;
  }
  return text.split(/(?<=\s)/);
}

function graphemes(text: string): string[] {
  if (typeof Intl !== "undefined" && "Segmenter" in Intl) {
    const segmenter = new Intl.Segmenter(undefined, {
      granularity: "grapheme",
    });
    return Array.from(segmenter.segment(text), (s) => s.segment);
  }
  return Array.from(text);
}
