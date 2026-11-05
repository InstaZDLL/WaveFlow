import { useCallback, useEffect, useState } from "react";

import { useProfile } from "./useProfile";
import {
  podcastEpisodeChapters,
  podcastErrorWord,
  type PodcastChapter,
  type PodcastEpisodeChapters,
} from "../lib/tauri/podcast";

/** Asked for once at a time: the page, the player's list and the "⋯" menu may all
 *  open one episode together, and the first answer is what the others would get. */
const inFlight = new Map<string, Promise<PodcastEpisodeChapters>>();

function ask(
  profileId: number | null,
  episodeId: number,
  again: boolean,
): Promise<PodcastEpisodeChapters> {
  const key = `${profileId ?? 0}:${episodeId}`;
  const running = inFlight.get(key);
  if (running) return running;
  const request = podcastEpisodeChapters(episodeId, again).finally(() =>
    inFlight.delete(key),
  );
  inFlight.set(key, request);
  return request;
}

interface Read {
  key: string;
  chapters: PodcastChapter[];
  /** A word from the list, or `null` for an episode that simply has none. */
  failure: string | null;
}

/**
 * An episode's chapters. Asking is what opening an episode means here: the backend
 * answers what it kept with nobody contacted, and only otherwise fetches.
 */
export function useEpisodeChapters(episodeId: number | null) {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const [attempt, setAttempt] = useState(0);
  const [read, setRead] = useState<Read | null>(null);
  const key = `${profileId ?? 0}:${episodeId}:${attempt}`;

  useEffect(() => {
    if (episodeId == null) return;
    let cancelled = false;
    ask(profileId, episodeId, attempt > 0)
      .then((answer) => {
        if (!cancelled) {
          setRead({
            key,
            chapters: answer.chapters,
            failure: answer.failure,
          });
        }
      })
      .catch((err) => {
        const word = podcastErrorWord(err);
        if (word == null) console.error("[podcasts] chapters failed", err);
        if (!cancelled) setRead({ key, chapters: [], failure: word ?? "" });
      });
    return () => {
      cancelled = true;
    };
  }, [episodeId, profileId, key, attempt]);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  const settled = read?.key === key ? read : null;
  return {
    chapters: settled?.chapters ?? [],
    failure: settled?.failure ?? null,
    loading: episodeId != null && settled == null,
    retry,
  };
}
