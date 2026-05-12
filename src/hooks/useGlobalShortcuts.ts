import { useEffect, useRef, useState } from "react";
import { usePlayer } from "./usePlayer";
import {
  comboFromEvent,
  DEFAULT_BINDINGS,
  loadBindings,
  SHORTCUTS_CHANGED_EVENT,
  type ShortcutAction,
  type ShortcutBindings,
} from "../lib/shortcuts";
import { toggleLikeTrack } from "../lib/tauri/track";

/**
 * Wires global keyboard shortcuts to PlayerContext actions. Mounted
 * once at the top of the app (AppLayout). Skips dispatch when the
 * focused element is an input/textarea/contenteditable so typing
 * "S" in a search box doesn't toggle shuffle.
 */
export function useGlobalShortcuts() {
  const player = usePlayer();
  const [bindings, setBindings] = useState<ShortcutBindings>(DEFAULT_BINDINGS);

  // Snapshot the latest player + bindings into a ref so the keydown
  // listener doesn't re-attach on every prop change. Re-attaching the
  // listener also drops keys held in flight on rare devices (game
  // controllers exposed as keyboards).
  const playerRef = useRef(player);
  const bindingsRef = useRef(bindings);
  useEffect(() => {
    playerRef.current = player;
  }, [player]);
  useEffect(() => {
    bindingsRef.current = bindings;
  }, [bindings]);

  // Hydrate + listen for in-app changes from the Settings panel.
  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      loadBindings()
        .then((b) => {
          if (!cancelled) setBindings(b);
        })
        .catch(() => {});
    };
    refresh();
    window.addEventListener(SHORTCUTS_CHANGED_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(SHORTCUTS_CHANGED_EVENT, refresh);
    };
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable) {
          return;
        }
      }

      const combo = comboFromEvent(event);
      if (!combo) return;

      const action = (
        Object.keys(bindingsRef.current) as ShortcutAction[]
      ).find((a) => bindingsRef.current[a] === combo);
      if (!action) return;

      event.preventDefault();
      void dispatch(action, playerRef.current);
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/** Volume nudge per arrow-up / arrow-down press, in 0-100 units. */
const VOLUME_STEP = 5;

async function dispatch(
  action: ShortcutAction,
  player: ReturnType<typeof usePlayer>,
) {
  switch (action) {
    case "togglePlayback":
      await player.togglePlayback();
      break;
    case "next":
      await player.next();
      break;
    case "previous":
      await player.previous();
      break;
    case "volumeUp":
      player.setVolume(Math.min(100, player.volume + VOLUME_STEP));
      break;
    case "volumeDown":
      player.setVolume(Math.max(0, player.volume - VOLUME_STEP));
      break;
    case "toggleMute":
      player.toggleMute();
      break;
    case "toggleShuffle":
      await player.toggleShuffle();
      break;
    case "cycleRepeat":
      await player.cycleRepeatMode();
      break;
    case "toggleQueue":
      player.toggleQueue();
      break;
    case "toggleNowPlaying":
      player.toggleNowPlaying();
      break;
    case "toggleLyrics":
      player.toggleLyrics();
      break;
    case "toggleLike": {
      const track = player.currentTrack;
      if (!track) return;
      try {
        await toggleLikeTrack(track.id);
      } catch (err) {
        console.error("[shortcuts] toggle like failed", err);
      }
      break;
    }
  }
}
