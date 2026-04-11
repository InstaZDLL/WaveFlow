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

export function switchProfile(profileId: number): Promise<Profile> {
  return invoke<Profile>("switch_profile", { profileId });
}

export function deactivateProfile(): Promise<void> {
  return invoke<void>("deactivate_profile");
}
