import { useCallback, useEffect, useRef, useState } from "react";

import {
  getPluginFavorites,
  setPluginFavorites,
  type PluginFavorite,
} from "../lib/tauri/plugins";
import { useProfile } from "./useProfile";

/** Plugin id of the bundled Web Radio source plugin. */
export const WEB_RADIO_PLUGIN_ID = "web-radio";

/// DOM event fired after a favorites write commits so every
/// `useWebRadioFavorites` instance in the SAME webview (WebRadioView +
/// PlayerBar) re-reads the list and stays in sync. Cross-webview (the
/// mini-player is a separate webview) is eventually consistent — each
/// reloads on its own mount / profile switch, which is enough for a
/// rarely-changed list.
const FAVORITES_CHANGED_EVENT = "waveflow:web-radio-favorites-changed";

/**
 * Per-profile Web Radio favorites with an optimistic, race-safe toggle.
 * Shared by WebRadioView (the list + per-row star) and the PlayerBar /
 * mini-player favorite-station star so the three never drift.
 *
 * Concurrency model (carried over from the original WebRadioView
 * implementation):
 * - `favoritesRef` mirrors the list synchronously so back-to-back
 *   toggles compute from the latest value before React commits.
 * - `writeChainRef` serialises backend writes so they land in click
 *   order — a stale snapshot can't resolve last and clobber a newer one.
 * - `favoriteSeqRef` lets only the most-recent toggle's failure re-sync
 *   from the server (an earlier failure must not overwrite a newer
 *   optimistic state a later write already persisted).
 * - `profileIdRef` skips a queued write/re-sync once the profile
 *   switched, so a stale write can't land in the wrong profile (the
 *   backend resolves `require_profile_pool()` at execution time).
 */
export function useWebRadioFavorites() {
  const { activeProfile } = useProfile();
  const [favorites, setFavorites] = useState<PluginFavorite[]>([]);
  const favoritesRef = useRef<PluginFavorite[]>([]);
  const writeChainRef = useRef<Promise<unknown>>(Promise.resolve());
  const favoriteSeqRef = useRef(0);
  const profileIdRef = useRef(activeProfile?.id);

  const applyFavorites = useCallback((list: PluginFavorite[]) => {
    favoritesRef.current = list;
    setFavorites(list);
  }, []);

  useEffect(() => {
    profileIdRef.current = activeProfile?.id;
  }, [activeProfile?.id]);

  // Load on mount + profile switch, and re-load whenever another
  // instance in this webview commits a change (the DOM bus below).
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      getPluginFavorites(WEB_RADIO_PLUGIN_ID).then(
        (list) => {
          if (!cancelled) applyFavorites(list);
        },
        (err: unknown) => {
          if (!cancelled) {
            console.warn("[useWebRadioFavorites] load failed", err);
          }
        },
      );
    };
    load();
    window.addEventListener(FAVORITES_CHANGED_EVENT, load);
    return () => {
      cancelled = true;
      window.removeEventListener(FAVORITES_CHANGED_EVENT, load);
    };
  }, [activeProfile?.id, applyFavorites]);

  const isFavorite = useCallback(
    (id: string) => favorites.some((f) => f.id === id),
    [favorites],
  );

  const toggleFavorite = useCallback(
    (fav: PluginFavorite) => {
      const seq = ++favoriteSeqRef.current;
      const current = favoritesRef.current;
      const next = current.some((f) => f.id === fav.id)
        ? current.filter((f) => f.id !== fav.id)
        : [...current, fav];
      applyFavorites(next);
      const profileAtToggle = profileIdRef.current;
      writeChainRef.current = writeChainRef.current
        .catch(() => {})
        .then(() => {
          // Profile switched while queued → skip (the backend would
          // write this profile's list into the now-active profile).
          if (profileIdRef.current !== profileAtToggle) return;
          return setPluginFavorites(WEB_RADIO_PLUGIN_ID, next);
        })
        .then(() => {
          // Notify sibling instances (PlayerBar ↔ WebRadioView) to
          // re-read so the star and the list agree.
          window.dispatchEvent(new Event(FAVORITES_CHANGED_EVENT));
        })
        .catch(async (e) => {
          if (favoriteSeqRef.current !== seq) return;
          console.warn("[useWebRadioFavorites] write failed", e);
          if (profileIdRef.current !== profileAtToggle) return;
          try {
            const fresh = await getPluginFavorites(WEB_RADIO_PLUGIN_ID);
            if (favoriteSeqRef.current === seq) applyFavorites(fresh);
          } catch {
            /* leave optimistic state; the next load re-syncs */
          }
        });
    },
    [applyFavorites],
  );

  return { favorites, isFavorite, toggleFavorite };
}
