/** Format a duration in milliseconds as `Xh Ym` (or `Ym Xs` under an hour). */
export function formatListenTime(ms: number): string {
  if (ms <= 0) return "0m";
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
}

/** Format a 0..1 ratio as a localized percentage. */
export function formatPercent(ratio: number, locale: string): string {
  return new Intl.NumberFormat(locale, {
    style: "percent",
    maximumFractionDigits: 0,
  }).format(Math.max(0, Math.min(1, ratio)));
}

/** Format an integer count with thousands separator. */
export function formatCount(n: number, locale: string): string {
  return new Intl.NumberFormat(locale).format(n);
}

/** Parse a "YYYY-MM-DD" day key into a short, locale-aware label. */
export function formatDayShort(dayKey: string, locale: string): string {
  // Treat as local date by adding a noon offset to avoid TZ edge-cases.
  const d = new Date(`${dayKey}T12:00:00`);
  if (Number.isNaN(d.getTime())) return dayKey;
  return new Intl.DateTimeFormat(locale, {
    day: "2-digit",
    month: "short",
  }).format(d);
}
