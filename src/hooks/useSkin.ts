import { createContext, useContext } from "react";
import type { SkinPreset } from "../lib/skins";

interface SkinContextValue {
  /** Currently active skin (one of `SKIN_PRESETS`). */
  skin: SkinPreset;
  /** Switch to a skin by id. Persists across launches. */
  setSkinId: (id: string) => void;
}

export const SkinContext = createContext<SkinContextValue | null>(null);

export function useSkin() {
  const context = useContext(SkinContext);
  if (!context) throw new Error("useSkin must be used within SkinProvider");
  return context;
}
