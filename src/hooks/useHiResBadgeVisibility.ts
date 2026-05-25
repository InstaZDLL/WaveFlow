import { useCallback, useEffect, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

/** Per-profile `profile_setting` key gating every Hi-Res / DSD pill. */
const KEY = "ui.show_hi_res_badge";

/**
 * Window event broadcast by the Settings toggle after a write so every
 * mounted `HiResBadge` + the player-bar quality label re-read in one
 * go. Same pattern as `usePlayerBarLayout`.
 */
export const HI_RES_BADGE_EVENT = "waveflow:hi-res-badge-visibility";

const DEFAULT_VISIBLE = true;

function parseVisible(raw: string | null): boolean {
  if (raw == null) return DEFAULT_VISIBLE;
  return raw !== "false" && raw !== "0";
}

export interface HiResBadgeVisibility {
  visible: boolean;
  setVisible: (next: boolean) => Promise<void>;
}

/**
 * Hook returning the user's preference for the Hi-Res / DSD pill that
 * decorates track lists, album grids, and the player bar. Default
 * **on** because the badge is part of WaveFlow's audiophile identity;
 * the toggle lets users who find it noisy turn it off in one click.
 *
 * Stored in `profile_setting['ui.show_hi_res_badge']` (per-profile so
 * a kid's profile can stay clean while the audiophile profile keeps
 * the pills). Persistence + event handling mirror `usePlayerBarLayout`.
 */
export function useHiResBadgeVisibility(): HiResBadgeVisibility {
  const [visible, setVisibleState] = useState<boolean>(DEFAULT_VISIBLE);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setVisibleState(parseVisible(raw));
      } catch (err) {
        console.error("[useHiResBadgeVisibility] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(HI_RES_BADGE_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(HI_RES_BADGE_EVENT, refresh);
    };
  }, []);

  const setVisible = useCallback(async (next: boolean) => {
    setVisibleState(next);
    try {
      await setProfileSetting(KEY, next ? "true" : "false", "bool");
      window.dispatchEvent(new CustomEvent(HI_RES_BADGE_EVENT));
    } catch (err) {
      console.error("[useHiResBadgeVisibility] write failed", err);
    }
  }, []);

  return { visible, setVisible };
}
