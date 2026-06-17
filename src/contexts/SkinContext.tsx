import { useCallback, useEffect, useState, type ReactNode } from "react";
import { SkinContext } from "../hooks/useSkin";
import {
  applySkin,
  DEFAULT_SKIN_ID,
  findSkin,
  SKIN_PRESETS,
  type SkinPreset,
} from "../lib/skins";

const SKIN_STORAGE_KEY = "waveflow.skin.id";

const readStoredSkin = (): SkinPreset => {
  if (typeof window === "undefined") return findSkin(DEFAULT_SKIN_ID);
  try {
    const stored = window.localStorage.getItem(SKIN_STORAGE_KEY);
    return findSkin(stored);
  } catch {
    return findSkin(DEFAULT_SKIN_ID);
  }
};

const writeStoredSkin = (id: string) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(SKIN_STORAGE_KEY, id);
  } catch {
    // localStorage unavailable — skin won't survive next launch.
  }
};

/**
 * Skin provider. Mirrors [`ThemeProvider`](./ThemeContext.tsx)'s shape
 * but without the View Transitions API integration — a skin swap is a
 * subtler change than a theme swap and a plain re-render is enough.
 * Iterating on a richer cross-fade can land in a follow-up if user
 * feedback asks for it.
 */
export function SkinProvider({ children }: { children: ReactNode }) {
  const [skin, setSkin] = useState<SkinPreset>(readStoredSkin);

  useEffect(() => {
    applySkin(skin);
  }, [skin]);

  const setSkinId = useCallback((id: string) => {
    const next = findSkin(id);
    writeStoredSkin(next.id);
    setSkin(next);
  }, []);

  return (
    <SkinContext.Provider value={{ skin, setSkinId }}>
      {children}
    </SkinContext.Provider>
  );
}

export { SKIN_PRESETS };
