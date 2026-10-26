/**
 * Drawing helpers shared by the square share cards (Now Playing, playlist,
 * artist). Canvas only, the WebView's own font stack, no dependency.
 */

/** The font stack every card writes with, behind a weight and a size. */
export function cardFont(weight: number, size: number): string {
  return `${weight} ${size}px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif`;
}

/** A card's eyebrow in capitals, by the rules of the UI language: a bare
 *  `toUpperCase()` turns Turkish "sözleri" into "SÖZLERI", without the
 *  dot its capital İ keeps. */
export function cardEyebrow(text: string): string {
  return text.toLocaleUpperCase(document.documentElement.lang || undefined);
}

export function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = (e) => reject(e);
    img.src = src;
  });
}

/** `loadImage`, with a missing or unreadable image as `null`: a card
 *  draws a placeholder rather than failing over one cover. */
export async function tryLoadImage(
  src: string | null,
): Promise<HTMLImageElement | null> {
  if (!src) return null;
  try {
    return await loadImage(src);
  } catch {
    return null;
  }
}

export function roundedRect(
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

export function drawCard(
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

/** Cover-fit an image into a rounded frame: scaled and centre-cropped,
 *  so a source that is not 1:1 still fills it. A radius of half the
 *  side makes a circle. */
export function drawRoundedImage(
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
  const ratio = Math.max(w / img.width, h / img.height);
  const drawW = img.width * ratio;
  const drawH = img.height * ratio;
  ctx.drawImage(img, x + (w - drawW) / 2, y + (h - drawH) / 2, drawW, drawH);
  ctx.restore();
}

export function truncate(
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

export function darken(
  c: { r: number; g: number; b: number },
  factor: number,
): string {
  const r = Math.max(0, Math.round(c.r * factor));
  const g = Math.max(0, Math.round(c.g * factor));
  const b = Math.max(0, Math.round(c.b * factor));
  return `${r},${g},${b}`;
}

export function canvasToPng(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise<Blob>((resolve, reject) => {
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
