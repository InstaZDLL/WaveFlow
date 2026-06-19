import { useCallback, useEffect, useState, type ReactNode } from "react";
import { SkinContext } from "../hooks/useSkin";
import { useProfile } from "../hooks/useProfile";
import {
  applySkin,
  DEFAULT_SKIN_ID,
  findSkin,
  SKIN_PRESETS,
  type SkinPreset,
} from "../lib/skins";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

const PROFILE_SETTING_KEY = "appearance.skin.id";
// First-paint cache. Source of truth lives in the active profile's
// `profile_setting`; see [`ThemeContext`](./ThemeContext.tsx) for the
// same hybrid storage rationale.
const SKIN_CACHE_KEY = "waveflow.skin.id";

const readCachedSkin = (): SkinPreset => {
  if (typeof window === "undefined") return findSkin(DEFAULT_SKIN_ID);
  try {
    const stored = window.localStorage.getItem(SKIN_CACHE_KEY);
    return findSkin(stored);
  } catch {
    return findSkin(DEFAULT_SKIN_ID);
  }
};

const writeCachedSkin = (id: string) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SKIN_CACHE_KEY, id);
  } catch {
    // localStorage unavailable — DB value still wins on next launch.
  }
};

/**
 * Skin provider. Mirrors [`ThemeProvider`](./ThemeContext.tsx)'s
 * hybrid storage (DB source-of-truth + localStorage first-paint
 * cache) without the View Transitions API integration — a skin swap
 * is a subtler change than a theme swap and a plain re-render is
 * enough. Iterating on a richer cross-fade can land in a follow-up
 * if user feedback asks for it.
 */
export function SkinProvider({ children }: { children: ReactNode }) {
  const [skin, setSkin] = useState<SkinPreset>(readCachedSkin);
  const { activeProfile } = useProfile();

  useEffect(() => {
    applySkin(skin);
  }, [skin]);

  // Source-of-truth read on mount + profile switch. Same pattern as
  // `ThemeProvider` — the cache may hold the previous profile's
  // choice, the DB row wins.
  useEffect(() => {
    if (!activeProfile) return;
    let cancelled = false;
    (async () => {
      try {
        const stored = await getProfileSetting(PROFILE_SETTING_KEY);
        if (cancelled) return;
        if (stored) {
          const fromDb = findSkin(stored);
          if (fromDb.id !== skin.id) {
            setSkin(fromDb);
            writeCachedSkin(fromDb.id);
          }
          return;
        }
        // First-time seed from the currently applied skin so future
        // reads have something to anchor on.
        await setProfileSetting(PROFILE_SETTING_KEY, skin.id, "string");
      } catch (err) {
        console.warn("[SkinContext] profile-scoped skin load failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProfile?.id]);

  const setSkinId = useCallback((id: string) => {
    const next = findSkin(id);
    writeCachedSkin(next.id);
    void setProfileSetting(PROFILE_SETTING_KEY, next.id, "string").catch(
      (err) => console.warn("[SkinContext] profile-setting write failed", err),
    );
    setSkin(next);
  }, []);

  return (
    <SkinContext.Provider value={{ skin, setSkinId }}>
      {children}
    </SkinContext.Provider>
  );
}

export { SKIN_PRESETS };
