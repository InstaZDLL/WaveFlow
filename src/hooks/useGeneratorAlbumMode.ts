import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "playback.generator_album_mode";

/** Broadcast after a successful write so every mounted consumer
 *  re-reads in one go. */
export const GENERATOR_ALBUM_MODE_EVENT = "waveflow:generator-album-mode";

/** Default OFF. It changes what Mood Radio and the Daily Mix produce,
 *  so it should only ever happen because somebody asked for it. */
const DEFAULT_ENABLED = false;

export interface GeneratorAlbumMode {
  enabled: boolean;
  /** `false` until the stored preference has been read for the active
   *  profile. The Settings toggle doesn't need it — its checkbox
   *  settles — but a caller that gates a button on it would. */
  resolved: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: build Mood Radio and the Daily Mix out of
 * whole records rather than scattered tracks (#618).
 *
 * One setting for both, because it is one preference — someone who
 * listens to albums wants albums from anything that builds them a
 * session. The backend reads the same key in
 * `waveflow_core::album_playback::album_mode_enabled`.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useGeneratorAlbumMode(): GeneratorAlbumMode {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: GENERATOR_ALBUM_MODE_EVENT,
    label: "useGeneratorAlbumMode",
  });
  return { enabled: value, resolved: ready, setEnabled: setValue };
}
