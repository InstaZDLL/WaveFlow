import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "ui.artist_hero";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + the artist page) re-reads in one go. */
export const ARTIST_HERO_EVENT = "waveflow:artist-hero";

/** Default ON — the hero is the baseline look of the artist page (a
 *  static backdrop, not extra motion), so it ships enabled and the
 *  toggle is there for users who prefer the flat header. */
const DEFAULT_ENABLED = true;

export interface ArtistHero {
  enabled: boolean;
  /** `false` until the stored preference has been read for the active
   *  profile. The hero gates on this: with an ON default, painting
   *  before the read lands would flash a backdrop at users who turned
   *  it off. The Settings card doesn't need it — its checkbox settles. */
  resolved: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: paint a full-bleed hero backdrop behind the
 * artist detail header (issue #482) — the wide TheAudioDB fanart when the
 * artist has one, a blurred version of the square photo otherwise. Default
 * ON.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useArtistHero(): ArtistHero {
  const { value, ready, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: ARTIST_HERO_EVENT,
    label: "useArtistHero",
  });
  return { enabled: value, resolved: ready, setEnabled: setValue };
}
