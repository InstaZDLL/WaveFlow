import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/**
 * Subscribe to the backend's `artist:updated` event for the lifetime of
 * the host component. The callback receives the artist id, which most
 * consumers do need: an artist photo is resolved per artist and cached
 * per artist, so a consumer showing one artist only has to act when it
 * is that artist.
 *
 * Emitted by the three commands behind the artist image picker — set
 * from Deezer, set from a file, remove. Before it existed, only the
 * page the picker was opened from refreshed: the now-playing panel and
 * the cover slideshow resolve the photo once per artist id and had no
 * reason to look again while the same track played, and the library
 * grid re-fetched on a scan or a tag edit but not on a picture change
 * (#692).
 *
 * The `track:updated` sibling, [`useTrackUpdated`](./useTrackUpdated.ts),
 * wraps the same `listen()` boilerplate: an async subscription returns
 * an unlisten that has to be stashed and called on unmount, with a
 * cancellation guard for the subscription resolving after the component
 * already tore down.
 */
export function useArtistUpdated(callback: (artistId: number) => void): void {
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    (async () => {
      try {
        const off = await listen<number>("artist:updated", (event) => {
          callback(event.payload);
        });
        if (cancelled) {
          off();
        } else {
          unlisten = off;
        }
      } catch (err) {
        console.error("[useArtistUpdated] listen failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [callback]);
}
