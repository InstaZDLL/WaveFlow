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
    document.documentElement.dataset.isPlaying = isPlaying
      ? "true"
      : "false";
  }, [isPlaying]);

  return null;
}
