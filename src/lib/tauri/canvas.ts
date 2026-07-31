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
