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
/// PlayerBar) re-reads the list and stays in sync.
///
/// Scope, by design: same-webview only. The mini-player is a separate
/// webview, so its writes aren't serialised against the main window's
/// `writeChainRef` — two near-simultaneous toggles of *different*
/// stations from the two webviews could last-write-wins one out. This
/// is accepted for v1: it's a single-user, one-toggle-at-a-time surface,
/// each write is an atomic single-row UPSERT (never torn), and each
/// webview reconciles on its next mount / profile switch. A true
/// cross-process guarantee (a backend add/remove merge instead of a
/// full-array replace, or a Tauri-event write queue) is the proper fix
/// if this ever bites — deliberately out of scope here.
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
  // `true` once the CURRENT profile's favorites have loaded. Toggles are
  // ignored until then so a click in the load window can't compute from
  // the just-cleared (empty) ref and persist a wiped list — a full-array
  // replace would otherwise delete every existing favorite.
  const loadedRef = useRef(false);
  // Bumped each time the profile changes; a stale (previous-profile)
  // load resolving late checks this and drops itself instead of applying
  // over the new profile's list.
  const loadGenRef = useRef(0);
  // In-flight write count. The cross-instance reload event fires only
  // when this drops back to 0 (the whole rapid-toggle batch settled),
  // not once per write — an intermediate reload could otherwise read a
  // mid-batch backend state and clobber `favoritesRef` out of order.
  const pendingWritesRef = useRef(0);

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
    // New profile generation: clear the synchronous snapshot + mark the
    // list "not loaded" so a toggle fired before the new list lands is
    // ignored (rather than computing from — and persisting — an empty or
    // previous-profile list). The async load below refills it.
    const gen = ++loadGenRef.current;
    loadedRef.current = false;
    favoritesRef.current = [];
    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional reset before per-profile reload
    setFavorites([]);
    const load = () => {
      getPluginFavorites(WEB_RADIO_PLUGIN_ID).then(
        (list) => {
          // Drop a stale load that resolved after a newer profile switch.
          if (cancelled || gen !== loadGenRef.current) return;
          // Don't clobber fresher optimistic state with stale backend
          // data: when local writes are still in flight (a bus reload
          // racing a pending toggle), skip the apply — the pending
          // batch's own settle re-syncs. The initial / profile-switch
          // load always passes this (toggles are blocked until
          // `loadedRef`, so no writes are pending yet).
          if (pendingWritesRef.current === 0) {
            applyFavorites(list);
          }
          loadedRef.current = true;
        },
        (err: unknown) => {
          if (!cancelled && gen === loadGenRef.current) {
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
      // Ignore toggles until the current profile's list has loaded — a
      // full-array write computed from the not-yet-loaded (empty) ref
      // would wipe the stored favorites. The window is sub-second (mount
      // / profile switch); after that this is always true.
      if (!loadedRef.current) return;
      const seq = ++favoriteSeqRef.current;
      const current = favoritesRef.current;
      const next = current.some((f) => f.id === fav.id)
        ? current.filter((f) => f.id !== fav.id)
        : [...current, fav];
      applyFavorites(next);
      const profileAtToggle = profileIdRef.current;
      pendingWritesRef.current += 1;
      writeChainRef.current = writeChainRef.current
        .catch(() => {})
        .then(() => {
          // Profile switched while queued → skip (the backend would
          // write this profile's list into the now-active profile).
          if (profileIdRef.current !== profileAtToggle) return;
          return setPluginFavorites(WEB_RADIO_PLUGIN_ID, next);
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
        })
        .finally(() => {
          // Only the last write of a rapid batch notifies siblings
          // (PlayerBar ↔ WebRadioView) to re-read, so an intermediate
          // reload can't apply a mid-batch backend state out of order.
          pendingWritesRef.current -= 1;
          if (pendingWritesRef.current === 0) {
            window.dispatchEvent(new Event(FAVORITES_CHANGED_EVENT));
          }
        });
    },
    [applyFavorites],
  );

  return { favorites, isFavorite, toggleFavorite };
}
