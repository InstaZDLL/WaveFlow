/**
 * Extract the dominant colour of an image via canvas pixel sampling.
 *
 * Loads the URL into an `Image` (cross-origin ok for `tauri://` since
 * everything is local), draws it onto a tiny offscreen canvas, then
 * averages every Nth pixel skipping near-black / near-white runs so
 * the result reflects the cover's "feel" rather than its corners.
 *
 * Cheap enough (~5 ms on a 64×64 sample) to run on every track
 * change without throttling.
 */
export async function dominantColor(
  url: string,
): Promise<{ r: number; g: number; b: number }> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => {
      const SIZE = 64;
      const canvas = document.createElement("canvas");
      canvas.width = SIZE;
      canvas.height = SIZE;
      const ctx = canvas.getContext("2d", { willReadFrequently: true });
      if (!ctx) {
        reject(new Error("canvas 2d context unavailable"));
        return;
      }
      ctx.drawImage(img, 0, 0, SIZE, SIZE);
      const { data } = ctx.getImageData(0, 0, SIZE, SIZE);

      let r = 0;
      let g = 0;
      let b = 0;
      let count = 0;
      // Sample every 4th pixel — at 64×64 that's still 1024 samples.
      for (let i = 0; i < data.length; i += 16) {
        const pr = data[i];
        const pg = data[i + 1];
        const pb = data[i + 2];
        const pa = data[i + 3];
        if (pa < 128) continue;
        // Skip near-monochrome pixels (white margins, black bars) so
        // they don't flatten the average toward grey.
        const max = Math.max(pr, pg, pb);
        const min = Math.min(pr, pg, pb);
        if (max - min < 24 && (max < 32 || max > 224)) continue;
        r += pr;
        g += pg;
        b += pb;
        count += 1;
      }
      if (count === 0) {
        resolve({ r: 39, g: 39, b: 42 }); // tailwind zinc-800 fallback
        return;
      }
      resolve({
        r: Math.round(r / count),
        g: Math.round(g / count),
        b: Math.round(b / count),
      });
    };
    img.onerror = (e) => reject(e);
    img.src = url;
  });
}

/**
 * Darken an `(r,g,b)` triple by a uniform factor so the resulting
 * gradient stays legible behind white text. Used for the bottom
 * stop of the mini-player background gradient.
 */
export function darken(
  rgb: { r: number; g: number; b: number },
  factor: number,
): { r: number; g: number; b: number } {
  const f = Math.max(0, Math.min(1, factor));
  return {
    r: Math.round(rgb.r * f),
    g: Math.round(rgb.g * f),
    b: Math.round(rgb.b * f),
  };
}

export function rgb({
  r,
  g,
  b,
}: {
  r: number;
  g: number;
  b: number;
}): string {
  return `rgb(${r}, ${g}, ${b})`;
}
