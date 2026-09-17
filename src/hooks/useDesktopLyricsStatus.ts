import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  DESKTOP_LYRICS_STATE_EVENT,
  closeDesktopLyrics,
  getDesktopLyricsStatus,
  openDesktopLyrics,
  setDesktopLyricsLocked,
  type DesktopLyricsStatus,
} from "../lib/tauri/desktopLyrics";

const CLOSED: DesktopLyricsStatus = {
  open: false,
  locked: false,
  wayland: false,
};

/**
 * Whether the desktop lyrics window is open and locked, as the backend
 * says (issue #582), plus the three actions every control surface offers.
 *
 * The state comes from the backend's broadcast, never from what this
 * surface last asked for: the tray menu changes it without any React
 * code running, and a surface that trusted its own last click would show
 * "locked" after the tray had unlocked it.
 */
export function useDesktopLyricsStatus() {
  const [status, setStatus] = useState<DesktopLyricsStatus>(CLOSED);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    // Subscribe before reading, so a change landing between the two is
    // not lost. The read can still come back after a broadcast that is
    // newer than what it read, so it only applies while no broadcast has
    // arrived: once one has, the broadcast is the truth.
    let heardBroadcast = false;
    listen<DesktopLyricsStatus>(DESKTOP_LYRICS_STATE_EVENT, (event) => {
      heardBroadcast = true;
      setStatus(event.payload);
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
        return getDesktopLyricsStatus();
      })
      .then((initial) => {
        if (!cancelled && !heardBroadcast) setStatus(initial);
      })
      .catch((err) => {
        console.warn("[useDesktopLyricsStatus] status unavailable", err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const toggleOpen = useCallback(() => {
    const action = status.open ? closeDesktopLyrics() : openDesktopLyrics();
    action.catch((err) => {
      console.error("[useDesktopLyricsStatus] open/close failed", err);
    });
  }, [status.open]);

  const setLocked = useCallback((locked: boolean) => {
    setDesktopLyricsLocked(locked).catch((err) => {
      console.error("[useDesktopLyricsStatus] lock failed", err);
    });
  }, []);

  return { status, toggleOpen, setLocked };
}
