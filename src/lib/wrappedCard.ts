import type { WrappedPayload } from "./tauri/wrapped";
import { resolveRemoteImage } from "./tauri/artwork";
import { formatDuration } from "./tauri/track";

/**
 * Build a 1080×1920 portrait Wrapped share card on an offscreen
 * Canvas and return the resulting PNG Blob. Everything is rendered
 * with the browser's native 2D Canvas API — no external library, no
 * Rust font dependency. The visual style mirrors the in-app overlay
 * (radial-gradient backdrop, white type, frosted-glass row cards)
 * so a card screenshot still looks "WaveFlow" without the chrome.
 *
 * Returns a PNG Blob via `canvas.toBlob`; the caller decides whether
 * to save it to disk, copy it to the clipboard, or both.
 */
export interface CardOptions {
  /** Localised labels — passed in by the caller so this module stays
   *  free of `react-i18next` and works in any context. */
  labels: {
    wrapped: string;
    yourYear: string;
    minutes: string;
    plays: string;
    artists: string;
    topTracks: string;
    topArtists: string;
    mood: string;
    streak: string;
    daysInARow: string;
    poweredBy: string;
  };
  /** Locale code used for `toLocaleString` on big numbers. */
  locale: string;
}

export interface CardAccent {
  base: string;
  glow: string;
  glow2: string;
}

const W = 1080;
const H = 1920;

export async function renderWrappedCard(
  payload: WrappedPayload,
  accent: CardAccent,
  opts: CardOptions,
): Promise<Blob> {
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D context unavailable");

  // ----- Backdrop -----------------------------------------------------
  drawGradientBackdrop(ctx, accent);

  // ----- Header -------------------------------------------------------
  ctx.fillStyle = "rgba(255,255,255,0.7)";
  ctx.font =
    "600 32px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.textAlign = "left";
  ctx.fillText(opts.labels.wrapped.toUpperCase(), 80, 130);
  drawSparkle(ctx, 80 + measureWidth(ctx, opts.labels.wrapped.toUpperCase()) + 32, 110);

  // Year — the marquee element. Drawn massive and bold so the card
  // reads from a thumbnail.
  ctx.fillStyle = "#fff";
  ctx.font =
    "900 220px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(String(payload.year), 80, 360);

  // Subtitle
  ctx.fillStyle = "rgba(255,255,255,0.75)";
  ctx.font =
    "400 42px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(opts.labels.yourYear, 80, 430);

  // ----- Minutes block -----------------------------------------------
  const totalMinutes = Math.round(payload.total_listened_ms / 60_000);
  ctx.fillStyle = "rgba(255,255,255,0.55)";
  ctx.font =
    "600 28px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(opts.labels.minutes.toUpperCase(), 80, 540);

  ctx.fillStyle = "#fff";
  ctx.font =
    "800 160px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(totalMinutes.toLocaleString(opts.locale), 80, 690);

  // Sub-stats row (plays / artists)
  ctx.fillStyle = "rgba(255,255,255,0.7)";
  ctx.font =
    "500 34px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  const subStats = `${payload.total_plays.toLocaleString(opts.locale)} ${opts.labels.plays}  ·  ${payload.unique_artists.toLocaleString(opts.locale)} ${opts.labels.artists}`;
  ctx.fillText(subStats, 80, 750);

  // ----- Top tracks ---------------------------------------------------
  let y = 870;
  ctx.fillStyle = "rgba(255,255,255,0.55)";
  ctx.font =
    "600 26px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(opts.labels.topTracks.toUpperCase(), 80, y);
  y += 50;

  const top5Tracks = payload.top_tracks.slice(0, 5);
  await Promise.all(
    top5Tracks.map(async (tr, idx) => {
      const rowY = y + idx * 110;
      drawCard(ctx, 80, rowY, W - 160, 90);

      // Rank number
      ctx.fillStyle = "rgba(255,255,255,0.8)";
      ctx.font =
        "800 44px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
      ctx.textAlign = "center";
      ctx.fillText(String(idx + 1), 130, rowY + 60);

      // Artwork — try to load the local 2x or full path, fall back to
      // a blank tile when missing.
      const src = resolveRemoteImage(
        tr.artwork_path_2x ?? tr.artwork_path,
        null,
      );
      if (src) {
        try {
          const img = await loadImage(src);
          drawRoundedImage(ctx, img, 180, rowY + 12, 66, 66, 12);
        } catch {
          drawBlankTile(ctx, 180, rowY + 12, 66, 66, 12);
        }
      } else {
        drawBlankTile(ctx, 180, rowY + 12, 66, 66, 12);
      }

      // Title + artist
      ctx.textAlign = "left";
      ctx.fillStyle = "#fff";
      ctx.font =
        "600 32px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
      ctx.fillText(
        truncate(ctx, tr.title, W - 160 - 280 - 40),
        270,
        rowY + 40,
      );
      ctx.fillStyle = "rgba(255,255,255,0.6)";
      ctx.font =
        "400 24px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
      ctx.fillText(
        truncate(ctx, tr.artist_name ?? "—", W - 160 - 280 - 40),
        270,
        rowY + 72,
      );
    }),
  );
  y += 5 * 110 + 20;

  // ----- Mood + streak strip -----------------------------------------
  const stripY = y + 40;
  drawCard(ctx, 80, stripY, (W - 200) / 2, 160);
  ctx.fillStyle = "rgba(255,255,255,0.55)";
  ctx.font =
    "600 22px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(opts.labels.mood.toUpperCase(), 120, stripY + 50);
  ctx.fillStyle = "#fff";
  ctx.font =
    "800 64px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  const bpmDisplay =
    payload.mood.avg_bpm != null
      ? `${Math.round(payload.mood.avg_bpm)} BPM`
      : "—";
  ctx.fillText(bpmDisplay, 120, stripY + 130);

  const streakX = 80 + (W - 200) / 2 + 40;
  drawCard(ctx, streakX, stripY, (W - 200) / 2, 160);
  ctx.fillStyle = "rgba(255,255,255,0.55)";
  ctx.font =
    "600 22px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.fillText(opts.labels.streak.toUpperCase(), streakX + 40, stripY + 50);
  ctx.fillStyle = "#fff";
  ctx.font =
    "800 64px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  const streakDisplay = payload.streak ? `${payload.streak.days}` : "—";
  ctx.fillText(streakDisplay, streakX + 40, stripY + 130);
  if (payload.streak) {
    ctx.fillStyle = "rgba(255,255,255,0.6)";
    ctx.font =
      "400 24px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
    const dW = measureWidth(
      ctx,
      `${payload.streak.days}`,
      "800 64px ui-sans-serif",
    );
    ctx.fillText(
      opts.labels.daysInARow,
      streakX + 40 + dW + 20,
      stripY + 130,
    );
  }

  // ----- Footer / branding -------------------------------------------
  ctx.fillStyle = "rgba(255,255,255,0.45)";
  ctx.font =
    "500 28px ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif";
  ctx.textAlign = "center";
  ctx.fillText(
    `${opts.labels.poweredBy} · WaveFlow · ${formatDuration(payload.total_listened_ms)}`,
    W / 2,
    H - 80,
  );

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

function drawGradientBackdrop(
  ctx: CanvasRenderingContext2D,
  accent: CardAccent,
) {
  // Linear base layer — parse the `linear-gradient(135deg,#a 0%,#b 100%)`
  // string we already use in CSS so the Rust palette stays the source
  // of truth and the canvas matches the live overlay 1:1.
  const linear = parseLinearGradient(accent.base);
  const grad = ctx.createLinearGradient(0, 0, W, H);
  grad.addColorStop(0, linear[0]);
  grad.addColorStop(1, linear[1]);
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, W, H);

  // First radial glow — top-left bias matches the overlay.
  const r1 = ctx.createRadialGradient(W * 0.3, H * 0.2, 0, W * 0.3, H * 0.2, W * 0.7);
  r1.addColorStop(0, accent.glow);
  r1.addColorStop(0.6, "rgba(0,0,0,0)");
  ctx.fillStyle = r1;
  ctx.fillRect(0, 0, W, H);

  // Second radial glow — bottom-right.
  const r2 = ctx.createRadialGradient(W * 0.8, H * 0.7, 0, W * 0.8, H * 0.7, W * 0.7);
  r2.addColorStop(0, accent.glow2);
  r2.addColorStop(0.6, "rgba(0,0,0,0)");
  ctx.fillStyle = r2;
  ctx.fillRect(0, 0, W, H);
}

function parseLinearGradient(raw: string): [string, string] {
  // Best-effort parse of `linear-gradient(135deg,#xxx 0%,#yyy 100%)`.
  // Defaults to a safe dark→darker fallback when the format drifts.
  const match = raw.match(/#[0-9a-f]{3,8}/gi);
  if (match && match.length >= 2) return [match[0], match[1]];
  return ["#1d0e3a", "#3a1052"];
}

function drawCard(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
) {
  ctx.fillStyle = "rgba(255,255,255,0.1)";
  roundedRect(ctx, x, y, w, h, 24);
  ctx.fill();
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
  ctx.drawImage(img, x, y, w, h);
  ctx.restore();
}

function drawBlankTile(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.fillStyle = "rgba(255,255,255,0.2)";
  roundedRect(ctx, x, y, w, h, r);
  ctx.fill();
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    // Required so the canvas isn't tainted when reading the asset
    // protocol or Deezer CDN — both are cross-origin from the WebView's
    // perspective even though Tauri allows-listed them.
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = (e) => reject(e);
    img.src = src;
  });
}

function truncate(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
): string {
  // Cheap iterative truncation — bails out as soon as the current
  // string fits. For the row labels we're rendering this is well under
  // 60 chars, so the O(n) trim is fine.
  if (ctx.measureText(text).width <= maxWidth) return text;
  let s = text;
  while (s.length > 1 && ctx.measureText(s + "…").width > maxWidth) {
    s = s.slice(0, -1);
  }
  return s + "…";
}

function measureWidth(
  ctx: CanvasRenderingContext2D,
  text: string,
  font?: string,
): number {
  if (font) {
    const previous = ctx.font;
    ctx.font = font;
    const w = ctx.measureText(text).width;
    ctx.font = previous;
    return w;
  }
  return ctx.measureText(text).width;
}

function drawSparkle(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
) {
  // A small 4-point star to echo the Lucide Sparkles icon used in the
  // overlay's intro slide. Drawn as a path so it scales cleanly.
  ctx.save();
  ctx.translate(x, y);
  ctx.fillStyle = "rgba(255,255,255,0.85)";
  ctx.beginPath();
  const size = 24;
  ctx.moveTo(0, -size);
  ctx.quadraticCurveTo(0, 0, size, 0);
  ctx.quadraticCurveTo(0, 0, 0, size);
  ctx.quadraticCurveTo(0, 0, -size, 0);
  ctx.quadraticCurveTo(0, 0, 0, -size);
  ctx.closePath();
  ctx.fill();
  ctx.restore();
}
