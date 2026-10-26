import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { playerGetQueue, type PlayerQueueSnapshot } from "../lib/tauri/player";

/**
 * The local play queue, re-read on every `player:queue-changed`. Shared by
 * the immersive view's Queue and Tracklist tabs, which show the same
 * snapshot two ways. `null` until the first read lands.
 */
export function usePlayerQueue(): PlayerQueueSnapshot | null {
  const [snapshot, setSnapshot] = useState<PlayerQueueSnapshot | null>(null);

  // Seq guard so overlapping refetches (rapid Next clicks fire many
  // `player:queue-changed` events) never resolve out of order.
  const fetchSeqRef = useRef(0);
  const doFetch = useCallback(() => {
    const seq = ++fetchSeqRef.current;
    playerGetQueue()
      .then((q) => {
        if (seq === fetchSeqRef.current) setSnapshot(q);
      })
      .catch((err) => {
        // A transient refetch failure shouldn't blank the last known
        // queue — just log and keep the previous snapshot on screen.
        console.error("[usePlayerQueue] fetch failed", err);
      });
  }, []);

  useEffect(() => {
    doFetch();
    let unlisten: UnlistenFn | null = null;
    // Guard against the component unmounting before `listen` resolves —
    // otherwise `unlisten` is still null when cleanup runs and the
    // subscription leaks. If disposed by the time it resolves, tear it
    // down immediately.
    let disposed = false;
    (async () => {
      try {
        const fn = await listen("player:queue-changed", doFetch);
        if (disposed) fn();
        else unlisten = fn;
      } catch (err) {
        console.error("[usePlayerQueue] listen failed", err);
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [doFetch]);

  return snapshot;
}
