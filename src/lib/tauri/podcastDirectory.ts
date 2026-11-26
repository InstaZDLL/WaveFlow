import { invoke } from "./invoke";
import type { PodcastChannel } from "./podcast";

/**
 * The podcast directory. Mirrors `commands::podcast_directory`.
 *
 * A show comes with its id in the directory and never with its feed's address:
 * subscribing asks by that id. A refusal rejects with `podcast: <word>`, read
 * by `podcastErrorWord`.
 */

/** One show of a chart or a search. */
export interface DirectoryShow {
  id: number;
  title: string;
  author: string | null;
  /** One of `PODCAST_GENRES`, or none the app knows. */
  genre_id: number | null;
  /** Whether {@link podcastDirectoryArtwork} may have a picture for it. */
  has_artwork: boolean;
  episode_count: number | null;
  explicit: boolean;
  /** The channel it already is in this profile. */
  channel_id: number | null;
}

/** `country`'s chart (two letters), narrowed to `genre` when there is one. */
export function podcastDirectoryTop(
  country: string,
  genre: number | null,
): Promise<DirectoryShow[]> {
  return invoke<DirectoryShow[]>("podcast_directory_top", { country, genre });
}

/** The shows a search for `term` finds in `country`'s storefront. */
export function podcastDirectorySearch(
  country: string,
  term: string,
): Promise<DirectoryShow[]> {
  return invoke<DirectoryShow[]>("podcast_directory_search", { country, term });
}

/** A show's picture as a `data:` address, `null` when there is none. */
export function podcastDirectoryArtwork(id: number): Promise<string | null> {
  return invoke<string | null>("podcast_directory_artwork", { id });
}

/** Subscribes to a show by its id; resolves with its channel. */
export function podcastDirectorySubscribe(
  country: string,
  id: number,
): Promise<PodcastChannel> {
  return invoke<PodcastChannel>("podcast_directory_subscribe", { country, id });
}
