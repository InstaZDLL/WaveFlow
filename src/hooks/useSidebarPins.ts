import { createContext, useContext } from "react";

import {
  setSidebarPin,
  type PinKind,
  type SidebarPin,
} from "../lib/tauri/sidebarPins";

/** DOM event fired after a pin is added or removed (or a pinned album's
 *  cover changes), so the provider re-reads the list. */
export const SIDEBAR_PINS_EVENT = "waveflow:sidebar-pins-changed";

/** Pin or unpin, then tell the provider. */
export async function toggleSidebarPin(
  kind: PinKind,
  id: number,
  pinned: boolean,
): Promise<void> {
  await setSidebarPin(kind, id, pinned);
  window.dispatchEvent(new CustomEvent(SIDEBAR_PINS_EVENT));
}

interface SidebarPinsContextValue {
  /** The pinned albums, artists and playlists, in pinning order. */
  pins: SidebarPin[];
  isPinned: (kind: PinKind, id: number) => boolean;
}

export const SidebarPinsContext = createContext<SidebarPinsContextValue | null>(
  null,
);

/** The sidebar pins, read once for the whole app by `SidebarPinsProvider`:
 *  the sidebar, the playlist menus and the album and artist grids all ask,
 *  and one read per change serves them all. */
export function useSidebarPins(): SidebarPinsContextValue {
  const ctx = useContext(SidebarPinsContext);
  if (!ctx) {
    throw new Error("useSidebarPins must be used inside SidebarPinsProvider");
  }
  return ctx;
}
