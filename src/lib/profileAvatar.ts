import { convertFileSrc } from "@tauri-apps/api/core";
import type { Profile } from "./tauri/profile";

export type AvatarProfile = Pick<
  Profile,
  "name" | "color_id" | "avatar_path" | "avatar_hash"
>;

/** The `asset:` URL of a profile's picture, keyed on its hash: the file
 *  keeps one name, and a new picture must not come from the webview's
 *  cache of the last one. The asset protocol ignores the query. */
export function profileAvatarSrc(
  profile: AvatarProfile | null | undefined,
): string | null {
  if (!profile?.avatar_path || !profile.avatar_hash) return null;
  return `${convertFileSrc(profile.avatar_path)}?v=${encodeURIComponent(profile.avatar_hash)}`;
}
