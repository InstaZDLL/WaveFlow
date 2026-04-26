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
