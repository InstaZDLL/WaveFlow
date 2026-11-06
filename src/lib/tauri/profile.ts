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
  /** BLAKE3 of the stored picture, or `null` when the profile has none.
   *  Changes with every new picture, so it keys the image URL. */
  avatar_hash: string | null;
  data_dir: string;
  created_at: number;
  last_used_at: number;
  /** Absolute path of the profile's picture (`profiles/<id>/avatar.png`),
   *  resolved by the backend; `null` when there is none. */
  avatar_path: string | null;
}

export interface CreateProfileInput {
  name: string;
  color_id?: string;
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
export function renameProfile(
  profileId: number,
  name: string,
): Promise<Profile> {
  return invoke<Profile>("rename_profile", { profileId, name });
}

/**
 * Make the image at `filePath` the profile's picture. The backend reads
 * it (PNG, JPEG, WebP or GIF, 20 MB at most), keeps the centre square at
 * 256 px and stores that copy only; the original's path is not kept.
 * Rejects with `avatar: too-large`, `avatar: unsupported` or
 * `avatar: unreadable` when the file cannot be one — see
 * [`avatarRefusalKey`].
 */
export function setProfileAvatar(
  profileId: number,
  filePath: string,
): Promise<Profile> {
  return invoke<Profile>("set_profile_avatar", { profileId, filePath });
}

/** Remove the profile's picture; its initial is drawn again. */
export function clearProfileAvatar(profileId: number): Promise<Profile> {
  return invoke<Profile>("clear_profile_avatar", { profileId });
}

/** The extensions the picture picker offers. The backend reads the
 *  format from the bytes, so this only filters the dialog. */
export const PROFILE_PICTURE_EXTENSIONS = ["png", "jpg", "jpeg", "webp", "gif"];

/** The i18n key of the sentence for a refused picture, from the
 *  backend's `avatar: <code>` error, or `null` for any other failure. */
export function avatarRefusalKey(err: unknown): string | null {
  const message = err instanceof Error ? err.message : String(err);
  const code = /^avatar: (too-large|unsupported|unreadable)/.exec(message)?.[1];
  switch (code) {
    case "too-large":
      return "profiles.picture.errors.tooLarge";
    case "unsupported":
      return "profiles.picture.errors.unsupported";
    case "unreadable":
      return "profiles.picture.errors.unreadable";
    default:
      return null;
  }
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

/** Read a single value from the active profile's `profile_setting` table.
 *
 *  Pass `expectedProfileId` to pin the read to a specific profile — see
 *  [`setProfileSetting`] for why that matters. */
export function getProfileSetting(
  key: string,
  expectedProfileId?: number | null,
): Promise<string | null> {
  return invoke<string | null>("get_profile_setting", {
    key,
    expectedProfileId: expectedProfileId ?? null,
  });
}

/** Upsert a typed value into the active profile's `profile_setting` table.
 *
 *  `expectedProfileId` is the profile the caller believes is active. The
 *  backend compares it against the live one under the same lock that
 *  `switch_profile` takes, and rejects the write rather than letting it
 *  land in the new profile (issue #485). **Any caller that awaits before
 *  writing must pass it** — a JS-side "is the profile still the one I
 *  captured?" check necessarily runs before the IPC hop, so it cannot
 *  cover the gap on its own. Omit it for a fresh user action that targets
 *  whatever profile is active right now. */
export function setProfileSetting(
  key: string,
  value: string,
  valueType: "bool" | "int" | "string" | "json",
  expectedProfileId?: number | null,
): Promise<void> {
  return invoke<void>("set_profile_setting", {
    key,
    value,
    valueType,
    expectedProfileId: expectedProfileId ?? null,
  });
}
