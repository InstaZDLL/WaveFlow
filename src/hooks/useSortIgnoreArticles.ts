import { useEffect, useSyncExternalStore } from "react";

import {
  getSortIgnoreArticles,
  setSortIgnoreArticles as persist,
} from "../lib/tauri/preferences";
import { SETTINGS } from "../lib/settingsRegistry";

/**
 * Whether artist and album listings set a leading article aside ("The
 * Beatles" under B). App-wide, stored by the backend, which sorts with it;
 * the interface reads it to file names under the same letter of the A–Z
 * rail and to fetch the listings again when it changes.
 *
 * Module state, one per webview, loaded once: only the main window lists
 * the library.
 */
let value = SETTINGS["library.sort_ignore_articles"].defaultValue;
let loaded = false;
/** Bumped by every toggle. A load or a failed save that started before
 *  the latest toggle must not overwrite what the user chose since. */
let revision = 0;
/** Saves run one after another, in the order of the toggles: two in
 *  flight could land in either order and leave the older value stored. */
let saving: Promise<void> = Promise.resolve();
const listeners = new Set<() => void>();

function publish(next: boolean) {
  if (next === value) return;
  value = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Save the choice, then publish it. Published once saved, not before: the
 * backend sorts with the flag it sets after the write, and a listing
 * refetched on an optimistic value would come back in the old order. The
 * save is a local write.
 *
 * Module-level rather than inside the hook, so the reset of a Settings
 * page can take the same path as the switch.
 */
export async function setSortIgnoreArticles(next: boolean): Promise<void> {
  const mine = ++revision;
  const save = saving.then(() => persist(next));
  saving = save.catch(() => {});
  await save;
  if (mine === revision) publish(next);
}

export function useSortIgnoreArticles() {
  const current = useSyncExternalStore(subscribe, () => value);

  useEffect(() => {
    if (loaded) return;
    loaded = true;
    const started = revision;
    getSortIgnoreArticles()
      .then((stored) => {
        if (started === revision) publish(stored);
      })
      .catch((err) => {
        loaded = false;
        console.error("[useSortIgnoreArticles] load failed", err);
      });
  }, []);

  return { ignoreArticles: current, setIgnoreArticles: setSortIgnoreArticles };
}
