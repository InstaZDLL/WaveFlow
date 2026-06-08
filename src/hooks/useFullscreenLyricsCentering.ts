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

  const setCentered = useCallback(async (next: boolean) => {
    setCenteredState(next);
    try {
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      window.dispatchEvent(new CustomEvent(FULLSCREEN_LYRICS_CENTERING_EVENT));
    } catch (err) {
      console.error("[useFullscreenLyricsCentering] write failed", err);
    }
  }, []);

  return { centered, setCentered };
}
