import { useCallback, useEffect, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "ui.scroll_long_titles";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + each `MarqueeText`) re-reads in one go. */
export const SCROLL_LONG_TITLES_EVENT = "waveflow:scroll-long-titles";

const DEFAULT_ENABLED = true;

function parseEnabled(raw: string | null): boolean {
  if (raw == null) return DEFAULT_ENABLED;
  return raw === "true" || raw === "1";
}

export interface ScrollLongTitles {
  enabled: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: scroll long titles (the marquee in the
 * PlayerBar + immersive view) end-to-end instead of truncating them.
 * Default ON. Turning it off makes every `MarqueeText` render static +
 * truncated.
 */
export function useScrollLongTitles(): ScrollLongTitles {
  const { activeProfile } = useProfile();
  const [enabled, setEnabledState] = useState<boolean>(DEFAULT_ENABLED);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setEnabledState(parseEnabled(raw));
      } catch (err) {
        console.error("[useScrollLongTitles] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(SCROLL_LONG_TITLES_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(SCROLL_LONG_TITLES_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const setEnabled = useCallback(
    async (next: boolean) => {
      const previous = enabled;
      setEnabledState(next);
      try {
        await setProfileSetting(KEY, next ? "true" : "false", "bool");
        window.dispatchEvent(new CustomEvent(SCROLL_LONG_TITLES_EVENT));
      } catch (err) {
        console.error("[useScrollLongTitles] write failed", err);
        setEnabledState(previous);
      }
    },
    [enabled],
  );

  return { enabled, setEnabled };
}
