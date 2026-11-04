import { useCallback } from "react";

import { usePlayer } from "./usePlayer";
import { episodeInPlayer } from "../lib/playerSources";
import { podcastPlayEpisode } from "../lib/tauri/podcast";

/**
 * Goes to a moment of an episode, a chapter or a line of its transcript: a seek when
 * the player holds it, and otherwise it starts there. Rejects with a word when
 * it could not start, and what plays keeps playing.
 */
export function useEpisodeJump() {
  const { currentTrack, playbackState, seek } = usePlayer();
  const inPlayer = episodeInPlayer(currentTrack, playbackState);
  const jump = useCallback(
    (episodeId: number, ms: number): Promise<void> =>
      episodeId === inPlayer ? seek(ms) : podcastPlayEpisode(episodeId, ms),
    [inPlayer, seek],
  );
  return { jump, inPlayer };
}
