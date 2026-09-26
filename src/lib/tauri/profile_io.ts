import { invoke } from "@tauri-apps/api/core";

/**
 * Bundle the active profile (or `profileId` when given) into a
 * `.waveflow` archive at `targetPath`. Throws on I/O failure.
 */
export function exportProfile(
  targetPath: string,
  profileId?: number | null,
): Promise<void> {
  return invoke<void>("export_profile", {
    profileId: profileId ?? null,
    targetPath,
  });
}

/** The profile an import created. `name` is the archive's own unless
 * another profile already held it, in which case it gains a " (2)"-style
 * suffix (#767). */
export interface ImportedProfile {
  id: number;
  name: string;
}

/**
 * Import a `.waveflow` archive as a brand-new profile. Returns the
 * new profile's id and name. The new profile is **not** auto-activated
 * — the caller decides when to switch.
 */
export function importProfile(
  sourcePath: string,
  name?: string | null,
): Promise<ImportedProfile> {
  return invoke<ImportedProfile>("import_profile", {
    sourcePath,
    name: name ?? null,
  });
}
