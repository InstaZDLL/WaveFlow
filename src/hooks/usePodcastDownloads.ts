import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  PODCAST_DOWNLOAD,
  type PodcastDownloadPayload,
} from "../lib/tauri/podcast";

/**
 * The downloads moving right now, by episode. A row holds its own `downloaded`
 * once a re-read has brought it, so one that ended leaves this map. Calls
 * `onFailed` with the word of one that failed.
 */
export function usePodcastDownloads(
  onFailed: (word: string | null) => void,
): ReadonlyMap<number, PodcastDownloadPayload> {
  const [active, setActive] = useState<
    ReadonlyMap<number, PodcastDownloadPayload>
  >(new Map());

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    listen<PodcastDownloadPayload>(PODCAST_DOWNLOAD, ({ payload }) => {
      const moving = payload.state === "queued" || payload.state === "running";
      setActive((previous) => {
        const next = new Map(previous);
        if (moving) next.set(payload.episodeId, payload);
        else next.delete(payload.episodeId);
        return next;
      });
      if (payload.state === "failed") onFailed(payload.errorWord);
    })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[podcasts] listen failed", err));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [onFailed]);

  return active;
}
