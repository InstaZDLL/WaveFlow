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
}

/**
 * Sweep the library for tracks lacking an analysis row and process
 * them sequentially. Emits `analysis:progress` along the way.
 */
export function analyzeLibrary(): Promise<LibraryAnalysisSummary> {
  return invoke<LibraryAnalysisSummary>("analyze_library");
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
