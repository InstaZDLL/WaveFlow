import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "ui.motion_covers";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + each now-playing surface) re-reads in one go. */
export const MOTION_COVERS_EVENT = "waveflow:motion-covers";

const DEFAULT_ENABLED = true;

export interface MotionCovers {
  enabled: boolean;
  /** False until the profile's value has been read. */
  ready: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: show animated album covers at all — the plugin's
 * and the ones set by hand alike (#766). Default ON. It lives in Settings
 * rather than as a player button: the immersive top bar is already full on
 * a small screen, and this is a set-once choice, not a per-track one.
 *
 * Disabling the plugin was the only way to hide them before, and it could
 * not hide a hand-set cover. Read by `useAlbumMotionArtwork`, so every
 * surface that shows one obeys it and none has to be told separately.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useMotionCovers(): MotionCovers {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: MOTION_COVERS_EVENT,
    label: "useMotionCovers",
  });
  return { enabled: value, ready, setEnabled: setValue };
}
