import { useCallback } from "react";
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

  const remember = useCallback(
    (query: string) => {
      const trimmed = query.trim();
      // A single character is a real search too: one Han character is a
      // word. Before the profile's list is read, the updater would see
      // the empty default and the write would replace the stored list.
      if (trimmed === "" || !ready) return;
      const folded = trimmed.toLocaleLowerCase();
      void setValue((previous) =>
        [
          trimmed,
          ...previous.filter((q) => q.toLocaleLowerCase() !== folded),
        ].slice(0, MAX_RECENT),
      );
    },
    [ready, setValue],
  );

  const forget = useCallback(
    (query: string) => {
      void setValue((previous) => previous.filter((q) => q !== query));
    },
    [setValue],
  );

  const clear = useCallback(() => {
    void setValue(DEFAULT);
  }, [setValue]);

  return { recent: value, remember, forget, clear };
}
