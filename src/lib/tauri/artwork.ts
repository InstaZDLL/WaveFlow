import { convertFileSrc } from "@tauri-apps/api/core";

/**
 * Resolve a remote image source for components that previously consumed a raw
 * Deezer CDN URL. Prefers the locally-cached file (so the app keeps rendering
 * artist/album imagery offline) and falls back to the remote URL when the
 * local download has not happened yet.
 *
 * Returns `null` when neither a local path nor a remote URL is available so
 * the caller can render its placeholder branch unchanged.
 */
export function resolveRemoteImage(
  localPath: string | null | undefined,
  remoteUrl: string | null | undefined,
): string | null {
  if (localPath) return convertFileSrc(localPath);
  if (remoteUrl) return remoteUrl;
  return null;
}
