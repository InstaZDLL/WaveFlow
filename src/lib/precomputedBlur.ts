/**
 * Blur an image once, into a small canvas, on any engine.
 *
 * `PrecomputedBlur` (the component) stretches the result over its box;
 * this module does the drawing. Three ways, the first that works:
 *
 * 1. Pixels: the image is drawn into the canvas, darkened and saturated
 *    per pixel, then blurred by three box passes (a close approximation
 *    of a Gaussian) on the canvas's own few thousand pixels. Needs the
 *    pixels to be readable, which an image loaded with
 *    `crossOrigin="anonymous"` from the asset protocol is — the cover
 *    accent mode (`dominantColor`) reads them the same way. Works where
 *    the 2D context has no `filter`, which is WebKitGTK 2.54 and WebKit
 *    on macOS: exactly the engines the CSS blur hurt.
 * 2. `ctx.filter`, when the pixels are not readable (a remote cover
 *    served without CORS) and the context has a filter.
 * 3. Neither: the image shrunk to a few pixels and stretched back, which
 *    smooths it the way a large blur does; no darkening finer than a
 *    black wash, no saturation.
 */

export interface BlurRecipe {
  /** Radius in CSS pixels of the box, as a CSS `blur()` would say. */
  blur: number;
  saturate: number;
  brightness: number;
  /** Cover-fit, then zoomed this much past the box's edges. */
  scale: number;
}

/** Width of the canvas the image is blurred into, at most. */
export const MAX_BLUR_WIDTH = 192;

/**
 * Load an image whose pixels can be read when its server allows it: with
 * `crossOrigin="anonymous"` first, and without it when that fails (a CDN
 * that sends no CORS header refuses the anonymous request outright).
 */
export function loadBlurSource(src: string): Promise<HTMLImageElement> {
  const load = (cors: boolean) =>
    new Promise<HTMLImageElement>((resolve, reject) => {
      const image = new Image();
      if (cors) image.crossOrigin = "anonymous";
      image.decoding = "async";
      image.onload = () => resolve(image);
      image.onerror = () => reject(new Error(`cannot load ${src}`));
      image.src = src;
    });
  return load(true).catch(() => load(false));
}

/** Draw `image` blurred into `canvas`, sized for a box of `boxW` × `boxH`
 *  CSS pixels. */
export function drawBlurred(
  canvas: HTMLCanvasElement,
  image: HTMLImageElement,
  boxW: number,
  boxH: number,
  recipe: BlurRecipe,
): void {
  const w = Math.min(MAX_BLUR_WIDTH, Math.ceil(boxW));
  const h = Math.max(1, Math.round((w * boxH) / boxW));
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) return;
  const sigma = recipe.blur * (w / boxW);
  const s =
    Math.max(w / image.naturalWidth, h / image.naturalHeight) * recipe.scale;
  const dw = image.naturalWidth * s;
  const dh = image.naturalHeight * s;
  const x = (w - dw) / 2;
  const y = (h - dh) / 2;

  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(image, x, y, dw, dh);
  let pixels: ImageData | null = null;
  try {
    pixels = ctx.getImageData(0, 0, w, h);
  } catch {
    // Tainted: a cover served without CORS.
  }
  if (pixels) {
    tone(pixels.data, recipe.brightness, recipe.saturate);
    gaussian(pixels.data, w, h, sigma);
    ctx.putImageData(pixels, 0, 0);
    return;
  }
  if (typeof (ctx as { filter?: unknown }).filter === "string") {
    ctx.clearRect(0, 0, w, h);
    ctx.filter = `brightness(${recipe.brightness}) saturate(${recipe.saturate}) blur(${sigma.toFixed(2)}px)`;
    ctx.drawImage(image, x, y, dw, dh);
    return;
  }
  shrinkAndStretch(canvas, ctx, image, x, y, dw, dh, sigma);
  if (recipe.brightness < 1) {
    ctx.fillStyle = `rgb(0 0 0 / ${1 - recipe.brightness})`;
    ctx.fillRect(0, 0, w, h);
  }
}

/** CSS `brightness()` then `saturate()`, per pixel (the filter spec's
 *  matrices). */
function tone(d: Uint8ClampedArray, brightness: number, saturate: number) {
  if (brightness === 1 && saturate === 1) return;
  const s = saturate;
  const m = [
    0.213 + 0.787 * s,
    0.715 - 0.715 * s,
    0.072 - 0.072 * s,
    0.213 - 0.213 * s,
    0.715 + 0.285 * s,
    0.072 - 0.072 * s,
    0.213 - 0.213 * s,
    0.715 - 0.715 * s,
    0.072 + 0.928 * s,
  ];
  for (let i = 0; i < d.length; i += 4) {
    const r = d[i] * brightness;
    const g = d[i + 1] * brightness;
    const b = d[i + 2] * brightness;
    d[i] = m[0] * r + m[1] * g + m[2] * b;
    d[i + 1] = m[3] * r + m[4] * g + m[5] * b;
    d[i + 2] = m[6] * r + m[7] * g + m[8] * b;
  }
}

/** Three box blurs whose sizes add up to a Gaussian of `sigma`
 *  (Kovesi's construction), each run horizontally then vertically. Edges
 *  repeat their last pixel. */
function gaussian(d: Uint8ClampedArray, w: number, h: number, sigma: number) {
  if (sigma < 0.5) return;
  const n = 3;
  const ideal = Math.sqrt((12 * sigma * sigma) / n + 1);
  let wl = Math.floor(ideal);
  if (wl % 2 === 0) wl--;
  const wu = wl + 2;
  const mIdeal =
    (12 * sigma * sigma - n * wl * wl - 4 * n * wl - 3 * n) / (-4 * wl - 4);
  const m = Math.round(mIdeal);
  const tmp = new Float32Array(d.length);
  const src = Float32Array.from(d);
  for (let k = 0; k < n; k++) {
    const r = ((k < m ? wl : wu) - 1) / 2;
    boxH(src, tmp, w, h, r);
    boxV(tmp, src, w, h, r);
  }
  for (let i = 0; i < d.length; i++) d[i] = src[i];
}

function boxH(
  a: Float32Array,
  out: Float32Array,
  w: number,
  h: number,
  r: number,
) {
  const span = 2 * r + 1;
  for (let y = 0; y < h; y++) {
    const row = y * w * 4;
    for (let c = 0; c < 4; c++) {
      let acc = 0;
      for (let i = -r; i <= r; i++)
        acc += a[row + Math.min(w - 1, Math.max(0, i)) * 4 + c];
      for (let x = 0; x < w; x++) {
        out[row + x * 4 + c] = acc / span;
        const add = Math.min(w - 1, x + r + 1);
        const sub = Math.max(0, x - r);
        acc += a[row + add * 4 + c] - a[row + sub * 4 + c];
      }
    }
  }
}

function boxV(
  a: Float32Array,
  out: Float32Array,
  w: number,
  h: number,
  r: number,
) {
  const span = 2 * r + 1;
  for (let x = 0; x < w; x++) {
    for (let c = 0; c < 4; c++) {
      const col = x * 4 + c;
      let acc = 0;
      for (let i = -r; i <= r; i++)
        acc += a[Math.min(h - 1, Math.max(0, i)) * w * 4 + col];
      for (let y = 0; y < h; y++) {
        out[y * w * 4 + col] = acc / span;
        const add = Math.min(h - 1, y + r + 1);
        const sub = Math.max(0, y - r);
        acc += a[add * w * 4 + col] - a[sub * w * 4 + col];
      }
    }
  }
}

/** The last resort: halve down to a few pixels (each step averages, as a
 *  blur does), then let the browser's bilinear smoothing stretch it back. */
function shrinkAndStretch(
  canvas: HTMLCanvasElement,
  ctx: CanvasRenderingContext2D,
  image: HTMLImageElement,
  x: number,
  y: number,
  dw: number,
  dh: number,
  sigma: number,
) {
  const w = canvas.width;
  const h = canvas.height;
  const target = Math.max(2, Math.round(w / Math.max(1, sigma * 1.5)));
  let cur = document.createElement("canvas");
  cur.width = w;
  cur.height = h;
  cur.getContext("2d")!.drawImage(image, x, y, dw, dh);
  while (cur.width > target * 2) {
    const next = document.createElement("canvas");
    next.width = Math.max(target, Math.floor(cur.width / 2));
    next.height = Math.max(1, Math.round((next.width * h) / w));
    const nctx = next.getContext("2d")!;
    nctx.imageSmoothingQuality = "high";
    nctx.drawImage(cur, 0, 0, next.width, next.height);
    cur = next;
  }
  ctx.clearRect(0, 0, w, h);
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(cur, 0, 0, w, h);
}
