import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { useProfile } from "../hooks/useProfile";
import {
  SIDEBAR_PINS_EVENT,
  SidebarPinsContext,
} from "../hooks/useSidebarPins";
import {
  listSidebarPins,
  type PinKind,
  type SidebarPin,
} from "../lib/tauri/sidebarPins";

const NO_PINS: SidebarPin[] = [];

/**
 * Owns the sidebar pins for the whole app.
 *
 * Re-reads when a pin changes (or a pinned album's cover: the cover picker
 * fires the same event), when the profile changes, after a rescan — which
 * can bring a pinned album's tracks back or take them away — and when an
 * artist is updated (a picture arriving from Deezer).
 *
 * Pins are per profile and point at that profile's rows, so the list is
 * kept with the profile it was read for and given out only while that
 * profile is the active one: a switch shows nothing until the new list
 * lands, never the last profile's pins. A failed re-read of the same
 * profile keeps the last list; the sidebar would rather show a stale pin
 * than lose them all over a blip.
 */
export function SidebarPinsProvider({ children }: { children: ReactNode }) {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const [read, setRead] = useState<{
    profileId: number | null;
    pins: SidebarPin[];
  }>({ profileId: null, pins: NO_PINS });
  // Newest read wins: a pin toggled twice quickly fires two reads.
  const seqRef = useRef(0);

  useEffect(() => {
    if (profileId == null) return;
    const refresh = () => {
      const seq = ++seqRef.current;
      listSidebarPins().then(
        (pins) => {
          if (seq === seqRef.current) setRead({ profileId, pins });
        },
        (err: unknown) =>
          console.error("[SidebarPinsProvider] list failed", err),
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
        (err: unknown) =>
          console.error("[SidebarPinsProvider] listen failed", err),
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

  const value = useMemo(() => ({ pins, isPinned }), [pins, isPinned]);

  return (
    <SidebarPinsContext.Provider value={value}>
      {children}
    </SidebarPinsContext.Provider>
  );
}
