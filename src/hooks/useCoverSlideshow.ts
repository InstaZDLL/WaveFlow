import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "ui.cover_slideshow";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + each now-playing surface) re-reads in one go. */
export const COVER_SLIDESHOW_EVENT = "waveflow:cover-slideshow";

const DEFAULT_ENABLED = false;

export interface CoverSlideshow {
  enabled: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: gently crossfade the now-playing cover with the
 * artist photo (issue #466) — a living backdrop built from images already in
 * the library, no plugin. Default OFF (the static cover shows until the user
 * opts in). Read by the now-playing surfaces + the Settings card.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useCoverSlideshow(): CoverSlideshow {
  const { value, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: COVER_SLIDESHOW_EVENT,
    label: "useCoverSlideshow",
  });
  return { enabled: value, setEnabled: setValue };
}
