import { useCallback, useEffect, useRef, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "ui.cover_slideshow";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + each now-playing surface) re-reads in one go. */
export const COVER_SLIDESHOW_EVENT = "waveflow:cover-slideshow";

const DEFAULT_ENABLED = false;

function parseEnabled(raw: string | null): boolean {
  if (raw == null) return DEFAULT_ENABLED;
  return raw === "true" || raw === "1";
}

export interface CoverSlideshow {
  enabled: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: gently crossfade the now-playing cover with the
 * artist photo (issue #466) — a living backdrop built from images already in
 * the library, no plugin. Default OFF (the static cover shows until the user
 * opts in). Read by the now-playing surfaces + the Settings card; the write
 * machinery mirrors [`useScrollLongTitles`](./useScrollLongTitles.ts) —
 * serialized writes, profile-switch guards, and rollback to the last
 * backend-confirmed value.
 */
export function useCoverSlideshow(): CoverSlideshow {
  const { activeProfile } = useProfile();
  const [enabled, setEnabledState] = useState<boolean>(DEFAULT_ENABLED);
  const enabledRef = useRef(enabled);
  const confirmedEnabledRef = useRef(enabled);
  const writeChainRef = useRef<Promise<void>>(Promise.resolve());
  const writeSeqRef = useRef(0);
  const activeProfileIdRef = useRef<number | null>(activeProfile?.id ?? null);
  useEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);
  useEffect(() => {
    activeProfileIdRef.current = activeProfile?.id ?? null;
  }, [activeProfile?.id]);

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
        console.error("[useCoverSlideshow] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(COVER_SLIDESHOW_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(COVER_SLIDESHOW_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const setEnabled = useCallback(async (next: boolean) => {
    const seq = ++writeSeqRef.current;
    const profileId = activeProfileIdRef.current;
    enabledRef.current = next;
    setEnabledState(next);
    const write = writeChainRef.current.then(async () => {
      if (activeProfileIdRef.current !== profileId) return;
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      if (activeProfileIdRef.current !== profileId) return;
      confirmedEnabledRef.current = next;
    });
    writeChainRef.current = write.catch(() => undefined);
    try {
      await write;
      if (activeProfileIdRef.current !== profileId) return;
      if (seq !== writeSeqRef.current) return;
      window.dispatchEvent(new CustomEvent(COVER_SLIDESHOW_EVENT));
    } catch (err) {
      console.error("[useCoverSlideshow] write failed", err);
      if (activeProfileIdRef.current !== profileId) return;
      if (seq !== writeSeqRef.current) return;
      const rollback = confirmedEnabledRef.current;
      enabledRef.current = rollback;
      setEnabledState(rollback);
    }
  }, []);

  return { enabled, setEnabled };
}
