import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { getProfileStats, type ProfileStats } from "../lib/tauri/browse";
import { useLibrary } from "./useLibrary";
import { useProfile } from "./useProfile";

const EMPTY_STATS: ProfileStats = { liked_count: 0, recent_plays_count: 0 };

/** Backend events after which a counter may have moved. */
const STATS_EVENTS = [
  // A heart flipped: in a list, the player bar, the mini-player, by the
  // global shortcut (#1066).
  "track:liked-changed",
  // A play that ended went into the history: the next track started, or
  // the queue ran out and playback stopped.
  "player:track-changed",
  "player:state",
] as const;

/**
 * The active profile's counters — liked tracks, recently played — for
 * every surface that shows them: the sidebar and both Homes.
 *
 * Each of them used to read the counters once and refetch on its own
 * list of triggers, none of which was a like: the "Liked tracks" count
 * only moved at the next restart (#1066). They read them here now, so a
 * trigger added for one is there for all.
 *
 * Re-read on a profile switch, on the events above, and whenever the
 * library context reloads the libraries: that is what follows a scan, a
 * removed folder, a duplicate sent to the trash — every way a track, and
 * the like on it, leaves the database.
 *
 * The listeners are in place before the first read, the order
 * [`useLikedChanged`](./useLikedTracks.ts) explains: a like committed
 * between a read and the subscription would otherwise never be counted.
 */
export function useProfileStats(): ProfileStats {
  const { activeProfile } = useProfile();
  const { libraries } = useLibrary();
  const profileId = activeProfile?.id ?? null;
  // Kept with the profile they were read for, and given out only while
  // that profile is the active one: the sidebar stays mounted through a
  // switch, and must not show the last profile's counts until — or, if
  // the read fails, instead of — the next profile's.
  const [loaded, setLoaded] = useState<{
    profileId: number | null;
    stats: ProfileStats;
  }>({ profileId: null, stats: EMPTY_STATS });
  const [listening, setListening] = useState(false);
  // Read by the event listeners, which outlive a render.
  const profileIdRef = useRef(profileId);
  useEffect(() => {
    profileIdRef.current = profileId;
  }, [profileId]);

  // Only the answer to the latest request lands: an older one, slower,
  // may have been read before the change that triggered the newer one —
  // or for the profile that was active before. The backend switches
  // before this side learns of it, so a read labelled with the profile
  // known here may hold the next one's counts, never the previous one's.
  const seqRef = useRef(0);
  const refresh = useCallback(() => {
    const requestedFor = profileIdRef.current;
    if (requestedFor === null) return;
    const seq = ++seqRef.current;
    getProfileStats()
      .then((stats) => {
        if (seq === seqRef.current)
          setLoaded({ profileId: requestedFor, stats });
      })
      .catch((err) => console.error("[useProfileStats] fetch failed", err));
  }, []);

  useEffect(() => {
    let cancelled = false;
    const offs: UnlistenFn[] = [];
    Promise.all(
      STATS_EVENTS.map((name) =>
        listen(name, refresh)
          .then((off) => {
            if (cancelled) off();
            else offs.push(off);
          })
          .catch((err) =>
            console.error(`[useProfileStats] listen ${name} failed`, err),
          ),
      ),
    ).then(() => {
      // A failed `listen()` opens the gate as well: counters that miss
      // an event beat counters that never load.
      if (!cancelled) setListening(true);
    });
    return () => {
      cancelled = true;
      setListening(false);
      for (const off of offs) off();
    };
  }, [refresh]);

  useEffect(() => {
    if (!listening || profileId === null) return;
    refresh();
  }, [listening, profileId, libraries, refresh]);

  return profileId !== null && loaded.profileId === profileId
    ? loaded.stats
    : EMPTY_STATS;
}
