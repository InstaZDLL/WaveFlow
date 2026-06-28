import { useCallback, useEffect, useRef, useState } from "react";
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
  // Live mirror of the displayed value so a write can roll back to what
  // the user was actually seeing without re-creating `setEnabled` on
  // every state change. Monotonic token so only the latest write applies
  // its success / failure side effects (rapid toggles overlap otherwise).
  const enabledRef = useRef(enabled);
  const writeSeqRef = useRef(0);
  useEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);

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

  const setEnabled = useCallback(async (next: boolean) => {
    const seq = ++writeSeqRef.current;
    const previous = enabledRef.current;
    enabledRef.current = next;
    setEnabledState(next);
    try {
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      // A newer toggle superseded this one mid-write — let it own the
      // outcome so an older response can't clobber the latest intent.
      if (seq !== writeSeqRef.current) return;
      window.dispatchEvent(new CustomEvent(SCROLL_LONG_TITLES_EVENT));
    } catch (err) {
      console.error("[useScrollLongTitles] write failed", err);
      if (seq !== writeSeqRef.current) return;
      enabledRef.current = previous;
      setEnabledState(previous);
    }
  }, []);

  return { enabled, setEnabled };
}
