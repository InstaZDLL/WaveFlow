import type { ReactNode } from "react";
import { useEffectsLevel } from "../hooks/useEffectsLevel";

/**
 * Applies the per-profile effects level to the document.
 *
 * No context value, like `ContrastProvider`: the preference is readable
 * anywhere through `useEffectsLevel`, and `useProfileSetting` keeps
 * every consumer in sync. What this owns is the side effect that must
 * happen once per window — stamping `data-effects` on the root — and it
 * sits under `ProfileProvider` because the value is per profile.
 */
export function EffectsProvider({ children }: { children: ReactNode }) {
  useEffectsLevel();
  return <>{children}</>;
}
