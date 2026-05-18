import type { Track } from "./tauri/track";
import { resolveRemoteImage } from "./tauri/artwork";
import { dominantColor } from "./dominantColor";

/**
 * Build a 1080×1080 square "Now Playing" share card on an offscreen
 * Canvas and return the PNG Blob. Same Canvas-only philosophy as
 * `wrappedCard.ts` — text uses the WebView's native font stack, no
 * external dep. The visual recipe:
 *
 *   1. Backdrop = blurred + darkened copy of the cover (or a neutral
 *      gradient when there's no artwork).
 *   2. Cover artwork centred at ~58 % of the card width with rounded
 *      corners and a generous drop-shadow.
 *   3. Title (big bold) + artist (lighter) + album (small dim) stacked
 *      below the cover.
 *   4. WaveFlow brand strip + "Now Playing" eyebrow at the bottom.
 *
 * The cover image is read **before** rendering so we can extract its
 * dominant colour and use it for the bottom-of-card accent strip —
 * that little flourish keeps the card visually coherent with the
 * artwork instead of a generic dark backdrop on every share.
 */
export interface NowPlayingCardOptions {
  labels: {
    nowPlaying: string;
    on: string; // "on WaveFlow" or equivalent
  };
}

const SIZE = 1080;

export async function renderNowPlayingCard(
  track: Track,
  opts: NowPlayingCardOptions,
): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = SIZE;
  canvas.height = SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D context unavailable");
  // Smoother image scaling for the inevitable cover upscale below.
  // Browsers default to "low" which produces visibly soft cover art on
  // a 1080×1080 card.
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";

  // ----- Load cover (if any) ------------------------------------------
  // Prefer the full-resolution artwork over the thumbnails — `_2x` is
  // 128×128 and `_1x` is 64×64 (see src-tauri/src/thumbnails.rs), both
  // far too small for the 580 px cover slot and 1320 px blurred
  // backdrop on this 1080² card. The original `artwork_path` is the
  // image the scanner extracted from the audio file (typically
  // 500–1500 px square), which gives a crisp share card.
  const coverSrc = resolveRemoteImage(
    track.artwork_path ?? track.artwork_path_2x ?? track.artwork_path_1x,
    null,
  );
  let coverImg: HTMLImageElement | null = null;
  if (coverSrc) {
    try {
      coverImg = await loadImage(coverSrc);
    } catch {
      coverImg = null;
    }
  }

  // ----- Dominant colour ----------------------------------------------
  let accent: { r: number; g: number; b: number } = { r: 30, g: 30, b: 30 };
  if (coverSrc) {
    try {
      accent = await dominantColor(coverSrc);
    } catch {
      // Fall through to the neutral default — a Now Playing card
      // without artwork-derived accent still looks fine.
    }
  }

  // ----- Backdrop ------------------------------------------------------
  if (coverImg) {
    // Real CSS-style blur via the 2D context filter. Both
    // Chromium-WebView2 (Windows) and WebKitGTK 2.40+ (Linux) support
    // it; macOS WebKit has supported it for years. Falls back to a no-
    // op string on the off chance an old engine doesn't recognise it,
    // which gives a slightly less blurry but still readable backdrop.
    ctx.save();
    ctx.filter = "blur(60px) brightness(0.6)";
    // Draw a bit past the edges so the blur's transparent halo doesn't
    // bleed into the canvas border.
    ctx.drawImage(coverImg, -80, -80, SIZE + 160, SIZE + 160);
    ctx.restore();
    // Tinted gradient overlay so the foreground text + cover stay
    // readable on bright artwork.
    const wash = ctx.createLinearGradient(0, 0, 0, SIZE);
    wash.addColorStop(0, `rgba(0,0,0,0.45)`);
    wash.addColorStop(1, `rgba(${darken(accent, 0.4)},0.75)`);
    ctx.fillStyle = wash;
    ctx.fillRect(0, 0, SIZE, SIZE);
  } else {
    const grad = ctx.createLinearGradient(0, 0, SIZE, SIZE);
    grad.addColorStop(0, `rgb(${accent.r},${accent.g},${accent.b})`);
    grad.addColorStop(1, "#0c0c0c");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, SIZE, SIZE);
  }

  // ----- Eyebrow -------------------------------------------------------
  ctx.fillStyle = "rgba(255,255,255,0.7)";
  ctx.font =
    "600 28px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.textAlign = "center";
  ctx.fillText(opts.labels.nowPlaying.toUpperCase(), SIZE / 2, 80);

  // ----- Cover (centerpiece) ------------------------------------------
  const coverSize = 580;
  const coverX = (SIZE - coverSize) / 2;
  const coverY = 140;
  // Soft drop shadow under the cover.
  ctx.save();
  ctx.shadowColor = "rgba(0,0,0,0.5)";
  ctx.shadowBlur = 40;
  ctx.shadowOffsetY = 12;
  drawCard(
    ctx,
    coverX,
    coverY,
    coverSize,
    coverSize,
    32,
    "rgba(255,255,255,0.05)",
  );
  ctx.restore();

  if (coverImg) {
    drawRoundedImage(ctx, coverImg, coverX, coverY, coverSize, coverSize, 32);
  } else {
    // Music-note placeholder when there's no artwork.
    ctx.fillStyle = "rgba(255,255,255,0.3)";
    ctx.font =
      "900 200px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
    ctx.textAlign = "center";
    ctx.fillText("♪", SIZE / 2, coverY + coverSize / 2 + 80);
  }

  // ----- Track info ---------------------------------------------------
  const infoY = coverY + coverSize + 70;
  ctx.fillStyle = "#fff";
  ctx.font =
    "800 52px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.textAlign = "center";
  ctx.fillText(truncate(ctx, track.title, SIZE - 160), SIZE / 2, infoY);

  if (track.artist_name) {
    ctx.fillStyle = "rgba(255,255,255,0.8)";
    ctx.font =
      "500 34px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
    ctx.fillText(
      truncate(ctx, track.artist_name, SIZE - 160),
      SIZE / 2,
      infoY + 50,
    );
  }

  if (track.album_title) {
    ctx.fillStyle = "rgba(255,255,255,0.5)";
    ctx.font =
      "400 26px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
    ctx.fillText(
      truncate(ctx, track.album_title, SIZE - 200),
      SIZE / 2,
      infoY + 90,
    );
  }

  // ----- Accent strip + brand ----------------------------------------
  // Thin accent bar across the bottom in the artwork's dominant
  // colour so each card carries a hint of the cover's palette.
  ctx.fillStyle = `rgb(${accent.r},${accent.g},${accent.b})`;
  ctx.fillRect(0, SIZE - 8, SIZE, 8);

  ctx.fillStyle = "rgba(255,255,255,0.5)";
  ctx.font =
    "600 24px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.textAlign = "center";
  ctx.fillText(`${opts.labels.on} · WaveFlow`, SIZE / 2, SIZE - 36);

  return await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (blob) => {
        if (blob) resolve(blob);
        else reject(new Error("canvas.toBlob returned null"));
      },
      "image/png",
      0.95,
    );
  });
}

// =============================================================================
// Helpers
// =============================================================================

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = (e) => reject(e);
    img.src = src;
  });
}

function roundedRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + r);
  ctx.lineTo(x + w, y + h - r);
  ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
  ctx.lineTo(x + r, y + h);
  ctx.quadraticCurveTo(x, y + h, x, y + h - r);
  ctx.lineTo(x, y + r);
  ctx.quadraticCurveTo(x, y, x + r, y);
  ctx.closePath();
}

function drawCard(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
  fill: string,
) {
  ctx.fillStyle = fill;
  roundedRect(ctx, x, y, w, h, r);
  ctx.fill();
}

function drawRoundedImage(
  ctx: CanvasRenderingContext2D,
  img: HTMLImageElement,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.save();
  roundedRect(ctx, x, y, w, h, r);
  ctx.clip();
  // Cover-fit: scale + centre-crop so square covers fit a square
  // frame even if the source isn't 1:1.
  const ratio = Math.max(w / img.width, h / img.height);
  const drawW = img.width * ratio;
  const drawH = img.height * ratio;
  ctx.drawImage(img, x + (w - drawW) / 2, y + (h - drawH) / 2, drawW, drawH);
  ctx.restore();
}

function truncate(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
): string {
  if (ctx.measureText(text).width <= maxWidth) return text;
  let s = text;
  while (s.length > 1 && ctx.measureText(s + "…").width > maxWidth) {
    s = s.slice(0, -1);
  }
  return s + "…";
}

function darken(
  c: { r: number; g: number; b: number },
  factor: number,
): string {
  const r = Math.max(0, Math.round(c.r * factor));
  const g = Math.max(0, Math.round(c.g * factor));
  const b = Math.max(0, Math.round(c.b * factor));
  return `${r},${g},${b}`;
}
