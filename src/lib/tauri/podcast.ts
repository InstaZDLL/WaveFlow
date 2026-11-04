import { invoke } from "@tauri-apps/api/core";

/**
 * Podcasts (RFC-009). Mirrors `commands::podcast`.
 *
 * Nothing here carries a feed's or an enclosure's address: a row says whether
 * it has one, and the backend opens it by id. A refusal rejects with
 * `podcast: <word>`; see {@link podcastErrorWord}.
 */

/** Emitted when a channel or its episodes changed. */
export const PODCAST_UPDATED = "podcast:updated";

export interface PodcastUpdatedPayload {
  /** `null` when several channels may have changed. */
  channel_id: number | null;
}

/** Emitted as a download moves along. */
export const PODCAST_DOWNLOAD = "podcast:download";

export type PodcastDownloadState =
  "queued" | "running" | "done" | "failed" | "cancelled";

export interface PodcastDownloadPayload {
  episodeId: number;
  state: PodcastDownloadState;
  receivedBytes: number;
  totalBytes: number | null;
  /** One word when `state` is `failed`. */
  errorWord: string | null;
}

export interface PodcastDownloadUsage {
  usedBytes: number;
  quotaBytes: number;
}

/** Mirrors `podcast::store::PodcastChannel`. */
export interface PodcastChannel {
  id: number;
  /** Empty when the feed names none: show a word of your own. */
  title: string;
  /** Plain text. Never markup. */
  description: string | null;
  author: string | null;
  language: string | null;
  /** The feed names a website. Its address stays in the backend, like the others. */
  has_site_url: boolean;
  artwork_hash: string | null;
  /** The picture in the artwork cache, for `convertFileSrc`. */
  artwork_path: string | null;
  /** False in a profile restored from an archive, which carries no address.
   *  Nothing refreshes until the feed is subscribed to again, which finds this
   *  channel and keeps every position. */
  has_feed_address: boolean;
  episode_count: number;
  unplayed_count: number;
  last_refreshed_at: number | null;
  /** One word from `FetchFailure::code`; `null` after a success. */
  last_error: string | null;
  /** The speed this channel's episodes start at; `null` is the normal one. */
  playback_speed: number | null;
  created_at: number;
}

/** Mirrors `podcast::store::PodcastEpisode`. */
export interface PodcastEpisode {
  id: number;
  channel_id: number;
  /** Empty when the feed names none: show a word of your own. */
  title: string;
  /** Plain text. Never markup. */
  description: string | null;
  published_at: number | null;
  duration_ms: number | null;
  season: number | null;
  episode_number: number | null;
  enclosure_type: string | null;
  enclosure_length: number | null;
  /** There is an address to stream from. False in a restored profile until the
   *  feed is subscribed to again. */
  has_enclosure: boolean;
  position_ms: number;
  played_at: number | null;
  last_opened_at: number | null;
  downloaded: boolean;
  download_size: number | null;
  has_chapters: boolean;
  has_transcript: boolean;
}

/** How a pass over several channels ended. */
export type PodcastPassOutcome =
  | "done"
  | "cancelled"
  | "offline"
  | "disabled"
  | "profile_changed"
  | "already_running";

export interface PodcastRefreshSummary {
  outcome: PodcastPassOutcome;
  refreshed: number;
  unchanged: number;
  failed: number;
}

export interface PodcastOpmlFailure {
  title: string | null;
  code: string;
}

export interface PodcastOpmlImportReport {
  outcome: PodcastPassOutcome;
  added: number;
  already: number;
  failed: PodcastOpmlFailure[];
  total: number;
}

export interface PodcastOpmlExportReport {
  written: number;
  /** Channels a restored archive left without an address. */
  without_address: number;
}

const ERROR_PREFIX = "podcast: ";

/**
 * The word a refused command rejected with: `disabled`, `offline`,
 * `profile_changed`, `channel_gone`, `open_failed`, or one from `FetchFailure::code` (`address_refused`,
 * `too_many_redirects`, `too_large`, `unreached`, `http_<status>`,
 * `invalid_feed`, `not_audio`, `quota_exceeded`). `null` for any other error.
 */
export function podcastErrorWord(error: unknown): string | null {
  const text = String(error);
  return text.startsWith(ERROR_PREFIX) ? text.slice(ERROR_PREFIX.length) : null;
}

/** Fetches the feed and follows it. Rejects, and stores nothing, when it is not
 *  a feed. Resolves with the existing channel when already followed. */
export function podcastSubscribe(feedUrl: string): Promise<PodcastChannel> {
  return invoke<PodcastChannel>("podcast_subscribe", { feedUrl });
}

/** Stops following a channel and forgets its episodes. */
export function podcastUnsubscribe(channelId: number): Promise<void> {
  return invoke("podcast_unsubscribe", { channelId });
}

export function podcastListChannels(): Promise<PodcastChannel[]> {
  return invoke<PodcastChannel[]>("podcast_list_channels");
}

/** A page of a channel's episodes, newest first. */
export function podcastListEpisodes(
  channelId: number,
  limit?: number,
  offset?: number,
): Promise<PodcastEpisode[]> {
  return invoke<PodcastEpisode[]>("podcast_list_episodes", {
    channelId,
    limit,
    offset,
  });
}

/** Asks one channel, or all of them when none is given, for its feed. */
export function podcastRefresh(
  channelId?: number,
): Promise<PodcastRefreshSummary> {
  return invoke<PodcastRefreshSummary>("podcast_refresh", { channelId });
}

/** Subscribes to every feed of an `.opml` or `.xml` file, one at a time. */
export function podcastImportOpml(
  path: string,
): Promise<PodcastOpmlImportReport> {
  return invoke<PodcastOpmlImportReport>("podcast_import_opml", { path });
}

/** Writes every feed's address to the file at `path`. The one place addresses
 *  leave the profile, so the dialog calling this must say so. */
export function podcastExportOpml(
  path: string,
): Promise<PodcastOpmlExportReport> {
  return invoke<PodcastOpmlExportReport>("podcast_export_opml", { path });
}

/** Opens the channel's website in the person's browser. Its address stays in
 *  the backend, which checks it is an http or https one. */
export function podcastOpenSite(channelId: number): Promise<void> {
  return invoke("podcast_open_site", { channelId });
}

/** Starts an episode, from its position; a played one starts over. */
export function podcastPlayEpisode(episodeId: number): Promise<void> {
  return invoke("podcast_play_episode", { episodeId });
}

export function podcastMarkPlayed(
  episodeId: number,
  played: boolean,
): Promise<void> {
  return invoke("podcast_mark_played", { episodeId, played });
}

export function podcastMarkChannelPlayed(
  channelId: number,
  played: boolean,
): Promise<void> {
  return invoke("podcast_mark_channel_played", { channelId, played });
}

/** One episode per channel: the one in progress first, else the newest unplayed. */
export function podcastToListen(): Promise<PodcastEpisode[]> {
  return invoke<PodcastEpisode[]>("podcast_to_listen");
}

/** `null` puts the channel back on the normal speed. */
export function podcastSetChannelSpeed(
  channelId: number,
  speed: number | null,
): Promise<void> {
  return invoke("podcast_set_channel_speed", { channelId, speed });
}

export function podcastDownloadEpisode(episodeId: number): Promise<void> {
  return invoke("podcast_download_episode", { episodeId });
}

export function podcastCancelDownload(episodeId: number): Promise<void> {
  return invoke("podcast_cancel_download", { episodeId });
}

/** Removes the file and keeps the row. */
export function podcastDeleteDownload(episodeId: number): Promise<void> {
  return invoke("podcast_delete_download", { episodeId });
}

export function podcastDownloadUsage(): Promise<PodcastDownloadUsage> {
  return invoke<PodcastDownloadUsage>("podcast_download_usage");
}
