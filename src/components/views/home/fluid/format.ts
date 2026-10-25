import { resolveArtwork } from "../../../../lib/tauri/artwork";

/** `m:ss`, the player bar's form. */
export function clock(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** "Dimanche 18 octobre", first letter raised whatever the locale does. */
export function longDate(locale: string): string {
  const text = new Intl.DateTimeFormat(locale, {
    weekday: "long",
    day: "numeric",
    month: "long",
  }).format(new Date());
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** "il y a 2 h" — `played_at` is in milliseconds. */
export function relativeTime(unix: number, locale: string): string {
  const ms = unix > 1e12 ? unix : unix * 1000;
  const diff = ms - Date.now();
  const abs = Math.abs(diff);
  const rtf = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  if (abs < 3_600_000) return rtf.format(Math.round(diff / 60_000), "minute");
  if (abs < 86_400_000) return rtf.format(Math.round(diff / 3_600_000), "hour");
  if (abs < 30 * 86_400_000)
    return rtf.format(Math.round(diff / 86_400_000), "day");
  return rtf.format(Math.round(diff / (30 * 86_400_000)), "month");
}

/** "FLAC · 24/44.1" — the quality tag the player bar wears. */
export function qualityTag(
  codec: string | null,
  sampleRate: number | null,
  bitDepth: number | null,
): string | null {
  if (!codec) return null;
  const rate = sampleRate
    ? (sampleRate / 1000).toFixed(1).replace(/\.0$/, "")
    : null;
  if (bitDepth && rate) return `${codec} · ${bitDepth}/${rate}`;
  if (rate) return `${codec} · ${rate} kHz`;
  return codec;
}

/** The cover a blurred copy is drawn from: the 128 px thumbnail when
 *  there is one — a blur keeps no detail, and it decodes faster than the
 *  full file. */
export function blurSourceUrl(row: {
  artwork_path: string | null;
  artwork_path_1x?: string | null;
  artwork_path_2x?: string | null;
}): string | null {
  return resolveArtwork(
    {
      full: row.artwork_path,
      x1: row.artwork_path_1x ?? null,
      x2: row.artwork_path_2x ?? null,
    },
    "2x",
  );
}

/** The full-size cover URL of anything that carries the three paths. */
export function coverUrl(row: {
  artwork_path: string | null;
  artwork_path_1x?: string | null;
  artwork_path_2x?: string | null;
}): string | null {
  return resolveArtwork(
    {
      full: row.artwork_path,
      x1: row.artwork_path_1x ?? null,
      x2: row.artwork_path_2x ?? null,
    },
    "full",
  );
}
