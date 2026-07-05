import { useCallback, useEffect, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "lyrics.fullscreen_sync_centered";

/**
 * Window event broadcast after a successful write so any other
 * mounted consumer (the Settings card + the FullscreenLyrics view
 * itself) re-reads from the same profile_setting in one go.
 */
export const FULLSCREEN_LYRICS_CENTERING_EVENT =
  "waveflow:fullscreen-lyrics-centering";

const DEFAULT_CENTERED = false;

function parseCentered(raw: string | null): boolean {
  if (raw == null) return DEFAULT_CENTERED;
  return raw === "true" || raw === "1";
}

export interface FullscreenLyricsCentering {
  centered: boolean;
  setCentered: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference for centering the *synced* lyrics view in
 * the fullscreen overlay (#168). The plain (un-synced) lyrics are
 * already centered — flipping this aligns the synced view to match.
 *
 * Default OFF so existing users see no change unless they opt in via
 * Settings → Appearance.
 */
export function useFullscreenLyricsCentering(): FullscreenLyricsCentering {
  const { activeProfile } = useProfile();
  const [centered, setCenteredState] = useState<boolean>(DEFAULT_CENTERED);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setCenteredState(parseCentered(raw));
      } catch (err) {
        console.error("[useFullscreenLyricsCentering] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(FULLSCREEN_LYRICS_CENTERING_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(FULLSCREEN_LYRICS_CENTERING_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const setCentered = useCallback(
    async (next: boolean) => {
      // Snapshot the value the user is currently seeing so we can
      // roll back on a persistence failure — otherwise the UI keeps
      // showing the new value while the backend never recorded it,
      // and the next mount / refresh would silently revert.
      const previous = centered;
      setCenteredState(next);
      try {
        await setProfileSetting(KEY, next ? "true" : "false", "bool");
        // Only broadcast on success — other consumers re-reading
        // from the backend after a failed write would just confirm
        // the rolled-back value and the event would be misleading.
        window.dispatchEvent(
          new CustomEvent(FULLSCREEN_LYRICS_CENTERING_EVENT),
        );
      } catch (err) {
        console.error("[useFullscreenLyricsCentering] write failed", err);
        setCenteredState(previous);
      }
    },
    [centered],
  );

  return { centered, setCentered };
}
