import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  getNetworkBlockedTracks,
  listNetworkSources,
  type NetworkSourceView,
} from "./tauri/networkLibrary";

/**
 * Tracks on WebDAV and SMB shares, as the views see them.
 *
 * A row is grey when its share does not answer and no copy of it is cached.
 * That is not `is_available`, which hides a track everywhere: these tracks stay
 * listed, with their likes and playlists, and come back on their own. The
 * backend sends the ids, so this costs a library with no share one empty list
 * per change of a source's state, and nothing per row.
 */

const LOCATOR_PREFIX = "wfnet://";

/** Whether a stored path is on a share rather than on this machine. */
export function isNetworkPath(path: string | null | undefined): boolean {
  return path != null && path.startsWith(LOCATOR_PREFIX);
}

interface Snapshot {
  blocked: ReadonlySet<number>;
  sources: readonly NetworkSourceView[];
}

let snapshot: Snapshot = { blocked: new Set(), sources: [] };
let started = false;
const listeners = new Set<() => void>();
/** Only the latest read may land: a slow one must not undo a newer answer. */
let generation = 0;

async function refresh() {
  const mine = ++generation;
  try {
    const [blocked, sources] = await Promise.all([
      getNetworkBlockedTracks(),
      listNetworkSources(),
    ]);
    if (mine !== generation) return;
    snapshot = { blocked: new Set(blocked), sources };
    listeners.forEach((listener) => listener());
  } catch (err) {
    console.warn("[networkTracks] refresh failed", err);
  }
}

function start() {
  if (started) return;
  started = true;
  void refresh();
  // Subscribed before anything can change it, and not removed: this store
  // lives as long as the window. A profile switch reloads the sources in the
  // backend, which sends this event.
  void listen("network-source:changed", () => void refresh());
  void listen("library:rescanned", () => void refresh());
}

function subscribe(listener: () => void) {
  start();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Ask again now: after a change made from the settings card. */
export function refreshNetworkTracks(): Promise<void> {
  return refresh();
}

/** Whether a track cannot play right now because its share is down. */
export function useTrackBlocked(trackId: number | null | undefined): boolean {
  return useSyncExternalStore(subscribe, () =>
    trackId == null ? false : snapshot.blocked.has(trackId),
  );
}

/** The same as a set, for a view that renders its rows inline. */
export function useBlockedTrackIds(): ReadonlySet<number> {
  return useSyncExternalStore(subscribe, () => snapshot.blocked);
}

/** Every source of the profile, kept current. */
export function useNetworkSources(): readonly NetworkSourceView[] {
  return useSyncExternalStore(subscribe, () => snapshot.sources);
}

/** `<share name>/<path>` for a locator; any other path as it is. */
export function displayTrackPath(
  path: string,
  sources: readonly NetworkSourceView[],
): string {
  if (!isNetworkPath(path)) return path;
  const rest = path.slice(LOCATOR_PREFIX.length);
  const cut = rest.indexOf("/");
  const uuid = cut >= 0 ? rest.slice(0, cut) : rest;
  const name = sources.find((s) => s.uuid === uuid)?.name ?? "?";
  return cut >= 0 ? `${name}/${rest.slice(cut + 1)}` : name;
}
