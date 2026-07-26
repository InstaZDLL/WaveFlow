import { useCallback, useEffect, useState } from "react";
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

  useEffect(() => {
    let cancelled = false;
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
    async (next: boolean) => {
      // Snapshot the current value so a persistence failure rolls back
      // instead of leaving the UI ahead of what the backend recorded.
      const previous = collapsed;
      setCollapsedState(next);
      try {
        await setProfileSetting(KEY, next ? "true" : "false", "bool");
        window.dispatchEvent(new CustomEvent(ARTIST_BIO_COLLAPSED_EVENT));
      } catch (err) {
        console.error("[useArtistBioCollapsed] write failed", err);
        setCollapsedState(previous);
      }
    },
    [collapsed],
  );

  return { collapsed, setCollapsed };
}
