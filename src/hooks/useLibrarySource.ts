import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "library.source_filter";

/** Broadcast after a successful write so every mounted library surface
 *  re-reads at once — the filter is global to the library, not per tab. */
export const LIBRARY_SOURCE_EVENT = "waveflow:library-source";

/**
 * Which half of the library to show.
 *
 * `"all"` is the point of the unified library, and the default: the source is
 * a filter inside one list rather than a section beside it. The two narrow
 * values exist because "show me only what is on this machine" is a real
 * question — before a flight, or when a server is unreachable — not because
 * the two catalogues are separate places.
 */
export type LibrarySourceFilter = "all" | "local" | "remote";

const DEFAULT_FILTER: LibrarySourceFilter = "all";

/** Anything unrecognised — including an absent row — falls back to showing
 *  everything: a stored value from a newer build must not hide half the
 *  library. */
function parseFilter(raw: string | null): LibrarySourceFilter {
  return raw === "local" || raw === "remote" || raw === "all"
    ? raw
    : DEFAULT_FILTER;
}

export interface LibrarySource {
  source: LibrarySourceFilter;
  /** `false` until the active profile's value has been read. Gate the fetch
   *  on it, or the list loads once with the default and again with the stored
   *  value. */
  ready: boolean;
  setSource: (next: LibrarySourceFilter) => void;
}

/**
 * Per-profile preference for the library's source filter. Concurrency,
 * profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useLibrarySource(): LibrarySource {
  const { value, ready, setValue } = useProfileSetting<LibrarySourceFilter>({
    key: KEY,
    defaultValue: DEFAULT_FILTER,
    parse: parseFilter,
    serialize: (value) => value,
    valueType: "string",
    event: LIBRARY_SOURCE_EVENT,
    label: "useLibrarySource",
  });
  // Fire-and-forget by contract: the shared hook never rejects.
  const setSource = useCallback(
    (next: LibrarySourceFilter) => {
      void setValue(next);
    },
    [setValue],
  );
  return { source: value, ready, setSource };
}
