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

/**
 * Import a `.waveflow` archive as a brand-new profile. Returns the
 * new profile id. The new profile is **not** auto-activated — the
 * caller decides when to switch.
 */
export function importProfile(
  sourcePath: string,
  name?: string | null,
): Promise<number> {
  return invoke<number>("import_profile", {
    sourcePath,
    name: name ?? null,
  });
}
