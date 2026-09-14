import type { ReactNode } from "react";
import { useContrastMode } from "../hooks/useContrastMode";

/**
 * Applies the per-profile high-contrast preference to the document
 * (#596).
 *
 * It carries no context value, unlike its siblings in this folder: the
 * preference is readable anywhere through `useContrastMode`, and
 * `useProfileSetting` already keeps every mounted consumer in sync
 * through its broadcast event. What this component owns is the single
 * side effect that must happen exactly once per window — stamping
 * `data-contrast` on the document root. It sits in the tree, rather
 * than in a module-level effect, because the value is per profile and
 * therefore needs `ProfileProvider` above it.
 *
 * The bootstrap script in `index.html` has already stamped the cached
 * choice by the time this mounts; the effect inside the hook is what
 * corrects it once the active profile's row comes back.
 */
export function ContrastProvider({ children }: { children: ReactNode }) {
  useContrastMode();
  return <>{children}</>;
}
