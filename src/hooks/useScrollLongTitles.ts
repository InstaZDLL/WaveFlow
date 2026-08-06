import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "ui.scroll_long_titles";

/** Broadcast after a successful write so every mounted consumer (the
 *  Settings card + each `MarqueeText`) re-reads in one go. */
export const SCROLL_LONG_TITLES_EVENT = "waveflow:scroll-long-titles";

const DEFAULT_ENABLED = true;

export interface ScrollLongTitles {
  enabled: boolean;
  setEnabled: (next: boolean) => Promise<void>;
}

/**
 * Per-profile preference: scroll long titles (the marquee in the
 * PlayerBar + immersive view) end-to-end instead of truncating them.
 * Default ON. Turning it off makes every `MarqueeText` render static +
 * truncated.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useScrollLongTitles(): ScrollLongTitles {
  const { value, setValue } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_ENABLED,
    event: SCROLL_LONG_TITLES_EVENT,
    label: "useScrollLongTitles",
  });
  return { enabled: value, setEnabled: setValue };
}
