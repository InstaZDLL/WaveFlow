import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  PODCAST_UPDATED,
  podcastDownloadUsage,
  podcastErrorWord,
  podcastListChannels,
  podcastToListen,
  type PodcastChannel,
  type PodcastDownloadUsage,
  type PodcastEpisode,
} from "../lib/tauri/podcast";

/** A pass over a long list emits for each channel: a burst is one re-read. */
const RELOAD_DELAY_MS = 400;

export interface PodcastLibrary {
  channels: PodcastChannel[];
  toListen: PodcastEpisode[];
  usage: PodcastDownloadUsage | null;
  loading: boolean;
  /** The channels could not be read: `true`, with the word when it had one. */
  failed: boolean;
  failedWord: string | null;
  /** "To listen" could not be read, while the channels could. */
  toListenFailed: boolean;
  /** Resolves once the channels have been read, for what opens one just added. */
  reload: () => Promise<void>;
}

/**
 * What the podcasts page reads: both tables, nothing over the network, so this
 * holds with the switch off too. Subscribes first, then reads, so a pass landing
 * between the two is heard rather than lost (the "Events" invariant).
 */
export function usePodcastLibrary(): PodcastLibrary {
  const [channels, setChannels] = useState<PodcastChannel[]>([]);
  const [toListen, setToListen] = useState<PodcastEpisode[]>([]);
  const [usage, setUsage] = useState<PodcastDownloadUsage | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [failedWord, setFailedWord] = useState<string | null>(null);
  const [toListenFailed, setToListenFailed] = useState(false);
  const readSeq = useRef(0);

  const reload = useCallback(() => {
    const seq = ++readSeq.current;
    const current = () => seq === readSeq.current;
    const channelsRead = podcastListChannels()
      .then((rows) => {
        if (!current()) return;
        setChannels(rows);
        setFailed(false);
        setFailedWord(null);
      })
      .catch((err) => {
        console.error("[podcasts] listing the channels failed", err);
        if (!current()) return;
        setFailed(true);
        setFailedWord(podcastErrorWord(err));
      })
      .finally(() => {
        if (current()) setLoading(false);
      });
    podcastToListen()
      .then((rows) => {
        if (!current()) return;
        setToListen(rows);
        setToListenFailed(false);
      })
      .catch((err) => {
        console.warn("[podcasts] reading the list to listen to failed", err);
        if (current()) setToListenFailed(true);
      });
    podcastDownloadUsage()
      .then((value) => {
        if (current()) setUsage(value);
      })
      .catch((err) => {
        console.warn("[podcasts] reading the download usage failed", err);
      });
    return channelsRead;
  }, []);

  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const unlisteners: Array<() => void> = [];
    const later = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void reload(), RELOAD_DELAY_MS);
    };
    listen(PODCAST_UPDATED, later)
      .then((off) => {
        if (cancelled) off();
        else unlisteners.push(off);
      })
      .catch((err) => console.warn("[podcasts] listen failed", err))
      .finally(() => {
        if (!cancelled) void reload();
      });
    return () => {
      cancelled = true;
      readSeq.current += 1;
      window.clearTimeout(timer);
      unlisteners.forEach((off) => off());
    };
  }, [reload]);

  return {
    channels,
    toListen,
    usage,
    loading,
    failed,
    failedWord,
    toListenFailed,
    reload,
  };
}
