import { useCallback, useEffect, useRef, useState } from "react";
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

  // Serialises writes: a rapid second toggle while one is in flight is
  // ignored, so concurrent upserts can't land out of order and leave the
  // backend misaligned with the in-memory channel.
  const writing = useRef(false);

  const setChannel = useCallback(
    async (next: UpdateChannel) => {
      const previous = channel;
      // Already in the desired state — idempotent success, nothing to persist.
      if (next === previous) return;
      // A write is in flight: reject so the caller skips post-success
      // effects (the updater re-check) for a change that didn't apply,
      // rather than mistaking the busy-skip for a successful write.
      if (writing.current) {
        throw new Error("updater channel write already in progress");
      }
      writing.current = true;
      setChannelState(next); // optimistic
      try {
        await setUpdateChannel(next);
      } catch (err) {
        console.error("[useUpdateChannel] write failed", err);
        setChannelState(previous); // roll back on failure
        // Re-throw so callers can skip post-success effects (e.g. the
        // Settings card's updater re-check) when the write didn't land.
        throw err;
      } finally {
        writing.current = false;
      }
    },
    [channel],
  );

  return { channel, loaded, setChannel };
}
