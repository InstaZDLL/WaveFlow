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
 * rescan, a tag edit, an artist picture, an enrichment, a sync with the
 * server -- because the view is not there to hear them while it is
 * unmounted, and by the wrappers of the commands that change a cover
 * without an event (an album cover, a genre picture). Anything that
 * changes a list without either is caught by the fetch the view still
 * runs on every mount: a cached list is shown first, never instead of
 * asking.
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
/** Per kind, bumped by every fetch that starts and every drop. A fetch
 *  writes only if it is still the last thing that happened to its kind:
 *  an answer overtaken by a newer fetch (another sort, say) or by an
 *  invalidating event never lands in the cache. */
const tickets = new Map<LibraryListKind, number>();
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
    // An enrichment decision applied or undone, a tag workshop batch
    // written or undone, server changes applied: each rewrites rows the
    // lists show.
    "enrichment:changed",
    "tag-workshop:changed",
    "remote:synced",
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

function bump(kind: LibraryListKind): number {
  const next = (tickets.get(kind) ?? 0) + 1;
  tickets.set(kind, next);
  return next;
}

/** Called when a fetch of `kind` starts. The ticket goes back to
 *  {@link writeLibraryList}, which refuses it once a newer fetch has
 *  started or the entry has been dropped. */
export function startLibraryListFetch(kind: LibraryListKind): number {
  start();
  return bump(kind);
}

/** Keep `value` as `kind`'s list for `scope` and `params`, if `ticket`
 *  is still the latest: kept even when the view has gone meanwhile, since
 *  that answer is exactly what the next visit wants to paint. */
export function writeLibraryList(
  kind: LibraryListKind,
  scope: string,
  params: string,
  value: unknown,
  ticket: number,
): void {
  if (ticket !== tickets.get(kind)) return;
  entries.set(kind, { scope, params, value });
}

/** Forget `kind`'s list, or every list: the next mount fetches before it
 *  paints. */
export function dropLibraryLists(kind?: LibraryListKind): void {
  for (const k of kind ? [kind] : KINDS) {
    bump(k);
    entries.delete(k);
  }
}
