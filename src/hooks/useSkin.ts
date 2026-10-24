import { createContext, useContext } from "react";
import type { SkinPreset } from "../lib/skins";

interface SkinContextValue {
  /** Currently active skin (one of `SKIN_PRESETS`). */
  skin: SkinPreset;
  /** Switch to a skin by id. Persists across launches. Resolves once
   *  the change is saved and applied, rejects if it could not be (and
   *  raises `skinError` either way). A caller may ignore the promise —
   *  its failure is already handled here. A caller acting for a profile it captured earlier — a
   *  Settings page reset — passes it as `expectedProfileId`: the change is
   *  refused when another profile is active by then, and the backend
   *  refuses a write that a switch overtakes on the way, so one profile's
   *  choice never lands in another's. */
  setSkinId: (id: string, expectedProfileId?: number | null) => Promise<void>;
  /** A skin change or Studio recovery could not be saved. */
  skinError: boolean;
}

export const SkinContext = createContext<SkinContextValue | null>(null);

export function useSkin() {
  const context = useContext(SkinContext);
  if (!context) throw new Error("useSkin must be used within SkinProvider");
  return context;
}
