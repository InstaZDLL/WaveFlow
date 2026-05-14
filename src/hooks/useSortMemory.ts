import { useEffect, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

export interface SortState {
  orderBy: string;
  direction: "asc" | "desc";
}

export interface UseSortMemoryResult {
  sort: SortState;
  setSort: (s: SortState) => void;
  isLoaded: boolean;
}

export function useSortMemory(
  contextKey: string,
  defaults: SortState,
): UseSortMemoryResult {
  const [sort, setSortState] = useState<SortState>(defaults);
  const [isLoaded, setIsLoaded] = useState(false);
  const fullKey = `sort.${contextKey}`;

  useEffect(() => {
    let cancelled = false;
    // Reset to defaults before the new fetch so a key change (e.g.
    // navigating between playlists with per-playlist sort memory)
    // doesn't render the previous context's value for the duration of
    // the IPC round-trip. Library tabs use static keys and pay this
    // cost only once at mount, so the reset is effectively a no-op
    // there. The lint disables flag intentional cross-render reset —
    // the standard "reset state when a prop changes" pattern.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSortState(defaults);
    setIsLoaded(false);
    (async () => {
      try {
        const raw = await getProfileSetting(fullKey);
        if (cancelled) return;
        if (raw) {
          try {
            const parsed = JSON.parse(raw) as SortState;
            if (
              parsed &&
              typeof parsed.orderBy === "string" &&
              (parsed.direction === "asc" || parsed.direction === "desc")
            ) {
              setSortState(parsed);
            }
          } catch {
            // fall through with defaults
          }
        }
      } catch {
        // missing profile / pool → keep defaults
      } finally {
        if (!cancelled) setIsLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
    // `defaults` is a fresh object literal each render so adding it
    // to the dep array would loop — fullKey is the only signal we
    // actually react to.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fullKey]);

  const setSort = (s: SortState) => {
    setSortState(s);
    setProfileSetting(fullKey, JSON.stringify(s), "json").catch((err) => {
      console.error("[useSortMemory] persist failed", err);
    });
  };

  return { sort, setSort, isLoaded };
}
