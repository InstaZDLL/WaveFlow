import { createContext, useContext } from "react";
import type { SkinPreset } from "../lib/skins";

interface SkinContextValue {
  /** Currently active skin (one of `SKIN_PRESETS`). */
  skin: SkinPreset;
  /** Switch to a skin by id. Persists across launches. Resolves once
   *  the change is saved and applied, rejects if it could not be (and
   *  raises `skinError` either way). A caller may ignore the promise —
   *  its failure is already handled here. */
  setSkinId: (id: string) => Promise<void>;
  /** A skin change or Studio recovery could not be saved. */
  skinError: boolean;
}

export const SkinContext = createContext<SkinContextValue | null>(null);

export function useSkin() {
  const context = useContext(SkinContext);
  if (!context) throw new Error("useSkin must be used within SkinProvider");
  return context;
}
