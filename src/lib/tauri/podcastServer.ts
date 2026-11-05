import { invoke } from "@tauri-apps/api/core";

/**
 * A bound server's podcasts (RFC-009 decision 11). Mirrors
 * `commands::podcast_server`, and read live: nothing here is kept. A refusal
 * rejects with `podcast: <word>`, as every podcast command does, and nothing here
 * holds an address.
 */

/** Emitted when a position was written to the server at a pause, a seek or an end. */
export const PODCAST_SERVER_UPDATED = "podcast:server-updated";

export interface PodcastServerUpdatedPayload {
  channel_id: string;
}

/** Mirrors `remote::podcasts::ServerEpisode`. */
export interface ServerEpisode {
  id: string;
  channel_id: string;
  channel_title: string;
  artwork_hash: string | null;
  /** Empty when the feed names none: show a word of your own. */
  title: string;
  description: string | null;
  published_at: number | null;
  duration_ms: number | null;
  /** Only `completed` plays: the server downloads, and never relays. */
  status: "new" | "downloading" | "completed" | "error" | "deleted";
  /** Why its download failed, as a word. */
  error: string | null;
  size: number | null;
  content_type: string | null;
  /** Where this account stopped. `null` is an episode never started and one
   *  finished alike: the server keeps no played state. */
  position_ms: number | null;
}

/** Mirrors `remote::podcasts::ServerChannel`. */
export interface ServerChannel {
  id: string;
  /** Empty when the feed names none: show a word of your own. */
  title: string;
  description: string | null;
  artwork_hash: string | null;
  /** Why the server's last fetch failed, as a word. */
  error: string | null;
  refreshed_at: number | null;
  episode_count: number;
  /** On a read of one channel, newest first. */
  episodes?: ServerEpisode[];
}

/** Mirrors `remote::podcasts::ServerOverview`. */
export interface ServerOverview {
  enabled: boolean;
  /** Whether this account may subscribe, refresh, download and delete. A refusal
   *  reads as 404 over there, so no role is ever inferred from one. */
  can_manage: boolean;
  channels: ServerChannel[];
}

/** The server's channels, or `null` when there is no section to show: switched off
 *  here, offline, no server, signed out, off over there, unreachable. */
export function podcastServerOverview(): Promise<ServerOverview | null> {
  return invoke<ServerOverview | null>("podcast_server_overview");
}

/** One channel and its episodes, read now. */
export function podcastServerChannel(channelId: string): Promise<ServerChannel> {
  return invoke<ServerChannel>("podcast_server_channel", { channelId });
}

/** Plays a stored episode from where this account stopped, with its channel's stored
 *  episodes as the queue. Rejects with `not_stored` for any other. */
export function podcastServerPlay(
  channelId: string,
  episodeId: string,
): Promise<void> {
  return invoke("podcast_server_play", { channelId, episodeId });
}

/** Forgets where this account stopped in an episode. */
export function podcastServerClearPosition(episodeId: string): Promise<void> {
  return invoke("podcast_server_clear_position", { episodeId });
}

/** Subscribes the server. It reads the feed first, and stores nothing if it cannot. */
export function podcastServerSubscribe(feedUrl: string): Promise<ServerChannel> {
  return invoke<ServerChannel>("podcast_server_subscribe", { feedUrl });
}

/** Asks the server to check every channel, which it does in the background. */
export function podcastServerRefresh(): Promise<void> {
  return invoke("podcast_server_refresh");
}

/** Unsubscribes the server, for every account on it. */
export function podcastServerUnsubscribe(channelId: string): Promise<void> {
  return invoke("podcast_server_unsubscribe", { channelId });
}

/** Asks the server to keep an episode. Its status says the rest. */
export function podcastServerRequestDownload(episodeId: string): Promise<void> {
  return invoke("podcast_server_request_download", { episodeId });
}

/** Removes an episode and its file from the server, for every account on it. */
export function podcastServerDeleteEpisode(episodeId: string): Promise<void> {
  return invoke("podcast_server_delete_episode", { episodeId });
}
