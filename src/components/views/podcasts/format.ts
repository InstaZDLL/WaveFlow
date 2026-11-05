import type { TFunction } from "i18next";

import type {
  PodcastChannel,
  PodcastEpisode,
} from "../../../lib/tauri/podcast";

/** The parser leaves a title empty when the feed names none: the word is ours. */
export function channelTitle(t: TFunction, channel: PodcastChannel): string {
  return channel.title.trim() || t("podcasts.untitledChannel");
}

export function episodeTitle(t: TFunction, episode: PodcastEpisode): string {
  return episode.title.trim() || t("podcasts.untitledEpisode");
}

/** "1 h 12 min", or "48 min"; a short one still reads as a length. */
export function formatLength(ms: number, locale: string): string {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  const unit = (value: number, name: "hour" | "minute") =>
    new Intl.NumberFormat(locale, {
      style: "unit",
      unit: name,
      unitDisplay: "short",
    }).format(value);
  const hours = Math.floor(minutes / 60);
  if (hours === 0) return unit(minutes, "minute");
  const rest = minutes % 60;
  return rest === 0
    ? unit(hours, "hour")
    : `${unit(hours, "hour")} ${unit(rest, "minute")}`;
}

/** Stored in epoch milliseconds. */
export function formatDate(ms: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(
    new Date(ms),
  );
}

export function formatDateTime(ms: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(ms));
}

export function formatBytes(bytes: number, locale: string): string {
  const units = ["byte", "kilobyte", "megabyte", "gigabyte"] as const;
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit: units[index],
    unitDisplay: "short",
    maximumFractionDigits: index >= 3 ? 1 : 0,
  }).format(value);
}

/** Whether there is anything to play: the file, or an address to stream from. */
export function isPlayable(episode: PodcastEpisode): boolean {
  return episode.downloaded || episode.has_enclosure;
}

export function isInProgress(episode: PodcastEpisode): boolean {
  return episode.played_at == null && episode.position_ms > 0;
}
