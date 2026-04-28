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
  }, [fullKey]);

  const setSort = (s: SortState) => {
    setSortState(s);
    setProfileSetting(fullKey, JSON.stringify(s), "json").catch((err) => {
      console.error("[useSortMemory] persist failed", err);
    });
  };

  return { sort, setSort, isLoaded };
}
