import { invoke } from "@tauri-apps/api/core";

/**
 * Profile row as returned by the Rust backend (mirrors `commands::profile::Profile`).
 *
 * Field names use snake_case because `serde` serializes them as-is; the Rust
 * struct derives `Serialize` with the default attribute policy.
 */
export interface Profile {
  id: number;
  name: string;
  color_id: string;
  avatar_hash: string | null;
  data_dir: string;
  created_at: number;
  last_used_at: number;
}

export interface CreateProfileInput {
  name: string;
  color_id?: string;
  avatar_hash?: string | null;
}

export function listProfiles(): Promise<Profile[]> {
  return invoke<Profile[]>("list_profiles");
}

export function getActiveProfile(): Promise<Profile | null> {
  return invoke<Profile | null>("get_active_profile");
}

export function createProfile(input: CreateProfileInput): Promise<Profile> {
  return invoke<Profile>("create_profile", { input });
}

/**
 * Rename an existing profile in place. Safe to call against the
 * active profile — only `app.db` is touched. Used by the onboarding
 * wizard so the auto-created "Default" profile can be renamed
 * without forcing a full create-then-rescan flow.
 */
export function renameProfile(profileId: number, name: string): Promise<Profile> {
  return invoke<Profile>("rename_profile", { profileId, name });
}

export function switchProfile(profileId: number): Promise<Profile> {
  return invoke<Profile>("switch_profile", { profileId });
}

export function deactivateProfile(): Promise<void> {
  return invoke<void>("deactivate_profile");
}

/**
 * Permanently delete a profile. The backend refuses if the profile is active
 * (switch first) or if it's the last remaining profile.
 */
export function deleteProfile(profileId: number): Promise<void> {
  return invoke<void>("delete_profile", { profileId });
}

/** Read a single value from the active profile's `profile_setting` table. */
export function getProfileSetting(key: string): Promise<string | null> {
  return invoke<string | null>("get_profile_setting", { key });
}

/** Upsert a typed value into the active profile's `profile_setting` table. */
export function setProfileSetting(
  key: string,
  value: string,
  valueType: "bool" | "int" | "string" | "json",
): Promise<void> {
  return invoke<void>("set_profile_setting", { key, value, valueType });
}
