import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import type { TFunction } from "i18next";

import {
  WAVESYNC_CHANGED_EVENT,
  wavesyncAdd,
  wavesyncGetView,
  type WaveSyncView,
} from "../lib/tauri/wavesync";

/**
 * WaveSync's state for every component, from one subscription: the view the
 * backend emits after each change, whether the start / join dialog is open,
 * and the last notice (an add that skipped tracks, a refusal) for the toast.
 *
 * A module-level store rather than a context: the menu entry, the track
 * context menu, the panel and the toast sit far apart in the tree, and the
 * backend is the source of truth anyway.
 */
interface Store {
  view: WaveSyncView | null;
  dialogOpen: boolean;
  notice: { key: number; text: string } | null;
}

let store: Store = { view: null, dialogOpen: false, notice: null };
const listeners = new Set<() => void>();
let started = false;
let noticeKey = 0;

function set(next: Partial<Store>) {
  store = { ...store, ...next };
  listeners.forEach((l) => l());
}

function start() {
  if (started) return;
  started = true;
  void listen<WaveSyncView>(WAVESYNC_CHANGED_EVENT, (event) =>
    set({ view: event.payload }),
  );
  wavesyncGetView()
    .then((view) => set({ view }))
    .catch((err) => console.error("[wavesync] view", err));
}

function subscribe(listener: () => void) {
  start();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useWaveSync(): Store {
  return useSyncExternalStore(
    subscribe,
    () => store,
    () => store,
  );
}

/** Whether a session runs here (hosted, joined, or waiting to be let in). */
export function wavesyncActive(view: WaveSyncView | null): boolean {
  return view != null && view.status !== "idle";
}

export function openWaveSyncDialog() {
  set({ dialogOpen: true });
}

export function closeWaveSyncDialog() {
  set({ dialogOpen: false });
}

export function showWaveSyncNotice(text: string) {
  noticeKey += 1;
  set({ notice: { key: noticeKey, text } });
}

export function clearWaveSyncNotice() {
  set({ notice: null });
}

/**
 * Put library tracks in the session's queue and say what happened: how many
 * went in, how many could not be shared, or why none did.
 */
export async function addToWaveSync(
  trackIds: number[],
  t: TFunction,
  errorText: (err: unknown) => string,
) {
  try {
    const { added, skipped } = await wavesyncAdd(trackIds);
    showWaveSyncNotice(
      skipped > 0
        ? t("wavesync.notice.addedSkipped", { count: added, skipped })
        : t("wavesync.notice.added", { count: added }),
    );
  } catch (err) {
    showWaveSyncNotice(errorText(err));
  }
}
