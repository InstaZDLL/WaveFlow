import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { useProfile } from "./useProfile";
import {
  listSidebarPins,
  setSidebarPin,
  type PinKind,
  type SidebarPin,
} from "../lib/tauri/sidebarPins";

/** DOM event fired after a pin is added or removed (or a pinned album's
 *  cover changes), so the sidebar and every open menu re-read the list. */
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

const NO_PINS: SidebarPin[] = [];

/**
 * The pinned albums, artists and playlists, in pinning order.
 *
 * Re-read when a pin changes (or a pinned album's cover: the cover picker
 * fires the same event), when the profile changes, after a rescan — which
 * can bring a pinned album's tracks back or take them away — and when an
 * artist is updated (a picture arriving from Deezer).
 *
 * Pins are per profile and point at that profile's rows, so the list is
 * kept with the profile it was read for and shown only while that profile
 * is the active one: a switch shows nothing until the new list lands,
 * never the last profile's pins. A failed re-read of the same profile
 * keeps the last list; the sidebar would rather show a stale pin than
 * lose them all over a blip.
 */
export function useSidebarPins() {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const [read, setRead] = useState<{
    profileId: number | null;
    pins: SidebarPin[];
  }>({ profileId: null, pins: NO_PINS });
  // Newest read wins: a pin toggled twice quickly fires two reads.
  const seqRef = useRef(0);

  useEffect(() => {
    const refresh = () => {
      const seq = ++seqRef.current;
      listSidebarPins().then(
        (pins) => {
          if (seq === seqRef.current) setRead({ profileId, pins });
        },
        (err: unknown) => console.error("[useSidebarPins] list failed", err),
      );
    };
    refresh();
    window.addEventListener(SIDEBAR_PINS_EVENT, refresh);
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    for (const event of ["library:rescanned", "artist:updated"]) {
      listen(event, refresh).then(
        (off) => {
          if (disposed) off();
          else unlisteners.push(off);
        },
        (err: unknown) => console.error("[useSidebarPins] listen failed", err),
      );
    }
    const counter = seqRef;
    return () => {
      disposed = true;
      window.removeEventListener(SIDEBAR_PINS_EVENT, refresh);
      for (const off of unlisteners) off();
      // A read still in flight lands nowhere.
      counter.current += 1;
    };
  }, [profileId]);

  const pins = read.profileId === profileId ? read.pins : NO_PINS;

  const isPinned = useCallback(
    (kind: PinKind, id: number) =>
      pins.some((pin) => pin.kind === kind && pin.id === id),
    [pins],
  );

  return { pins, isPinned };
}
