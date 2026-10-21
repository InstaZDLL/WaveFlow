import { useEffect, useRef, useState } from "react";
import { usePlayer } from "./usePlayer";
import {
  comboFromEvent,
  DEFAULT_BINDINGS,
  FOCUS_SEARCH_EVENT,
  SHORTCUTS_ALLOWED_IN_FIELDS,
  comboIsFieldSafe,
  loadBindings,
  SHORTCUTS_CHANGED_EVENT,
  type ShortcutAction,
  type ShortcutBindings,
} from "../lib/shortcuts";
import { toggleLikeTrack } from "../lib/tauri/track";
import { openFilesToPlay, openFolderToPlay } from "../lib/openToPlay";
import { isStreamTrack } from "../lib/playerSources";

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
      const combo = comboFromEvent(event);
      if (!combo) return;

      const action = (
        Object.keys(bindingsRef.current) as ShortcutAction[]
      ).find((a) => bindingsRef.current[a] === combo);
      if (!action) return;

      const target = event.target as HTMLElement | null;
      const firesInFields =
        SHORTCUTS_ALLOWED_IN_FIELDS.has(action) && comboIsFieldSafe(combo);
      if (target && !firesInFields) {
        const tag = target.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || target.isContentEditable) {
          return;
        }
      }

      // A held F11 auto-repeats: one press, one switch. After the field
      // check, so a binding that types in a field still repeats there.
      // Same for the pickers: a held Ctrl+O would stack dialogs.
      if (
        event.repeat &&
        (action === "toggleFullscreen" ||
          action === "openFiles" ||
          action === "openFolder")
      ) {
        event.preventDefault();
        return;
      }

      event.preventDefault();
      void dispatch(action, playerRef.current);
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/**
 * Fullscreen for the whole window, not just the immersive view. The
 * immersive view's own native fullscreen checks `isFullscreen()` before
 * entering and only restores what it changed, so the two compose: open
 * it in an already-fullscreen window and closing it leaves the window
 * fullscreen.
 */
async function toggleWindowFullscreen(): Promise<void> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();
    await win.setFullscreen(!(await win.isFullscreen()));
  } catch (err) {
    console.error("[shortcuts] toggle fullscreen failed", err);
  }
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
    case "focusSearch":
      window.dispatchEvent(new CustomEvent(FOCUS_SEARCH_EVENT));
      break;
    case "toggleFullscreen":
      await toggleWindowFullscreen();
      break;
    case "openFiles":
      await openFilesToPlay();
      break;
    case "openFolder":
      await openFolderToPlay();
      break;
    case "toggleLike": {
      const track = player.currentTrack;
      // A track with no library row has nothing to like, as its ♥ button
      // is hidden.
      if (!track || isStreamTrack(track)) return;
      try {
        await toggleLikeTrack(track.id);
      } catch (err) {
        console.error("[shortcuts] toggle like failed", err);
      }
      break;
    }
  }
}
