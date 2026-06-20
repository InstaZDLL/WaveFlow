import { invoke } from "@tauri-apps/api/core";

/**
 * Cached analysis row for a single track. Mirrors the Rust
 * `TrackAnalysisRow`. `loudness_lufs` is mislabelled by tradition —
 * the value comes from a plain RMS pass without K-weighting, but we
 * keep the column name so the schema stays compatible with a future
 * BS.1770 implementation.
 */
export interface TrackAnalysis {
  track_id: number;
  bpm: number | null;
  musical_key: string | null;
  loudness_lufs: number | null;
  replay_gain_db: number | null;
  peak: number | null;
  analyzed_at: number;
}

export function getTrackAnalysis(
  trackId: number,
): Promise<TrackAnalysis | null> {
  return invoke<TrackAnalysis | null>("get_track_analysis", { trackId });
}

export function analyzeTrack(trackId: number): Promise<TrackAnalysis> {
  return invoke<TrackAnalysis>("analyze_track", { trackId });
}

export interface LibraryAnalysisSummary {
  processed: number;
  failed: number;
  skipped: number;
  /**
   * `true` when the run exited early because the user clicked the
   * "Stop" button (or any other call site triggered
   * `cancelLibraryAnalysis`). UI uses this to render "Cancelled at
   * X / Y" instead of pretending the run completed. Also `true`
   * when a second `analyzeLibrary` call was rejected because one
   * was already in flight.
   */
  cancelled: boolean;
}

/**
 * Sweep the library for tracks lacking an analysis row and process
 * them sequentially. Emits `analysis:progress` along the way.
 *
 * Cooperative + cancellable since 1.5.1 (issue #286): the worker
 * yields and sleeps 25 ms between tracks to keep the CPU available
 * for the UI and OS background services, and honours
 * `cancelLibraryAnalysis` at every iteration. Without these
 * primitives a 4000-track first-time analysis would peg a laptop's
 * CPU for 30+ minutes with no escape.
 */
export function analyzeLibrary(): Promise<LibraryAnalysisSummary> {
  return invoke<LibraryAnalysisSummary>("analyze_library");
}

/**
 * Signal the in-flight library analyzer to stop at the next track
 * boundary. Resolves with `true` when a run was actually in flight
 * (so the UI can show a confirmation toast), `false` when nothing
 * was running — clicking "Stop" twice or before "Start" is a
 * no-op, not an error.
 */
export function cancelLibraryAnalysis(): Promise<boolean> {
  return invoke<boolean>("cancel_library_analysis");
}

/**
 * Read the per-profile auto-analyze flag. When `true`, every scan
 * that adds new tracks fires the analyzer in the background so the
 * library acquires BPM / loudness data without any manual click.
 */
export function getAutoAnalyze(): Promise<boolean> {
  return invoke<boolean>("get_auto_analyze");
}

export function setAutoAnalyze(enable: boolean): Promise<void> {
  return invoke<void>("set_auto_analyze", { enable });
}
