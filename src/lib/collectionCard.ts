import { dominantColor } from "./dominantColor";
import {
  canvasToPng,
  cardEyebrow,
  cardFont,
  darken,
  drawCard,
  drawRoundedImage,
  roundedRect,
  tryLoadImage,
  truncate,
} from "./cardCanvas";

/**
 * A 1080×1080 share card for a playlist or an artist: its picture, its
 * name, a line of figures, and the first few tracks. The same recipe as
 * the Now Playing card (`nowPlayingCard.ts`) — a blurred copy of the
 * picture behind, an accent strip in its dominant colour, the app name at
 * the foot — so the cards read as one family.
 */
export interface CollectionCardOptions {
  /** A playlist's picture is a rounded square, an artist's a circle. */
  shape: "square" | "circle";
  /** The picture, already resolved to something an `<img>` can load. */
  imageSrc: string | null;
  /** Used when there is no picture: up to four covers in a 2×2 grid, the
   *  way a playlist without a cover of its own still looks like its
   *  contents. Ignored when `imageSrc` loads. */
  collageSrcs?: string[];
  title: string;
  /** The figures under the name ("12 tracks · 48 min"). */
  subtitle: string | null;
  /** The tracks listed under the header, at most five drawn. */
  rows: { title: string; subtitle: string | null }[];
  labels: {
    /** "Playlist", "Artist" — above the name. */
    eyebrow: string;
    /** The whole footer line, app name included ("on WaveFlow"). */
    footer: string;
  };
}

const SIZE = 1080;
const MARGIN = 90;
const PICTURE = 360;
const MAX_ROWS = 5;

export async function renderCollectionCard(
  opts: CollectionCardOptions,
): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = SIZE;
  canvas.height = SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D context unavailable");
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";

  // ----- Pictures -----------------------------------------------------
  const picture = await tryLoadImage(opts.imageSrc);
  const collage = picture
    ? []
    : (
        await Promise.all(
          (opts.collageSrcs ?? []).slice(0, 4).map((src) => tryLoadImage(src)),
        )
      ).filter((img): img is HTMLImageElement => img != null);
  // What the backdrop and the accent are drawn from: the picture, or the
  // first cover of the collage standing in for it.
  const backdrop = picture ?? collage[0] ?? null;
  const backdropSrc = picture
    ? opts.imageSrc
    : collage.length > 0
      ? collage[0].src
      : null;

  let accent = { r: 30, g: 30, b: 30 };
  if (backdropSrc) {
    try {
      accent = await dominantColor(backdropSrc);
    } catch {
      // The neutral default still makes a readable card.
    }
  }

  // ----- Backdrop -----------------------------------------------------
  if (backdrop) {
    ctx.save();
    ctx.filter = "blur(60px) brightness(0.55)";
    ctx.drawImage(backdrop, -80, -80, SIZE + 160, SIZE + 160);
    ctx.restore();
    const wash = ctx.createLinearGradient(0, 0, 0, SIZE);
    wash.addColorStop(0, "rgba(0,0,0,0.45)");
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

  // ----- Picture ------------------------------------------------------
  const px = MARGIN;
  const py = 110;
  const radius = opts.shape === "circle" ? PICTURE / 2 : 32;
  ctx.save();
  ctx.shadowColor = "rgba(0,0,0,0.5)";
  ctx.shadowBlur = 40;
  ctx.shadowOffsetY = 12;
  drawCard(ctx, px, py, PICTURE, PICTURE, radius, "rgba(255,255,255,0.06)");
  ctx.restore();
  if (picture) {
    drawRoundedImage(ctx, picture, px, py, PICTURE, PICTURE, radius);
  } else if (collage.length > 0) {
    drawCollage(ctx, collage, px, py, PICTURE, radius);
  } else {
    ctx.fillStyle = "rgba(255,255,255,0.3)";
    ctx.font = cardFont(900, 160);
    ctx.textAlign = "center";
    ctx.fillText("♪", px + PICTURE / 2, py + PICTURE / 2 + 56);
  }

  // ----- Name block, beside the picture -------------------------------
  const tx = px + PICTURE + 60;
  const textWidth = SIZE - MARGIN - tx;
  ctx.textAlign = "left";
  ctx.fillStyle = "rgba(255,255,255,0.7)";
  ctx.font = cardFont(600, 26);
  ctx.fillText(cardEyebrow(opts.labels.eyebrow), tx, py + 70);

  ctx.fillStyle = "#fff";
  ctx.font = cardFont(800, 60);
  const titleLines = wrapLines(ctx, opts.title, textWidth, 2);
  titleLines.forEach((line, i) => {
    ctx.fillText(line, tx, py + 150 + i * 70);
  });

  if (opts.subtitle) {
    ctx.fillStyle = "rgba(255,255,255,0.75)";
    ctx.font = cardFont(500, 30);
    ctx.fillText(
      truncate(ctx, opts.subtitle, textWidth),
      tx,
      py + 150 + titleLines.length * 70 + 20,
    );
  }

  // ----- Tracks -------------------------------------------------------
  const rows = opts.rows.slice(0, MAX_ROWS);
  const listTop = py + PICTURE + 100;
  const rowHeight = 84;
  rows.forEach((row, i) => {
    const y = listTop + i * rowHeight;
    ctx.textAlign = "right";
    ctx.fillStyle = "rgba(255,255,255,0.45)";
    ctx.font = cardFont(700, 30);
    ctx.fillText(String(i + 1), MARGIN + 36, y);

    ctx.textAlign = "left";
    ctx.fillStyle = "#fff";
    ctx.font = cardFont(600, 32);
    ctx.fillText(
      truncate(ctx, row.title, SIZE - MARGIN * 2 - 64),
      MARGIN + 64,
      y,
    );
    if (row.subtitle) {
      ctx.fillStyle = "rgba(255,255,255,0.55)";
      ctx.font = cardFont(400, 24);
      ctx.fillText(
        truncate(ctx, row.subtitle, SIZE - MARGIN * 2 - 64),
        MARGIN + 64,
        y + 32,
      );
    }
  });

  // ----- Accent strip + brand ----------------------------------------
  ctx.fillStyle = `rgb(${accent.r},${accent.g},${accent.b})`;
  ctx.fillRect(0, SIZE - 8, SIZE, 8);
  ctx.fillStyle = "rgba(255,255,255,0.5)";
  ctx.font = cardFont(600, 24);
  ctx.textAlign = "center";
  ctx.fillText(opts.labels.footer, SIZE / 2, SIZE - 36);

  return canvasToPng(canvas);
}

/** Four covers in a 2×2 grid inside the rounded frame. With fewer, the
 *  first one alone: a grid with empty cells reads as a broken image. */
function drawCollage(
  ctx: CanvasRenderingContext2D,
  images: HTMLImageElement[],
  x: number,
  y: number,
  size: number,
  radius: number,
) {
  if (images.length < 4) {
    drawRoundedImage(ctx, images[0], x, y, size, size, radius);
    return;
  }
  const half = size / 2;
  ctx.save();
  roundedRect(ctx, x, y, size, size, radius);
  ctx.clip();
  images.slice(0, 4).forEach((img, i) => {
    const cx = x + (i % 2) * half;
    const cy = y + Math.floor(i / 2) * half;
    drawRoundedImage(ctx, img, cx, cy, half, half, 0);
  });
  ctx.restore();
}

/** Break `text` into at most `maxLines` lines that fit `maxWidth`, the
 *  last one truncated with an ellipsis when the text runs on. */
function wrapLines(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
  maxLines: number,
): string[] {
  const words = text.split(/\s+/).filter(Boolean);
  const lines: string[] = [];
  let line = "";
  for (let i = 0; i < words.length; i++) {
    const candidate = line ? `${line} ${words[i]}` : words[i];
    if (ctx.measureText(candidate).width <= maxWidth || !line) {
      line = candidate;
      continue;
    }
    if (lines.length === maxLines - 1) {
      lines.push(
        truncate(ctx, `${line} ${words.slice(i).join(" ")}`, maxWidth),
      );
      return lines;
    }
    // Truncated too: a first word wider than the line was taken whole.
    lines.push(truncate(ctx, line, maxWidth));
    line = words[i];
  }
  if (line) lines.push(truncate(ctx, line, maxWidth));
  return lines;
}
