import { useEffect, useSyncExternalStore } from "react";

import {
  getUpdateAutoCheck,
  setUpdateAutoCheck as persist,
} from "../lib/tauri/updater";
import { SETTINGS } from "../lib/settingsRegistry";

/**
 * Whether the updater looks for a new version on its own, at launch and
 * after a channel switch. App-wide, stored by the backend, which is what
 * reads it before a check: the automatic check the UpdateBanner makes is
 * skipped there, without a request, while this is off.
 *
 * Module state, one per webview, loaded once: only the main window shows
 * Settings. Same shape as `useSortIgnoreArticles`.
 */
let value = SETTINGS["updater.auto_check"].defaultValue;
let loaded = false;
let ready = false;
/** Bumped by every toggle. A load or a failed save that started before
 *  the latest toggle must not overwrite what the user chose since. */
let revision = 0;
/** Saves run one after another, in the order of the toggles: two in
 *  flight could land in either order and leave the older value stored. */
let saving: Promise<void> = Promise.resolve();
const listeners = new Set<() => void>();

function notify() {
  for (const listener of listeners) listener();
}

function publish(next: boolean) {
  if (next === value && ready) return;
  value = next;
  ready = true;
  notify();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Save the choice, then publish it. Published once saved, not before: the
 * switch shows what the backend will act on at the next launch, and a
 * failed save rejects so the caller can say so.
 *
 * Module-level rather than inside the hook, so the reset of a Settings
 * page takes the same path as the switch.
 */
export async function setUpdateAutoCheck(next: boolean): Promise<void> {
  const mine = ++revision;
  const save = saving.then(() => persist(next));
  saving = save.catch(() => {});
  await save;
  if (mine === revision) publish(next);
}

export function useUpdateAutoCheck() {
  const current = useSyncExternalStore(subscribe, () => value);
  const isReady = useSyncExternalStore(subscribe, () => ready);

  useEffect(() => {
    if (loaded) return;
    loaded = true;
    const started = revision;
    getUpdateAutoCheck()
      .then((stored) => {
        if (started === revision) publish(stored);
      })
      .catch((err) => {
        loaded = false;
        console.error("[useUpdateAutoCheck] load failed", err);
      });
  }, []);

  return {
    autoCheck: current,
    ready: isReady,
    setAutoCheck: setUpdateAutoCheck,
  };
}
