import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Subscribe to the backend's `track:updated` event for the lifetime
 * of the host component. The callback receives the updated track id
 * — most consumers ignore it and just refetch their visible list,
 * but having the id available keeps the door open for targeted
 * row-level refreshes later.
 *
 * Wraps the Tauri `listen()` boilerplate (async subscription returns
 * an unlisten that has to be stashed and called on unmount, with the
 * usual cancellation guard against the subscription resolving after
 * the component already tore down).
 */
export function useTrackUpdated(callback: (trackId: number) => void): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const off = await listen<number>("track:updated", (event) => {
          callback(event.payload);
        });
        if (cancelled) {
          off();
        } else {
          unlisten = off;
        }
      } catch (err) {
        console.error("[useTrackUpdated] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [callback]);
}
