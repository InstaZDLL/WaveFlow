import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { noteDiscovery } from "../lib/discovery";
import {
  DESKTOP_LYRICS_STATE_EVENT,
  type DesktopLyricsStatus,
} from "../lib/tauri/desktopLyrics";

/**
 * The first-steps ticks whose use is only seen from the backend. The
 * desktop lyrics open from the player's menu, from Settings and from the
 * tray, which is the backend's own menu: the window's state event is the
 * one signal that covers all three.
 */
export function useDiscoveryWatchers() {
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    let wasOpen = false;
    listen<DesktopLyricsStatus>(DESKTOP_LYRICS_STATE_EVENT, (event) => {
      const open = event.payload.open;
      if (open && !wasOpen) noteDiscovery("desktopLyrics");
      wasOpen = open;
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch((err) =>
        console.error("[useDiscoveryWatchers] listen failed", err),
      );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
