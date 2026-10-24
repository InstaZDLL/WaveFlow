import { useProfileBooleanSetting } from "./useProfileSetting";
import { SETTINGS } from "../lib/settingsRegistry";

const KEY = "playback.autoplay";

/** Broadcast after a successful write so every mounted consumer
 *  re-reads in one go. */
export const AUTOPLAY_EVENT = "waveflow:autoplay";

/** Default OFF: it adds tracks to the queue nobody picked. */
const DEFAULT_ENABLED = SETTINGS[KEY].defaultValue;

export interface Autoplay {
  enabled: boolean;
  /** `false` until the stored preference has been read for the active
   *  profile. */
  resolved: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: when the queue runs out with repeat off, add
 * related tracks from the library and keep playing.
 *
 * The backend reads the same key in `autoplay::is_enabled` each time the
 * queue would run out, so a change applies from the next end of queue
 * with nothing to push to the engine.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useAutoplay(): Autoplay {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: AUTOPLAY_EVENT,
    label: "useAutoplay",
  });
  return { enabled: value, resolved: ready, setEnabled: setValue };
}
