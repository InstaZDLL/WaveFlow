import { useCallback } from "react";

import { usePlayer } from "./usePlayer";
import { episodeInPlayer } from "../lib/playerSources";
import { podcastPlayEpisode } from "../lib/tauri/podcast";

/**
 * Goes to a moment of an episode, a chapter or a line of its transcript: a seek when
 * it is playing or paused, and otherwise it starts there. The decoder ignores a seek
 * with nothing open, which is also the case while one still loads, so that one starts
 * again at the moment asked for. Rejects with a word when it could not start, and
 * what plays keeps playing.
 */
export function useEpisodeJump() {
  const { currentTrack, playbackState, seek } = usePlayer();
  const inPlayer = episodeInPlayer(currentTrack, playbackState);
  const canSeek = playbackState === "playing" || playbackState === "paused";
  const jump = useCallback(
    (episodeId: number, ms: number): Promise<void> =>
      canSeek && episodeId === inPlayer
        ? seek(ms)
        : podcastPlayEpisode(episodeId, ms),
    [canSeek, inPlayer, seek],
  );
  return { jump, inPlayer };
}
