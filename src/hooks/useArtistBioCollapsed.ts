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
  setCollapsed: (next: boolean) => Promise<void>;
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
  // Monotonic write id: a slow earlier write that fails/settles after a
  // newer one must not roll back or broadcast over the newer intent.
  const writeSeqRef = useRef(0);
  // Serializes the writes themselves so they land in click order — a
  // later click can't be persisted before an earlier one and leave the
  // DB holding stale intent.
  const writeChainRef = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    let cancelled = false;
    // Reset to the default the instant the profile changes so a switch
    // never briefly shows the previous profile's choice while the new
    // value loads. The `getProfileSetting` below then replaces it.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCollapsedState(DEFAULT_COLLAPSED);
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setCollapsedState(parseCollapsed(raw));
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
  }, [activeProfile?.id]);

  const setCollapsed = useCallback(
    (next: boolean) => {
      // Snapshot the current value so a persistence failure rolls back
      // instead of leaving the UI ahead of what the backend recorded.
      const previous = collapsed;
      const seq = ++writeSeqRef.current;
      setCollapsedState(next);
      const run = async () => {
        try {
          await setProfileSetting(KEY, next ? "true" : "false", "bool");
          // Only the latest request broadcasts / rolls back, so a stale
          // completion or failure can't clobber a newer click's state.
          if (seq === writeSeqRef.current) {
            window.dispatchEvent(new CustomEvent(ARTIST_BIO_COLLAPSED_EVENT));
          }
        } catch (err) {
          console.error("[useArtistBioCollapsed] write failed", err);
          if (seq === writeSeqRef.current) setCollapsedState(previous);
        }
      };
      writeChainRef.current = writeChainRef.current.then(run, run);
      return writeChainRef.current;
    },
    [collapsed],
  );

  return { collapsed, setCollapsed };
}
