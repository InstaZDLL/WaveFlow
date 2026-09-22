import type { TFunction } from "i18next";

/** Aggregate playlist duration; individual tracks still use m:ss. */
export function formatPlaylistDuration(ms: number, t: TFunction): string {
  if (!Number.isFinite(ms) || ms <= 0) return t("playlistDuration.zero");
  const minutes = Math.max(1, Math.floor(ms / 60_000));
  const hours = Math.floor(minutes / 60);
  const remainder = minutes % 60;
  if (hours === 0) return t("playlistDuration.minutes", { count: minutes });
  if (remainder === 0) return t("playlistDuration.hours", { count: hours });
  return t("playlistDuration.hoursMinutes", { hours, minutes: remainder });
}
