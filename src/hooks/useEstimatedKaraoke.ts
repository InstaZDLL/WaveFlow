import { useProfileBooleanSetting } from "./useProfileSetting";

/** Broadcast after a write, so every mounted lyrics view re-reads. */
export const ESTIMATED_KARAOKE_EVENT = "waveflow:lyrics-estimate-words-changed";

/**
 * Whether line-synced lyrics get an estimated word-by-word highlight
 * (#716), per profile in `profile_setting['lyrics.estimate_words']`.
 * Default off: the estimate is a guess, and a guess drawn as if it were
 * timing should be something the user asked for.
 */
export function useEstimatedKaraokeSetting() {
  return useProfileBooleanSetting({
    key: "lyrics.estimate_words",
    defaultValue: false,
    event: ESTIMATED_KARAOKE_EVENT,
    label: "useEstimatedKaraokeSetting",
  });
}

/** Just the value, for the lyrics views. */
export function useEstimatedKaraoke(): boolean {
  return useEstimatedKaraokeSetting().value;
}
