import type { Lang } from "./copy";

export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

export function hours(ms: number, lang: Lang): string {
  const h = Math.round(ms / 3_600_000);
  return `${new Intl.NumberFormat(lang).format(h)} h`;
}

export function integer(n: number, lang: Lang): string {
  return new Intl.NumberFormat(lang).format(n);
}

export function relativeTime(unix: number, lang: Lang): string {
  // `play_event.played_at` is stored in milliseconds; older rows and the
  // album `added_at` are in seconds. Tell them apart by magnitude.
  const ms = unix > 1e12 ? unix : unix * 1000;
  const diff = ms - Date.now();
  const abs = Math.abs(diff);
  const rtf = new Intl.RelativeTimeFormat(lang, { numeric: "auto" });
  if (abs < 3_600_000) return rtf.format(Math.round(diff / 60_000), "minute");
  if (abs < 86_400_000) return rtf.format(Math.round(diff / 3_600_000), "hour");
  if (abs < 30 * 86_400_000)
    return rtf.format(Math.round(diff / 86_400_000), "day");
  return rtf.format(Math.round(diff / (30 * 86_400_000)), "month");
}

export function longDate(lang: Lang): string {
  return new Intl.DateTimeFormat(lang, {
    weekday: "long",
    day: "numeric",
    month: "long",
  }).format(new Date());
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
