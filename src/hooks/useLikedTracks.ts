import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { listLikedTrackIds, toggleLikeTrack } from "../lib/tauri/track";

export interface LikedChangedPayload {
  trackId: number;
  liked: boolean;
}

/**
 * Subscribe to `track:liked-changed` for the lifetime of the host
 * component — the heart moved somewhere, possibly in the other window.
 *
 * Same contract as [`useTrackUpdated`](./useTrackUpdated.ts): memoize
 * the callback, or the subscription is torn down and rebuilt on every
 * render.
 */
export function useLikedChanged(
  callback: (payload: LikedChangedPayload) => void,
): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const off = await listen<LikedChangedPayload>(
          "track:liked-changed",
          (e) => callback(e.payload),
        );
        // Cleanup may have run before `listen()` resolved.
        if (cancelled) off();
        else unlisten = off;
      } catch (err) {
        console.error("[useLikedChanged] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [callback]);
}

/**
 * Liked-track ids for the surfaces that render a single heart — the
 * player bar and the mini-player.
 *
 * Both used to keep this set themselves, with the same fetch-on-mount,
 * refetch-on-track-change and optimistic-toggle logic. They live in
 * different webviews, so the copies drifted: liking from one left the
 * other showing an empty heart until the track changed (#523). The set
 * now follows the backend's `track:liked-changed` broadcast, and lives
 * here once so the two can't fall out of step again.
 *
 * The event carries the new state rather than a "refetch" signal, so a
 * like never costs a round-trip in the windows that didn't ask for it.
 */
export function useLikedTracks(currentTrackId: number | null | undefined) {
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());

  // Full list on mount, and again when the track changes — the user may
  // have liked this one from a library view, which has its own heart.
  useEffect(() => {
    let cancelled = false;
    listLikedTrackIds()
      .then((ids) => {
        if (!cancelled) setLikedIds(new Set(ids));
      })
      .catch((err) =>
        console.error("[useLikedTracks] initial fetch failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [currentTrackId]);

  const apply = useCallback((trackId: number, liked: boolean) => {
    setLikedIds((prev) => {
      if (prev.has(trackId) === liked) return prev;
      const next = new Set(prev);
      if (liked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  }, []);

  useLikedChanged(
    useCallback(
      (payload: LikedChangedPayload) => apply(payload.trackId, payload.liked),
      [apply],
    ),
  );

  /**
   * Flip the heart. The optimistic update is applied from the command's
   * own return value rather than assumed: the backend refuses to like a
   * row the scanner removed underneath us, and returns the real state.
   * The broadcast echoes back here too and lands as a no-op.
   */
  const toggleLike = useCallback(
    async (trackId: number) => {
      try {
        apply(trackId, await toggleLikeTrack(trackId));
      } catch (err) {
        console.error("[useLikedTracks] toggle failed", err);
      }
    },
    [apply],
  );

  return { likedIds, toggleLike };
}
