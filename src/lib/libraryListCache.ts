import { listen } from "@tauri-apps/api/event";

/**
 * The library tabs' lists, kept beyond the library view's lifetime.
 *
 * `LibraryView` unmounts as soon as the user opens anything else, and
 * every list it had fetched went with it: coming back to the Albums tab
 * painted a skeleton for as long as the query took, on a library whose
 * albums had not changed. Here the last answer of each tab survives, so
 * a return paints it at once while the view asks again behind it.
 *
 * An entry is filed under a **scope** -- the profile and the libraries'
 * signature (their `updated_at`, and the article-aware sort) -- and the
 * **parameters** of its query (source filter, sort). A read names both,
 * so a list for another sort, another profile or a library that has been
 * rescanned since is never shown. The one read without parameters is the
 * view's first render, which runs before the stored sort and source have
 * been read back: it takes the entry of the current scope whatever its
 * parameters, which are the ones that sort and source were last set to.
 *
 * One entry per tab, not one per query, so what is held while the view
 * is away is one list per tab -- the track list being the large one.
 *
 * Dropped by the events that say the lists changed underneath -- a
 * rescan, a tag edit, an artist picture -- because the view is not there
 * to hear them while it is unmounted. Anything that changes a list
 * without either an event or a new signature is caught by the fetch the
 * view still runs on every mount: a cached list is shown first, never
 * instead of asking.
 */

const KINDS = [
  "tracks",
  "albums",
  "artists",
  "genres",
  "years",
  "folders",
] as const;
export type LibraryListKind = (typeof KINDS)[number];

interface Entry {
  scope: string;
  params: string;
  value: unknown;
}

const entries = new Map<LibraryListKind, Entry>();
/** Bumped per kind when its entry is dropped, so a fetch started before
 *  an invalidating event cannot write its answer back afterwards. */
const generations = new Map<LibraryListKind, number>();
let started = false;

function start() {
  if (started) return;
  started = true;
  // Subscribed once and never removed: the cache lives as long as the
  // window. A profile switch needs no event of its own -- the profile id
  // is part of every scope.
  for (const event of [
    "track:updated",
    "library:rescanned",
    "artist:updated",
  ]) {
    void listen(event, () => dropLibraryLists());
  }
}

/** The list cached for `kind` in `scope`, under exactly `params` -- or,
 *  with `params` omitted, under whichever it was fetched with last. */
export function readLibraryList<T>(
  kind: LibraryListKind,
  scope: string,
  params?: string,
): T | undefined {
  start();
  const entry = entries.get(kind);
  if (!entry || entry.scope !== scope) return undefined;
  if (params !== undefined && entry.params !== params) return undefined;
  return entry.value as T;
}

/** A token to hand back to {@link writeLibraryList}, taken when a fetch
 *  starts: the write is refused if `kind` was dropped meanwhile. */
export function libraryListGeneration(kind: LibraryListKind): number {
  start();
  return generations.get(kind) ?? 0;
}

/** Keep `value` as `kind`'s list for `scope` and `params`, unless the
 *  entry was dropped since `since` was taken. */
export function writeLibraryList(
  kind: LibraryListKind,
  scope: string,
  params: string,
  value: unknown,
  since: number,
): void {
  if (since !== (generations.get(kind) ?? 0)) return;
  entries.set(kind, { scope, params, value });
}

/** Forget `kind`'s list, or every list: the next mount fetches before it
 *  paints. */
export function dropLibraryLists(kind?: LibraryListKind): void {
  for (const k of kind ? [kind] : KINDS) {
    generations.set(k, (generations.get(k) ?? 0) + 1);
    entries.delete(k);
  }
}
