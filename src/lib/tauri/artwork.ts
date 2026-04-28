import { convertFileSrc } from "@tauri-apps/api/core";
import { getCachedUrl, setCachedUrl } from "../imageCache";

export type ArtworkSize = "1x" | "2x" | "full";

export interface ArtworkPaths {
  full?: string | null;
  x1?: string | null;
  x2?: string | null;
  remoteUrl?: string | null;
}

/**
 * Resolve the best `asset://` URL (or remote fallback) for the requested
 * size. Walks the `paths` map in a size-specific priority order and
 * returns the first variant that exists, going through the in-memory
 * LRU cache so the same `convertFileSrc` result isn't recomputed on
 * every render.
 *
 * `1x` / `2x` callers gracefully fall back to a larger pre-resized
 * variant when the smaller one hasn't been generated yet (fresh scans
 * before the worker thread has caught up).
 */
export function resolveArtwork(
  paths: ArtworkPaths,
  size: ArtworkSize,
): string | null {
  const orderByRequested: Array<keyof ArtworkPaths> =
    size === "1x"
      ? ["x1", "x2", "full", "remoteUrl"]
      : size === "2x"
        ? ["x2", "x1", "full", "remoteUrl"]
        : ["full", "x2", "x1", "remoteUrl"];
  for (const k of orderByRequested) {
    const v = paths[k];
    if (!v) continue;
    if (k === "remoteUrl") return v;
    const cached = getCachedUrl(v);
    if (cached) return cached;
    const url = convertFileSrc(v);
    setCachedUrl(v, url);
    return url;
  }
  return null;
}

/**
 * Backwards-compatible wrapper for callers that only know about a
 * single local path + remote URL fallback. New code should use
 * `resolveArtwork` directly with explicit size hints.
 */
export function resolveRemoteImage(
  localPath: string | null | undefined,
  remoteUrl: string | null | undefined,
): string | null {
  return resolveArtwork(
    { full: localPath ?? null, remoteUrl: remoteUrl ?? null },
    "full",
  );
}
