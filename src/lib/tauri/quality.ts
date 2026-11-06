import { invoke } from "@tauri-apps/api/core";

/**
 * What the audio of a file says about it, measured rather than read from
 * its container — `waveflow_core::quality_verdict_sql!`. In the order the
 * backend lists them: worst news first.
 */
export const QUALITY_VERDICTS = [
  "lossy_source",
  "pending",
  "consistent",
  "lossy",
  "not_measured",
] as const;

export type QualityVerdict = (typeof QUALITY_VERDICTS)[number];

export interface QualitySummary {
  /** Every verdict, zeros included, in `QUALITY_VERDICTS` order. */
  verdicts: { verdict: QualityVerdict; count: number }[];
  /** Lossless files: the only ones the measure concerns. */
  lossless: number;
  /** Of those, the ones measured by the current pass. */
  measured: number;
  /** When the last lossless file was measured, epoch milliseconds. */
  last_analyzed_at: number | null;
}

export interface QualityTrackRow {
  id: number;
  title: string;
  artist_name: string | null;
  album_title: string | null;
  codec: string | null;
  bit_depth: number | null;
  sample_rate: number | null;
  bitrate: number | null;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Where the highs stop, in hertz; `null` with no wall or no measure. */
  cutoff_hz: number | null;
  verdict: QualityVerdict;
}

export interface QualityTracksPage {
  /** How many rows the filter holds in all. */
  total: number;
  items: QualityTrackRow[];
}

/** Counts per verdict over the whole library. */
export function libraryQualitySummary(): Promise<QualitySummary> {
  return invoke<QualitySummary>("library_quality_summary");
}

/**
 * One page of the files with `verdict` in `format` (both optional), worst
 * news first. `format` is a name as the search chips use it (`FLAC`,
 * `ALAC`, `OGG`…).
 */
export function libraryQualityTracks(
  verdict: QualityVerdict | null,
  format: string | null,
  offset: number,
  limit: number,
): Promise<QualityTracksPage> {
  return invoke<QualityTracksPage>("library_quality_tracks", {
    verdict,
    format,
    offset,
    limit,
  });
}

/**
 * Measure every lossless file's highs again, through the library
 * analysis and its task. `null` when an analysis is already running;
 * otherwise how many measured files were queued again.
 */
export function reanalyzeLossless(): Promise<number | null> {
  return invoke<number | null>("reanalyze_lossless");
}
