import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  PODCAST_DOWNLOAD,
  podcastDownloadQueue,
  type PodcastDownloadPayload,
} from "../lib/tauri/podcast";

/**
 * The downloads moving right now, by episode. A row holds its own `downloaded`
 * once a re-read has brought it, so one that ended leaves this map. Calls
 * `onFailed` with the word of one that failed, and `onSettled` for any that ended,
 * which a list has to read its rows again for.
 */
export function usePodcastDownloads(
  onFailed: (word: string | null) => void,
  onSettled: () => void,
): ReadonlyMap<number, PodcastDownloadPayload> {
  const [active, setActive] = useState<
    ReadonlyMap<number, PodcastDownloadPayload>
  >(new Map());

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    // Ended since listening began: the queue, read a moment earlier, may still name
    // them, and no later event would take them off again.
    const ended = new Set<number>();
    listen<PodcastDownloadPayload>(PODCAST_DOWNLOAD, ({ payload }) => {
      const moving = payload.state === "queued" || payload.state === "running";
      if (moving) ended.delete(payload.episodeId);
      else ended.add(payload.episodeId);
      setActive((previous) => {
        const next = new Map(previous);
        if (moving) next.set(payload.episodeId, payload);
        else next.delete(payload.episodeId);
        return next;
      });
      if (payload.state === "failed") onFailed(payload.errorWord);
      if (!moving) onSettled();
    })
      .then((unlisten) => {
        if (cancelled) {
          unlisten();
          return;
        }
        off = unlisten;
        // What moved while the page was closed: no event replays for it.
        return podcastDownloadQueue().then((queue) => {
          if (cancelled) return;
          setActive((previous) => {
            const next = new Map(previous);
            for (const payload of queue) {
              if (
                !next.has(payload.episodeId) &&
                !ended.has(payload.episodeId)
              ) {
                next.set(payload.episodeId, payload);
              }
            }
            return next;
          });
        });
      })
      .catch((err) => console.warn("[podcasts] downloads read failed", err));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [onFailed, onSettled]);

  return active;
}
