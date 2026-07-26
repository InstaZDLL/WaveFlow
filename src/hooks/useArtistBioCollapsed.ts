import { useCallback, useEffect, useRef, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "artist.bio_collapsed";

/**
 * Window event broadcast after a successful write so every mounted
 * artist page re-reads the same profile_setting at once (the preference
 * is global, not per-artist, so navigating between artists keeps it).
 */
export const ARTIST_BIO_COLLAPSED_EVENT = "waveflow:artist-bio-collapsed";

const DEFAULT_COLLAPSED = false;

function parseCollapsed(raw: string | null): boolean {
  if (raw == null) return DEFAULT_COLLAPSED;
  return raw === "true" || raw === "1";
}

export interface ArtistBioCollapsed {
  collapsed: boolean;
  /** Fire-and-forget: optimistic UI update, persistence is serialized
   *  internally, guarded against a mid-flight profile switch, and rolls
   *  back to the last DB-confirmed value on failure. */
  setCollapsed: (next: boolean) => void;
}

/**
 * Per-profile preference for hiding the artist-page biography block so a
 * long bio doesn't push the discography out of view (issue #422). The
 * toggle lives on the artist page itself, not in Settings, but the choice
 * is remembered globally across every artist.
 *
 * Default OFF (bio shown) so existing users see no change unless they
 * collapse it.
 */
export function useArtistBioCollapsed(): ArtistBioCollapsed {
  const { activeProfile } = useProfile();
  const [collapsed, setCollapsedState] = useState<boolean>(DEFAULT_COLLAPSED);

  // Monotonic token shared by reads and writes: a write bumps it to
  // invalidate any in-flight refresh (so a stale read can't clobber the
  // newer optimistic state), and only the latest write broadcasts /
  // rolls back.
  const seqRef = useRef(0);
  // Serializes writes so they land in click order — a later click can't
  // be persisted before an earlier one and leave the DB holding stale
  // intent.
  const writeChainRef = useRef<Promise<unknown>>(Promise.resolve());
  // Last DB-confirmed value — the rollback target. After a run of failed
  // writes the optimistic pre-toggle value was never persisted, so a
  // failure reverts to confirmed truth, not the optimistic snapshot.
  // `null` = nothing confirmed yet for this profile (a fresh mount/switch
  // whose initial read hasn't landed, or was invalidated by a write) — the
  // rollback must NOT treat the reset default as a confirmed value.
  const persistedRef = useRef<boolean | null>(DEFAULT_COLLAPSED);
  // Active profile id mirrored into a ref so a queued write can check —
  // at the moment it runs — that the profile is still the one the user
  // toggled (`set_profile_setting` targets whatever profile is active
  // when it runs, so a mid-flight switch would write to the wrong one).
  const activeProfileId = activeProfile?.id;
  const activeProfileIdRef = useRef(activeProfileId);
  useEffect(() => {
    activeProfileIdRef.current = activeProfileId;
  }, [activeProfileId]);

  useEffect(() => {
    let cancelled = false;
    // Reset to the default the instant the profile changes so a switch
    // never briefly shows the previous profile's choice while the new
    // value loads. The `getProfileSetting` below then replaces it.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCollapsedState(DEFAULT_COLLAPSED);
    // Mark unconfirmed until the read below (or a successful write) lands —
    // the reset default is a display placeholder, not a rollback target.
    persistedRef.current = null;
    const refresh = async () => {
      // Marker captured before the async read. If a local write bumps
      // `seqRef` while we're awaiting, this read is stale — its newer
      // optimistic state (plus that write's own refresh) supersedes it,
      // so we drop the result instead of clobbering it.
      const reqSeq = seqRef.current;
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled || seqRef.current !== reqSeq) return;
        const loaded = parseCollapsed(raw);
        setCollapsedState(loaded);
        persistedRef.current = loaded;
      } catch (err) {
        console.error("[useArtistBioCollapsed] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(ARTIST_BIO_COLLAPSED_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(ARTIST_BIO_COLLAPSED_EVENT, refresh);
    };
  }, [activeProfileId]);

  const setCollapsed = useCallback((next: boolean) => {
    setCollapsedState(next); // optimistic; bumping seqRef invalidates reads
    const profileAtClick = activeProfileIdRef.current;
    const seq = ++seqRef.current;
    // Queue behind any in-flight write. A leading no-op catch keeps a
    // prior failure from breaking the chain for later writes.
    writeChainRef.current = writeChainRef.current
      .catch(() => {})
      .then(async () => {
        // Profile switched out from under this queued write — skip so a
        // toggle never lands in another profile's settings.
        if (activeProfileIdRef.current !== profileAtClick) return;
        await setProfileSetting(KEY, next ? "true" : "false", "bool");
        // The switch effect reset persistedRef / state to the new profile's
        // default while we were awaiting. Stamping `next` (the old profile's
        // value) onto persistedRef or broadcasting a refresh now would
        // corrupt the current profile's confirmed truth, so skip all
        // post-write bookkeeping once the profile has changed.
        if (activeProfileIdRef.current !== profileAtClick) return;
        persistedRef.current = next; // confirmed in the DB
        // Only the latest enqueued write broadcasts, so an older write's
        // completion can't refresh over a newer state.
        if (seq === seqRef.current) {
          window.dispatchEvent(new CustomEvent(ARTIST_BIO_COLLAPSED_EVENT));
        }
      })
      .catch((err: unknown) => {
        console.error("[useArtistBioCollapsed] write failed", err);
        // Roll back only for the still-current profile, and only if no
        // later write superseded this one.
        if (
          activeProfileIdRef.current !== profileAtClick ||
          seq !== seqRef.current
        )
          return;
        const confirmed = persistedRef.current;
        if (confirmed !== null) {
          // Revert to the last DB-confirmed value, not the optimistic one.
          setCollapsedState(confirmed);
          return;
        }
        // No confirmed value yet — this profile's initial read was
        // invalidated by this very write, so persistedRef would wrongly
        // report the reset default. Re-read the DB truth (a failed write
        // left it unchanged) rather than rolling back to a guessed `false`.
        getProfileSetting(KEY)
          .then((raw) => {
            if (
              activeProfileIdRef.current !== profileAtClick ||
              seq !== seqRef.current
            )
              return;
            const loaded = parseCollapsed(raw);
            persistedRef.current = loaded;
            setCollapsedState(loaded);
          })
          .catch((readErr: unknown) => {
            console.error(
              "[useArtistBioCollapsed] rollback read failed",
              readErr,
            );
          });
      });
  }, []);

  return { collapsed, setCollapsed };
}
