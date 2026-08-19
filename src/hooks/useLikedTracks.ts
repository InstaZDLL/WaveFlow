import { useCallback, useEffect, useRef, useState } from "react";
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
 *
 * Returns whether it is safe to take a snapshot of the liked set:
 * `listen()` is a round-trip, and Tauri does not replay what was
 * emitted before it resolved. A caller that fetches in parallel can
 * therefore both miss the event *and* read a row the write had not
 * reached yet, and it would stay wrong until the next refetch. Waiting
 * on this flag orders the two: everything committed before the snapshot
 * is in it, everything after arrives as an event.
 *
 * A failed `listen()` also flips the flag — nothing will ever arrive on
 * that subscription, and a stale set beats never loading one at all.
 */
export function useLikedChanged(
  callback: (payload: LikedChangedPayload) => void,
): boolean {
  const [ready, setReady] = useState(false);
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
        if (cancelled) {
          off();
          return;
        }
        unlisten = off;
      } catch (err) {
        console.error("[useLikedChanged] listen failed", err);
      }
      if (!cancelled) setReady(true);
    })();
    return () => {
      cancelled = true;
      // Dropped on unmount; on a callback change it re-arms the barrier
      // so the caller re-snapshots around the gap in coverage.
      setReady(false);
      unlisten?.();
    };
  }, [callback]);
  return ready;
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
 *
 * The subscription is permanent while the host component lives, rather
 * than being re-established around each fetch: that would reopen at
 * every track change the gap in coverage that the readiness barrier
 * below exists to close.
 */
export function useLikedTracks(currentTrackId: number | null | undefined) {
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());

  // Changes seen while a full fetch is in flight. The fetch answers with
  // a snapshot taken *before* them, so dropping it straight into state
  // would undo a like that arrived from the other window mid-request —
  // and it would stay undone until the next track change. They're
  // replayed on top of the snapshot instead. Null when no fetch is
  // pending, which is the common case.
  const inFlight = useRef<Map<number, boolean> | null>(null);

  const apply = useCallback((trackId: number, liked: boolean) => {
    inFlight.current?.set(trackId, liked);
    setLikedIds((prev) => {
      if (prev.has(trackId) === liked) return prev;
      const next = new Set(prev);
      if (liked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  }, []);

  const listenerReady = useLikedChanged(
    useCallback(
      (payload: LikedChangedPayload) => apply(payload.trackId, payload.liked),
      [apply],
    ),
  );

  // Full list once the listener is live, and again when the track
  // changes — the user may have liked this one from a library view,
  // which has its own heart.
  useEffect(() => {
    if (!listenerReady) return;
    let cancelled = false;
    const deltas = new Map<number, boolean>();
    inFlight.current = deltas;
    const settle = () => {
      if (inFlight.current === deltas) inFlight.current = null;
    };
    listLikedTrackIds()
      .then((ids) => {
        if (cancelled) return;
        const next = new Set(ids);
        for (const [id, liked] of deltas) {
          if (liked) next.add(id);
          else next.delete(id);
        }
        setLikedIds(next);
      })
      .catch((err) =>
        console.error("[useLikedTracks] initial fetch failed", err),
      )
      .finally(settle);
    return () => {
      cancelled = true;
      settle();
    };
  }, [currentTrackId, listenerReady]);

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
