import { useCallback } from "react";
import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "artist.bio_collapsed";

/**
 * Window event broadcast after a successful write so every mounted
 * artist page re-reads the same profile_setting at once (the preference
 * is global, not per-artist, so navigating between artists keeps it).
 */
export const ARTIST_BIO_COLLAPSED_EVENT = "waveflow:artist-bio-collapsed";

const DEFAULT_COLLAPSED = false;

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
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useArtistBioCollapsed(): ArtistBioCollapsed {
  const { value, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_COLLAPSED,
    event: ARTIST_BIO_COLLAPSED_EVENT,
    label: "useArtistBioCollapsed",
  });
  // Fire-and-forget by contract: the shared hook never rejects (it logs
  // and rolls back internally), so dropping the promise is safe.
  const setCollapsed = useCallback(
    (next: boolean) => {
      void setValue(next);
    },
    [setValue],
  );
  return { collapsed: value, setCollapsed };
}
