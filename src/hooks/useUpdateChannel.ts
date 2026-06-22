import { useCallback, useEffect, useState } from "react";
import {
  getUpdateChannel,
  setUpdateChannel,
  type UpdateChannel,
} from "../lib/tauri/updater";

export interface UpdateChannelState {
  channel: UpdateChannel;
  /** False until the initial backend read resolves. */
  loaded: boolean;
  setChannel: (next: UpdateChannel) => Promise<void>;
}

/**
 * Hook backing the Settings → Diagnostics "beta channel" toggle. The
 * channel is an app-wide preference (`app_setting['updater.channel']`)
 * so there's a single consumer — no shared store needed, unlike the
 * per-badge `useHiResBadgeVisibility`. Defaults to `stable` until the
 * backend read lands.
 */
export function useUpdateChannel(): UpdateChannelState {
  const [channel, setChannelState] = useState<UpdateChannel>("stable");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getUpdateChannel()
      .then((c) => {
        if (!cancelled) {
          setChannelState(c);
          setLoaded(true);
        }
      })
      .catch((err) => {
        console.error("[useUpdateChannel] read failed", err);
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const setChannel = useCallback(
    async (next: UpdateChannel) => {
      const previous = channel;
      if (next === previous) return;
      setChannelState(next); // optimistic
      try {
        await setUpdateChannel(next);
      } catch (err) {
        console.error("[useUpdateChannel] write failed", err);
        setChannelState(previous); // roll back on failure
        // Re-throw so callers can skip post-success effects (e.g. the
        // Settings card's updater re-check) when the write didn't land.
        throw err;
      }
    },
    [channel],
  );

  return { channel, loaded, setChannel };
}
