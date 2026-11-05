import { useTranslation } from "react-i18next";

import type {
  PodcastChannel,
  PodcastEpisode,
} from "../../../lib/tauri/podcast";
import { channelTitle, formatDate, formatLength, isInProgress } from "./format";

/** Why the play button is off, or `null` when it is on. */
export function playBlocker(
  episode: PodcastEpisode,
  network: boolean,
): string | null {
  if (episode.downloaded) return null;
  if (!episode.has_enclosure) return "podcasts.episode.noAddress";
  return network ? null : "podcasts.episode.needsNetwork";
}

/** Date, length, what is left. Tabular, in the row's quiet line. */
export function useEpisodeMeta(
  episode: PodcastEpisode,
  channel?: PodcastChannel,
) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;
  const parts: string[] = [];
  if (channel) parts.push(channelTitle(t, channel));
  if (episode.season != null && episode.episode_number != null) {
    parts.push(
      t("podcasts.episode.seasonEpisode", {
        season: episode.season,
        episode: episode.episode_number,
      }),
    );
  } else if (episode.episode_number != null) {
    parts.push(
      t("podcasts.episode.number", { episode: episode.episode_number }),
    );
  }
  if (episode.published_at != null) {
    parts.push(formatDate(episode.published_at, locale));
  }
  if (episode.duration_ms != null) {
    parts.push(
      isInProgress(episode) && episode.duration_ms > episode.position_ms
        ? t("podcasts.episode.remaining", {
            time: formatLength(
              episode.duration_ms - episode.position_ms,
              locale,
            ),
          })
        : formatLength(episode.duration_ms, locale),
    );
  }
  return parts;
}

export function progressOf(episode: PodcastEpisode): number | null {
  if (!isInProgress(episode) || !episode.duration_ms) return null;
  return Math.min(100, (episode.position_ms / episode.duration_ms) * 100);
}
