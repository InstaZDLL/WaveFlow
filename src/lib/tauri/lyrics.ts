import { invoke } from "@tauri-apps/api/core";

export type LyricsFormat = "plain" | "lrc" | "enhanced_lrc";
export type LyricsSource = "embedded" | "lrc_file" | "api" | "manual";

export interface LyricsPayload {
  track_id: number;
  content: string;
  format: LyricsFormat;
  source: LyricsSource;
}

/** Cache-only lookup. Returns null when no row exists yet. */
export function getLyrics(trackId: number): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("get_lyrics", { trackId });
}

/**
 * Three-tier lookup: cache → embedded tag → LRCLIB.
 * Caches the first hit. Returns null if every tier failed.
 */
export function fetchLyrics(trackId: number): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("fetch_lyrics", { trackId });
}

/**
 * Read a `.lrc` (or text) file from disk and store it as the track's
 * lyrics, replacing whatever was cached. Format is detected from the
 * content (LRC if it has `[mm:ss…]` timestamps).
 */
export function importLrcFile(
  trackId: number,
  filePath: string,
): Promise<LyricsPayload> {
  return invoke<LyricsPayload>("import_lrc_file", { trackId, filePath });
}

/** Drop the cached lyrics row so the next fetch re-runs the waterfall. */
export function clearLyrics(trackId: number): Promise<void> {
  return invoke<void>("clear_lyrics", { trackId });
}

export interface SaveLyricsPayload {
  content: string;
  format: "plain" | "lrc";
  /**
   * When true, the backend also writes the lyrics into the audio
   * file's USLT/LYRICS/©lyr frame. Disabled writes are cache-only.
   */
  write_to_file: boolean;
}

/**
 * Persist user-edited lyrics for a track. Always upserts the cache
 * row (source = manual); when `write_to_file` is true the backend
 * also writes the embedded tag and re-hashes the file.
 */
export function saveLyrics(
  trackId: number,
  payload: SaveLyricsPayload,
): Promise<LyricsPayload> {
  return invoke<LyricsPayload>("save_lyrics", { trackId, payload });
}

/**
 * Format a millisecond timestamp as the LRC `[mm:ss.xx]` tag.
 * Centisecond precision matches Musicolet / LRCLIB output.
 */
export function formatLrcTimestamp(timeMs: number): string {
  const safe = Math.max(0, Math.floor(timeMs));
  const minutes = Math.floor(safe / 60_000);
  const seconds = Math.floor((safe % 60_000) / 1000);
  const centis = Math.floor((safe % 1000) / 10);
  const mm = minutes.toString().padStart(2, "0");
  const ss = seconds.toString().padStart(2, "0");
  const cc = centis.toString().padStart(2, "0");
  return `[${mm}:${ss}.${cc}]`;
}

/**
 * Serialize an array of `{ timeMs, text }` rows back into LRC text.
 * Lines without a captured timestamp (timeMs < 0) are emitted with
 * the placeholder `[--:--.--]` so the user can revisit them.
 */
export function serializeLrc(
  rows: Array<{ timeMs: number; text: string }>,
): string {
  return rows
    .map((row) => {
      const stamp = row.timeMs < 0 ? "[--:--.--]" : formatLrcTimestamp(row.timeMs);
      return `${stamp}${row.text}`;
    })
    .join("\n");
}

export interface LyricsPrefetchProgress {
  processed: number;
  total: number;
  hits: number;
  misses: number;
  failed: number;
  current_title: string | null;
}

export interface LyricsPrefetchSummary {
  processed: number;
  hits: number;
  misses: number;
  failed: number;
  cancelled: boolean;
}

/**
 * Walk every uncached track in the active profile and try to populate
 * its lyric (embedded → LRCLIB, throttled at ~2 req/s). Emits
 * `lyrics:prefetch-progress` events the UI can render as a progress
 * bar. Resolves with a summary once the run finishes (or is cancelled).
 */
export function prefetchLibraryLyrics(): Promise<LyricsPrefetchSummary> {
  return invoke<LyricsPrefetchSummary>("prefetch_library_lyrics");
}

/** Flip the prefetch cancel flag. Returns `true` if one was running. */
export function cancelLyricsPrefetch(): Promise<boolean> {
  return invoke<boolean>("cancel_lyrics_prefetch");
}

// ── LRC parser ──────────────────────────────────────────────────────

export interface LrcLine {
  /** Timestamp in milliseconds when this line should be highlighted. */
  timeMs: number;
  /** Plain text of the line (HTML-safe — no markup expected from LRC). */
  text: string;
}

const LRC_TIMESTAMP_RE = /\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]/g;

/**
 * Parse an LRC string into time-stamped lines, sorted by ascending
 * timestamp. Lines without a timestamp (e.g. `[ar:Artist]` metadata
 * tags or stray text) are dropped. A single text line carrying
 * multiple timestamps yields multiple entries.
 *
 * Returns an empty array if no timestamps are found — the caller
 * should fall back to plain rendering in that case.
 */
export function parseLrc(content: string): LrcLine[] {
  const out: LrcLine[] = [];
  for (const raw of content.split(/\r?\n/)) {
    LRC_TIMESTAMP_RE.lastIndex = 0;
    const stamps: number[] = [];
    let match: RegExpExecArray | null;
    while ((match = LRC_TIMESTAMP_RE.exec(raw)) !== null) {
      const minutes = Number(match[1]);
      const seconds = Number(match[2]);
      const fracRaw = match[3] ?? "0";
      // LRC fractional is hundredths of a second; .xxx is rare but
      // valid. Pad to 3 digits then divide to get ms.
      const fracMs = Number(fracRaw.padEnd(3, "0").slice(0, 3));
      stamps.push(minutes * 60_000 + seconds * 1000 + fracMs);
    }
    if (stamps.length === 0) continue;
    const text = raw.replace(LRC_TIMESTAMP_RE, "").trim();
    for (const timeMs of stamps) {
      out.push({ timeMs, text });
    }
  }
  out.sort((a, b) => a.timeMs - b.timeMs);
  return out;
}

/**
 * Find the index of the line that should currently be highlighted
 * given a playback position. Uses a simple linear-scan from the hint
 * (the previous index) — synchronized lyrics rarely jump backwards
 * mid-track so this is O(1) amortized.
 *
 * Returns `-1` when the position is before the first line.
 */
export function findActiveLineIndex(
  lines: LrcLine[],
  positionMs: number,
  hint = 0,
): number {
  if (lines.length === 0 || positionMs < lines[0].timeMs) return -1;
  // Walk forward from the hint until the next line's timestamp is in
  // the future. Cap at lines.length - 1.
  let i = Math.max(0, Math.min(hint, lines.length - 1));
  // Walk backwards if the user seeked.
  while (i > 0 && lines[i].timeMs > positionMs) i--;
  while (i + 1 < lines.length && lines[i + 1].timeMs <= positionMs) i++;
  return i;
}
