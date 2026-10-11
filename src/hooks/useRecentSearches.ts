import { useCallback, useEffect, useRef } from "react";
import { useProfile } from "./useProfile";
import { useProfileSetting } from "./useProfileSetting";

/** How many past searches the top bar keeps. */
const MAX_RECENT = 8;

const KEY = "ui.recent_searches";
const EVENT = "waveflow:recent-searches-changed";
const DEFAULT: string[] = [];

function parse(raw: string | null): string[] {
  if (!raw) return DEFAULT;
  try {
    const value: unknown = JSON.parse(raw);
    return Array.isArray(value)
      ? value
          .filter((q): q is string => typeof q === "string")
          .slice(0, MAX_RECENT)
      : DEFAULT;
  } catch {
    return DEFAULT;
  }
}

function serialize(value: string[]): string {
  return JSON.stringify(value);
}

export interface RecentSearches {
  recent: string[];
  /** Record a search the user acted on: most recent first, no duplicates
   *  (case-insensitive), capped. */
  remember: (query: string) => void;
  forget: (query: string) => void;
  clear: () => void;
}

/**
 * The searches the user went somewhere from, per profile. Only a search
 * that led to a result is kept — opening an artist, an album or playing a
 * track from the dropdown — so the list holds what was found, not every
 * prefix typed on the way.
 */
export function useRecentSearches(): RecentSearches {
  const { value, ready, setValue } = useProfileSetting<string[]>({
    key: KEY,
    defaultValue: DEFAULT,
    parse,
    serialize,
    valueType: "json",
    event: EVENT,
    label: "useRecentSearches",
  });

  // Each query goes to the top in turn, so the last one recorded ends
  // up first; duplicates are folded case-insensitively.
  const record = useCallback(
    (queries: string[]) => {
      void setValue((previous) => {
        let next = previous;
        for (const query of queries) {
          const folded = query.toLocaleLowerCase();
          next = [
            query,
            ...next.filter((q) => q.toLocaleLowerCase() !== folded),
          ].slice(0, MAX_RECENT);
        }
        return next;
      });
    },
    [setValue],
  );

  // Searches made before the profile's list is read wait here: written
  // then, the updater would see the empty default and replace the list.
  const pendingRef = useRef<string[]>([]);
  // The queue belongs to the profile that was active when it filled: a
  // switch drops it, or the old profile's searches would land in the new
  // one. Seeded with the current profile, so the first read keeps it.
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  const pendingProfileRef = useRef(activeProfileId);
  useEffect(() => {
    if (pendingProfileRef.current === activeProfileId) return;
    pendingProfileRef.current = activeProfileId;
    pendingRef.current = [];
  }, [activeProfileId]);
  useEffect(() => {
    if (!ready || pendingRef.current.length === 0) return;
    const queued = pendingRef.current;
    pendingRef.current = [];
    record(queued);
  }, [ready, record]);

  const remember = useCallback(
    (query: string) => {
      // A single character is a real search too: one Han character is a
      // word.
      const trimmed = query.trim();
      if (trimmed === "") return;
      if (!ready) {
        pendingRef.current.push(trimmed);
        return;
      }
      record([trimmed]);
    },
    [ready, record],
  );

  const forget = useCallback(
    (query: string) => {
      // Also out of the queue, or reading the profile would bring it back.
      pendingRef.current = pendingRef.current.filter((q) => q !== query);
      void setValue((previous) => previous.filter((q) => q !== query));
    },
    [setValue],
  );

  const clear = useCallback(() => {
    pendingRef.current = [];
    void setValue(DEFAULT);
  }, [setValue]);

  return { recent: value, remember, forget, clear };
}
