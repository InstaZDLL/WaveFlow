import { invoke } from "@tauri-apps/api/core";

/** A track's looping Canvas clip (issue #442), resolved to a local absolute
 *  path the webview renders through `convertFileSrc`. Mirrors
 *  `commands::canvas::TrackCanvas` (camelCase). */
export interface TrackCanvas {
  localPath: string;
}

/**
 * Look up a track's Canvas clip, if one was set via
 * `setTrackCanvasFromFile`. Resolves `null` when the track has none —
 * callers render the static cover in that case.
 */
export function getTrackCanvas(trackId: number): Promise<TrackCanvas | null> {
  return invoke<TrackCanvas | null>("get_track_canvas", { trackId });
}

/**
 * Set a track's Canvas from a local mp4 file (issue #442), replacing any
 * previous one. Rejects non-mp4 files (validated by magic bytes) and files
 * above the backend's size cap.
 */
export function setTrackCanvasFromFile(
  trackId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_track_canvas_from_file", { trackId, filePath });
}

/**
 * Clear a track's Canvas, if any — the now-playing view falls back to the
 * static cover. A no-op when there was nothing to clear.
 */
export function clearTrackCanvas(trackId: number): Promise<void> {
  return invoke<void>("clear_track_canvas", { trackId });
}

/** A `canvas`-world plugin's resolved Canvas for a track (issue #473).
 *  Mirrors `commands::canvas::PluginCanvas` (camelCase). Unlike
 *  {@link TrackCanvas}, `url` is a **remote** mp4 the webview `<video>`
 *  loads directly (no `convertFileSrc`), and it sits one rung below the
 *  manual local Canvas in the backdrop precedence. */
export interface PluginCanvas {
  url: string;
  pluginId: string;
}

/**
 * Ask enabled `canvas`-world plugins for a track's Canvas (issue #473).
 * Resolves `null` when offline, when no canvas plugin is installed, or when
 * none has a Canvas for this track. Fail-soft backend-side: a plugin
 * error/timeout is logged and skipped, never thrown.
 *
 * With the local cache on (see {@link setCanvasCacheEnabled}) `url` is an
 * on-disk path instead of a remote URL — `isRemoteCanvasUrl` tells them apart.
 */
export function fetchTrackCanvas(
  artist: string,
  title: string,
  album: string | null,
  durationMs: number | null,
): Promise<PluginCanvas | null> {
  return invoke<PluginCanvas | null>("fetch_track_canvas", {
    artist,
    title,
    album,
    durationMs,
  });
}

/**
 * Opt-in local Canvas cache state (issue #473). When enabled, the backend
 * downloads each resolved plugin Canvas mp4 into an app-wide LRU cache
 * (`<app-data>/waveflow/canvas_cache/`) and serves the local copy — offline
 * replay, no re-stream. Mirrors `commands::canvas::CanvasCacheInfo`. Default
 * OFF; the plugin Canvas streams from the CDN otherwise.
 */
export interface CanvasCacheInfo {
  enabled: boolean;
  sizeBytes: number;
  fileCount: number;
}

/** Read the Canvas-cache toggle + current on-disk footprint. */
export function getCanvasCacheInfo(): Promise<CanvasCacheInfo> {
  return invoke<CanvasCacheInfo>("get_canvas_cache_info");
}

/** Toggle the local Canvas cache. Turning it off does NOT purge existing files. */
export function setCanvasCacheEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_canvas_cache_enabled", { enabled });
}

/** Delete every cached Canvas mp4. */
export function clearCanvasCache(): Promise<void> {
  return invoke<void>("clear_canvas_cache");
}

/**
 * A resolved Canvas source is **remote** (a plugin's `https` URL the webview
 * `<video>` loads directly) when it starts with `http(s)://`; otherwise it's
 * a **local** path that must go through `convertFileSrc`. Shared so the
 * renderer (`CanvasStage`) and the manual-only picker check (`ImmersiveView`)
 * never drift on the local-vs-remote split.
 */
export function isRemoteCanvasUrl(source: string): boolean {
  return /^https?:\/\//i.test(source);
}
