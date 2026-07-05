import { useEffect } from "react";
import { usePlayer } from "../../hooks/usePlayer";

/**
 * Mirrors the player's `isPlaying` state onto `<html>` as
 * `data-is-playing="true|false"`. The attribute is namespace
 * neutral — any skin's CSS can read it via the attribute
 * selector to drive playback-tied animations (vinyl spin,
 * EQ bars, breathing border etc.) without each skin needing
 * its own React subscription.
 *
 * Pulse uses it to spin the player-bar cover thumbnail like
 * a vinyl when audio is rolling. Future skins can opt in by
 * keying their own animations off the same attribute.
 *
 * Renders nothing — pure side-effect component mounted once
 * by AppLayout next to SkinAmbientBackdrop.
 */
export function SkinPlayingState() {
  const { isPlaying } = usePlayer();

  useEffect(() => {
    document.documentElement.dataset.isPlaying = isPlaying ? "true" : "false";
    // Cleanup: drop the attribute entirely on unmount so a future
    // refactor (test harness teardown, route-level remount, etc.)
    // doesn't leave a stale `data-is-playing="true"` on the
    // documentElement that a CSS animation would silently keep
    // running for. In normal app life this component lives at
    // AppLayout root and never unmounts; the cleanup is purely
    // defensive.
    return () => {
      delete document.documentElement.dataset.isPlaying;
    };
  }, [isPlaying]);

  return null;
}
