import { useCallback, useEffect, useRef, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

const KEY = "ui.artist_hero";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + the artist page) re-reads in one go. */
export const ARTIST_HERO_EVENT = "waveflow:artist-hero";

/** Default ON — the hero is the baseline look of the artist page (a
 *  static backdrop, not extra motion), so it ships enabled and the
 *  toggle is there for users who prefer the flat header. */
const DEFAULT_ENABLED = true;

/**
 * Parses a stored preference value into an enabled state.
 *
 * @param raw - The stored preference value
 * @returns `true` for `"true"` or `"1"`, the default enabled state for a missing value, and `false` for other values
 */
function parseEnabled(raw: string | null): boolean {
  if (raw == null) return DEFAULT_ENABLED;
  return raw === "true" || raw === "1";
}

export interface ArtistHero {
  enabled: boolean;
  /** `false` until the stored preference has been read **for the
   *  currently active profile** — a switch drops back to unresolved
   *  until the new read lands, and the value is reset to the default
   *  meanwhile, so the previous profile's preference never paints even
   *  if that read fails. Consumers that would otherwise show the ON default for a
   *  frame (the hero itself) gate on this; the Settings card doesn't
   *  need to, its checkbox simply settles. */
  resolved: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Manages the per-profile artist hero backdrop preference.
 *
 * The preference is enabled by default and synchronized with the active
 * profile. Updates are applied optimistically and rolled back to the last
 * confirmed value if the latest write fails.
 *
 * @returns The current enabled state, whether the active profile's preference
 * has been resolved, and a function for updating the preference.
 */
export function useArtistHero(): ArtistHero {
  const { activeProfile } = useProfile();
  const [enabled, setEnabledState] = useState<boolean>(DEFAULT_ENABLED);
  // Profile whose read has completed, rather than a bare "did we read
  // once" flag: `undefined` means never, and any other value is only
  // authoritative for that profile.
  const [readProfileId, setReadProfileId] = useState<number | null | undefined>(
    undefined,
  );
  const enabledRef = useRef(enabled);
  const confirmedEnabledRef = useRef(enabled);
  const writeChainRef = useRef<Promise<void>>(Promise.resolve());
  const writeSeqRef = useRef(0);
  // Bumped by every read AND every write. A read applies its result only
  // while it's still the latest thing to have happened: an optimistic
  // toggle fired mid-read must win over the now-stale value that read is
  // about to bring back, and two overlapping reads must not fight.
  const readSeqRef = useRef(0);
  const activeProfileIdRef = useRef<number | null>(activeProfile?.id ?? null);
  useEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);
  useEffect(() => {
    activeProfileIdRef.current = activeProfile?.id ?? null;
  }, [activeProfile?.id]);

  const activeProfileId = activeProfile?.id ?? null;
  useEffect(() => {
    let cancelled = false;
    // Drop the outgoing profile's value before reading the new one, so a
    // read that *fails* lands on the default instead of silently keeping
    // a preference belonging to another profile. Deliberately out of
    // `refresh`, which also serves ARTIST_HERO_EVENT — resetting there
    // would flash the default on every same-profile broadcast, and a
    // failed same-profile refresh is better off keeping what it had.
    // `readProfileId` needs no reset: `resolved` is derived from the id
    // comparison, so it already reads false the moment the id changes.
    // The lint rule guards against effects that derive state from props;
    // this one clears state that belongs to a profile we just left, which
    // has no derived form — `enabled` must stay writable for the
    // optimistic toggle.
    enabledRef.current = DEFAULT_ENABLED;
    confirmedEnabledRef.current = DEFAULT_ENABLED;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setEnabledState(DEFAULT_ENABLED);
    const refresh = async () => {
      const seq = ++readSeqRef.current;
      try {
        const raw = await getProfileSetting(KEY);
        // Stale read: a write (or a newer read) started meanwhile and
        // owns the value now. The stamp in `finally` still runs — this
        // profile HAS been read, and skipping it would leave `resolved`
        // false forever when the racing write then fails.
        if (cancelled || seq !== readSeqRef.current) return;
        const parsed = parseEnabled(raw);
        enabledRef.current = parsed;
        confirmedEnabledRef.current = parsed;
        setEnabledState(parsed);
      } catch (err) {
        console.error("[useArtistHero] read failed", err);
      } finally {
        // Stamped either way: a failed read leaves the default in place,
        // and never stamping would hide the hero forever.
        if (!cancelled) setReadProfileId(activeProfileId);
      }
    };
    void refresh();
    window.addEventListener(ARTIST_HERO_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(ARTIST_HERO_EVENT, refresh);
    };
  }, [activeProfileId]);

  const setEnabled = useCallback(async (next: boolean) => {
    const seq = ++writeSeqRef.current;
    // Invalidate any read still in flight — the user's click is newer
    // than whatever that read is carrying.
    readSeqRef.current += 1;
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
      window.dispatchEvent(new CustomEvent(ARTIST_HERO_EVENT));
    } catch (err) {
      console.error("[useArtistHero] write failed", err);
      if (activeProfileIdRef.current !== profileId) return;
      if (seq !== writeSeqRef.current) return;
      const rollback = confirmedEnabledRef.current;
      enabledRef.current = rollback;
      setEnabledState(rollback);
    }
  }, []);

  // Derived, not stored: a profile switch re-renders with a new
  // `activeProfileId` and the stamp no longer matches, so `resolved`
  // goes false again for free — without a set-state-in-effect dance.
  const resolved =
    readProfileId !== undefined && readProfileId === activeProfileId;

  return { enabled, resolved, setEnabled };
}
