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
  // Last value the backend actually confirmed — the rollback target on a
  // failed write (the optimistic `enabledRef` may already hold a newer,
  // unconfirmed toggle). `writeChainRef` serialises the Tauri writes so
  // a slow `true` write can't land after a later `false` and leave the
  // profile persisted to the wrong value.
  const confirmedEnabledRef = useRef(enabled);
  const writeChainRef = useRef<Promise<void>>(Promise.resolve());
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
        const parsed = parseEnabled(raw);
        enabledRef.current = parsed;
        confirmedEnabledRef.current = parsed;
        setEnabledState(parsed);
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
    enabledRef.current = next;
    setEnabledState(next);
    // Serialise on the write chain so the backend applies toggles in the
    // order they were issued — the last write wins = the user's last
    // intent. `.catch` keeps the chain alive so one failed write doesn't
    // stall every later toggle.
    const write = writeChainRef.current.then(async () => {
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      confirmedEnabledRef.current = next;
    });
    writeChainRef.current = write.catch(() => undefined);
    try {
      await write;
      // A newer toggle superseded this one mid-write — let it own the
      // outcome so an older response can't clobber the latest intent.
      if (seq !== writeSeqRef.current) return;
      window.dispatchEvent(new CustomEvent(SCROLL_LONG_TITLES_EVENT));
    } catch (err) {
      console.error("[useScrollLongTitles] write failed", err);
      if (seq !== writeSeqRef.current) return;
      // Roll back to the last backend-confirmed value, not the optimistic
      // one (which may hold this very failed toggle).
      const rollback = confirmedEnabledRef.current;
      enabledRef.current = rollback;
      setEnabledState(rollback);
    }
  }, []);

  return { enabled, setEnabled };
}
