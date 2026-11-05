import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { podcastErrorWord } from "../lib/tauri/podcast";
import {
  PODCAST_SERVER_UPDATED,
  podcastServerChannel,
  podcastServerOverview,
  type PodcastServerUpdatedPayload,
  type ServerChannel,
  type ServerOverview,
} from "../lib/tauri/podcastServer";

/** How often an open channel is read while the server downloads one of its episodes:
 *  nothing tells this side when it is done. */
const WHILE_DOWNLOADING_MS = 5_000;

/** Fires when the binding may have changed. A sign-in waits on the browser for minutes
 *  and lands whichever page is open by then, with the same profile, so nothing else
 *  tells a page that was read as somebody else, or as nobody. */
function useRemoteChanged(onChange: () => void) {
  useEffect(() => {
    window.addEventListener("waveflow:remote-changed", onChange);
    return () => window.removeEventListener("waveflow:remote-changed", onChange);
  }, [onChange]);
}

/**
 * The server's section of the page, read when the page opens, when it moves between
 * the overview and a channel (`place`), when the binding changes, and when asked.
 * `null` is no section at all, for any reason, and never an error on the page. Not read
 * with the switch off or offline, which the backend refuses in any case: the state then
 * goes back to `null` rather than showing what was read before.
 */
export function usePodcastServerOverview(wanted: boolean, place: string | null) {
  const [overview, setOverview] = useState<ServerOverview | null>(null);
  const readSeq = useRef(0);

  const reload = useCallback(() => {
    const seq = ++readSeq.current;
    return podcastServerOverview()
      .then((answer) => {
        if (seq === readSeq.current) setOverview(answer);
      })
      .catch(() => {
        // An older build with no such command, or no profile: no section.
        if (seq === readSeq.current) setOverview(null);
      });
  }, []);

  useEffect(() => {
    if (wanted) void reload();
    return () => {
      readSeq.current += 1;
    };
  }, [wanted, place, reload]);

  const reread = useCallback(() => {
    if (wanted) void reload();
  }, [wanted, reload]);
  useRemoteChanged(reread);

  return { overview: wanted ? overview : null, reload };
}

/** One channel of the server, read when it opens, and again when a position written
 *  from here, a download over there or a change of binding may have changed it. What
 *  was read stays through a failed read, except one saying the account is signed out:
 *  its rows are then nobody's to act on. */
export function usePodcastServerChannel(channelId: string, revision: number) {
  const [channel, setChannel] = useState<ServerChannel | null>(null);
  const [loading, setLoading] = useState(true);
  const [failedWord, setFailedWord] = useState<string | null | undefined>(
    undefined,
  );
  const readSeq = useRef(0);

  const reload = useCallback(() => {
    const seq = ++readSeq.current;
    podcastServerChannel(channelId)
      .then((answer) => {
        if (seq !== readSeq.current) return;
        setChannel(answer);
        setFailedWord(undefined);
      })
      .catch((err) => {
        if (seq !== readSeq.current) return;
        const word = podcastErrorWord(err);
        if (word === "server_signed_out") setChannel(null);
        setFailedWord(word);
      })
      .finally(() => {
        if (seq === readSeq.current) setLoading(false);
      });
  }, [channelId]);

  useEffect(() => {
    reload();
    return () => {
      readSeq.current += 1;
    };
  }, [reload, revision]);

  useRemoteChanged(reload);

  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    listen<PodcastServerUpdatedPayload>(PODCAST_SERVER_UPDATED, ({ payload }) => {
      if (payload.channel_id === channelId) reload();
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
  }, [channelId, reload]);

  const downloading =
    channel?.episodes?.some((episode) => episode.status === "downloading") ??
    false;
  useEffect(() => {
    if (!downloading) return;
    const timer = window.setInterval(reload, WHILE_DOWNLOADING_MS);
    return () => window.clearInterval(timer);
  }, [downloading, reload]);

  return { channel, loading, failedWord, reload };
}
