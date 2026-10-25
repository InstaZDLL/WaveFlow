import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { useProfile } from "./useProfile";
import {
  listSidebarPins,
  setSidebarPin,
  type PinKind,
  type SidebarPin,
} from "../lib/tauri/sidebarPins";

/** DOM event fired after a pin is added or removed, so the sidebar and
 *  every open menu re-read the list. */
export const SIDEBAR_PINS_EVENT = "waveflow:sidebar-pins-changed";

/** Pin or unpin, then tell every reader. */
export async function toggleSidebarPin(
  kind: PinKind,
  id: number,
  pinned: boolean,
): Promise<void> {
  await setSidebarPin(kind, id, pinned);
  window.dispatchEvent(new CustomEvent(SIDEBAR_PINS_EVENT));
}

/**
 * The pinned albums, artists and playlists, in pinning order.
 *
 * Re-read when a pin changes, when the profile changes (pins are per
 * profile) and after a rescan, which can bring a pinned album's tracks
 * back or take them away. A failed read keeps the last list: the
 * sidebar would rather show a stale pin than lose them all over a blip.
 */
export function useSidebarPins() {
  const { activeProfile } = useProfile();
  const [pins, setPins] = useState<SidebarPin[]>([]);
  // Newest read wins: a pin toggled twice quickly fires two reads.
  const seqRef = useRef(0);

  const refresh = useCallback(() => {
    const seq = ++seqRef.current;
    listSidebarPins().then(
      (list) => {
        if (seq === seqRef.current) setPins(list);
      },
      (err: unknown) => console.error("[useSidebarPins] list failed", err),
    );
  }, []);

  useEffect(() => {
    refresh();
    window.addEventListener(SIDEBAR_PINS_EVENT, refresh);
    let unlisten: UnlistenFn | null = null;
    let disposed = false;
    listen("library:rescanned", refresh).then(
      (off) => {
        if (disposed) off();
        else unlisten = off;
      },
      (err: unknown) => console.error("[useSidebarPins] listen failed", err),
    );
    const counter = seqRef;
    return () => {
      disposed = true;
      window.removeEventListener(SIDEBAR_PINS_EVENT, refresh);
      unlisten?.();
      // A read still in flight lands nowhere.
      counter.current += 1;
    };
  }, [refresh, activeProfile?.id]);

  const isPinned = useCallback(
    (kind: PinKind, id: number) =>
      pins.some((pin) => pin.kind === kind && pin.id === id),
    [pins],
  );

  return { pins, isPinned };
}
