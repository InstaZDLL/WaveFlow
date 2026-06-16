import { invoke } from "@tauri-apps/api/core";

export type LyricsFormat = "plain" | "lrc" | "enhanced_lrc" | "ttml";
export type LyricsSource = "embedded" | "lrc_file" | "api" | "manual";

export interface LyricsPayload {
  track_id: number;
  content: string;
  format: LyricsFormat;
  source: LyricsSource;
  /**
   * Set by `save_lyrics` when the user asked for `write_to_file` but
   * the audio container can't carry the chosen format (currently TTML
   * on MP3/ID3v2). The DB cache is still updated; the UI surfaces a
   * toast so the user knows the file itself wasn't touched. Absent on
   * every other return path.
   */
  tag_write_skipped?: boolean;
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

/**
 * Write a serialized lyrics payload (already trimmed + format-encoded)
 * to an arbitrary on-disk path. Used by the Lyrics Editor "Save to
 * file…" affordance so the user can ship the LRC/TXT as a sidecar
 * next to the song file (the in-tag + cache-only options stay
 * available via the existing `saveLyrics` flow). The caller is
 * expected to resolve `targetPath` via the Tauri save dialog
 * (`@tauri-apps/plugin-dialog`'s `save()`); the backend re-validates
 * the parent directory exists before writing.
 */
export function exportLyricsToPath(
  targetPath: string,
  content: string,
): Promise<void> {
  return invoke<void>("export_lyrics_to_path", {
    targetPath,
    content,
  });
}

export interface SaveLyricsPayload {
  content: string;
  format: "plain" | "lrc" | "enhanced_lrc" | "ttml";
  /**
   * When true, the backend also writes the lyrics into the audio
   * file's USLT/LYRICS/©lyr frame. Disabled writes are cache-only.
   * TTML on MP3 is silently skipped (see `tag_write_skipped` on the
   * returned payload).
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
 * Format a millisecond timestamp as `<open>mm:ss.xx<close>`.
 * Centisecond precision matches Musicolet / LRCLIB output. Used with
 * `[` / `]` for LRC line stamps and `<` / `>` for Enhanced LRC inline
 * word stamps — picking the delimiters up-front avoids string-replace
 * round-trips on a known-good output.
 */
function formatTimestamp(timeMs: number, open: string, close: string): string {
  const safe = Math.max(0, Math.floor(timeMs));
  const minutes = Math.floor(safe / 60_000);
  const seconds = Math.floor((safe % 60_000) / 1000);
  const centis = Math.floor((safe % 1000) / 10);
  const mm = minutes.toString().padStart(2, "0");
  const ss = seconds.toString().padStart(2, "0");
  const cc = centis.toString().padStart(2, "0");
  return `${open}${mm}:${ss}.${cc}${close}`;
}

/**
 * Format a millisecond timestamp as the LRC `[mm:ss.xx]` tag.
 * Centisecond precision matches Musicolet / LRCLIB output.
 */
export function formatLrcTimestamp(timeMs: number): string {
  return formatTimestamp(timeMs, "[", "]");
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
      const stamp =
        row.timeMs < 0 ? "[--:--.--]" : formatLrcTimestamp(row.timeMs);
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

// ── Lyrics parsers (LRC + Enhanced LRC + TTML) ──────────────────────

/**
 * One karaoke word within a synchronized line. `endMs` is the
 * timestamp at which the next word becomes active — for the last word
 * of a line it falls back to the next line's `timeMs` (or +∞ on the
 * very last line).
 */
export interface LyricsWord {
  timeMs: number;
  endMs: number;
  text: string;
}

/**
 * Unified line type returned by every parser. Components that don't
 * care about word-level timing can ignore `words`; the karaoke view
 * uses it when present to drive the per-word highlight animation.
 *
 * Kept structurally compatible with the legacy `LrcLine` shape so
 * existing call sites (`findActiveLineIndex`, panel scroll, etc.)
 * keep working without per-call casts.
 */
export interface LyricsLine {
  timeMs: number;
  /** End of this line in ms. -1 if unknown (e.g. last line). */
  endMs: number;
  /** Plain text — for word-timed lines, this is the joined word text. */
  text: string;
  /** Per-word timestamps when the source format provides them. */
  words?: LyricsWord[];
}

/** Backwards-compatible alias used across the panel + fullscreen views. */
export type LrcLine = LyricsLine;

const LRC_LINE_STAMP_RE = /\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]/g;
const LRC_WORD_STAMP_RE = /<(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?>/g;

/** `mm:ss(.xx|:xx)?` → ms. Pads fractions to 3 digits then truncates. */
function lrcStampToMs(
  minutes: string,
  seconds: string,
  fraction?: string,
): number {
  const m = Number(minutes);
  const s = Number(seconds);
  const fracMs = Number((fraction ?? "0").padEnd(3, "0").slice(0, 3));
  return m * 60_000 + s * 1000 + fracMs;
}

/**
 * Parse a plain LRC string (line-level timestamps only) into a sorted
 * list of `LyricsLine`. Lines without a timestamp are dropped; a
 * single text line carrying multiple timestamps yields multiple
 * entries. Returns an empty array when no stamps are found — the
 * caller should fall back to plain rendering.
 */
export function parseLrc(content: string): LyricsLine[] {
  const out: LyricsLine[] = [];
  for (const raw of content.split(/\r?\n/)) {
    LRC_LINE_STAMP_RE.lastIndex = 0;
    const stamps: number[] = [];
    let match: RegExpExecArray | null;
    while ((match = LRC_LINE_STAMP_RE.exec(raw)) !== null) {
      stamps.push(lrcStampToMs(match[1], match[2], match[3]));
    }
    if (stamps.length === 0) continue;
    const text = raw.replace(LRC_LINE_STAMP_RE, "").trim();
    for (const timeMs of stamps) {
      out.push({ timeMs, endMs: -1, text });
    }
  }
  out.sort((a, b) => a.timeMs - b.timeMs);
  return fillEndTimestamps(out);
}

/**
 * Parse Enhanced LRC (`[mm:ss.xx]La <mm:ss.xx>nuit <mm:ss.xx>tombe`).
 * Each line keeps its line-level `timeMs` and gets a `words[]` array
 * with one entry per `<mm:ss.xx>word` token. A line with stamps but
 * no inline word stamps gracefully degrades to the plain LRC layout
 * (no `words` field).
 */
export function parseEnhancedLrc(content: string): LyricsLine[] {
  const lines: LyricsLine[] = [];
  for (const raw of content.split(/\r?\n/)) {
    LRC_LINE_STAMP_RE.lastIndex = 0;
    const lineStamps: number[] = [];
    let m: RegExpExecArray | null;
    while ((m = LRC_LINE_STAMP_RE.exec(raw)) !== null) {
      lineStamps.push(lrcStampToMs(m[1], m[2], m[3]));
    }
    if (lineStamps.length === 0) continue;
    const body = raw.replace(LRC_LINE_STAMP_RE, "");

    LRC_WORD_STAMP_RE.lastIndex = 0;
    const wordStamps: Array<{ at: number; timeMs: number }> = [];
    let wm: RegExpExecArray | null;
    while ((wm = LRC_WORD_STAMP_RE.exec(body)) !== null) {
      wordStamps.push({
        at: wm.index,
        timeMs: lrcStampToMs(wm[1], wm[2], wm[3]),
      });
    }

    if (wordStamps.length === 0) {
      // Plain LRC line — keep the text as-is.
      const text = body.trim();
      for (const timeMs of lineStamps) {
        lines.push({ timeMs, endMs: -1, text });
      }
      continue;
    }

    // Slice the body between consecutive word stamps to recover the
    // word text. The slice from `wordStamps[i]` end to the next stamp
    // start is the displayed word.
    const built: LyricsWord[] = [];

    // Any text before the first inline word stamp is sung at the
    // line's own timestamp — common when a tool emits
    // `[mm:ss]First <mm:ss>second`. Treat it as a virtual leading
    // word; its timeMs is rewritten per-duplicate-line in the loop
    // below so that `[00:01][00:30]Hello <00:31>world` doesn't make
    // every clone inherit the same first-word time.
    const prefix = body.slice(0, wordStamps[0].at);
    const hasPrefix = prefix.length > 0 && prefix.trim().length > 0;
    if (hasPrefix) {
      built.push({
        timeMs: -1, // placeholder, set per-line below
        endMs: wordStamps[0].timeMs,
        text: prefix,
      });
    }

    for (let i = 0; i < wordStamps.length; i += 1) {
      const start =
        wordStamps[i].at + matchedStampLength(body, wordStamps[i].at);
      const end =
        i + 1 < wordStamps.length ? wordStamps[i + 1].at : body.length;
      built.push({
        timeMs: wordStamps[i].timeMs,
        endMs: i + 1 < wordStamps.length ? wordStamps[i + 1].timeMs : -1,
        text: body.slice(start, end),
      });
    }

    // Drop trailing empty segments without timing (artefact of a
    // trailing space after the last stamp).
    const words = built.filter((w) => w.text.length > 0 || w.timeMs >= 0);
    const text = words
      .map((w) => w.text)
      .join("")
      .trim();

    // Deep-clone the words array per line entry so `fillEndTimestamps`
    // can mutate each independently. For prefix-bearing lines, the
    // virtual first word inherits the current line stamp instead of a
    // shared placeholder.
    for (const timeMs of lineStamps) {
      const clonedWords = words.map((w, idx) => ({
        ...w,
        timeMs: hasPrefix && idx === 0 ? timeMs : w.timeMs,
      }));
      lines.push({ timeMs, endMs: -1, text, words: clonedWords });
    }
  }
  lines.sort((a, b) => a.timeMs - b.timeMs);
  return fillEndTimestamps(lines);
}

/** Length of the `<mm:ss(.xx)?>` token starting at `at` in `body`. */
function matchedStampLength(body: string, at: number): number {
  const close = body.indexOf(">", at);
  return close < 0 ? 0 : close - at + 1;
}

/**
 * Parse Apple-Music-style TTML. Walks `<p>` for lines and `<span>` for
 * words. `begin`/`end` accept `HH:MM:SS.mmm`, `MM:SS.mmm`, plain
 * seconds (`12.5s`), or a bare number of seconds.
 *
 * Char-level spans (TTML lets `<span>` nest inside `<span>`) are
 * collapsed into the outer word — we don't animate character-by-char
 * in v1.
 *
 * Returns an empty array if the document has no parseable lines.
 */
export function parseTtml(content: string): LyricsLine[] {
  if (typeof window === "undefined" || typeof DOMParser === "undefined") {
    return [];
  }
  const doc = new DOMParser().parseFromString(content, "application/xml");
  if (doc.querySelector("parsererror")) return [];

  const out: LyricsLine[] = [];
  const paragraphs = doc.getElementsByTagName("p");
  for (let i = 0; i < paragraphs.length; i += 1) {
    const p = paragraphs[i];
    const lineBegin = parseTtmlTime(p.getAttribute("begin"));
    if (lineBegin < 0) continue;
    const lineEnd = parseTtmlTime(p.getAttribute("end"));

    // Direct child <span>s are the words. Nested spans are folded into
    // their parent's text so char-level timing collapses cleanly.
    const wordEls: Element[] = [];
    for (const child of Array.from(p.children)) {
      if (child.tagName.toLowerCase() === "span") wordEls.push(child);
    }

    let words: LyricsWord[] | undefined;
    let text: string;
    if (wordEls.length > 0) {
      words = [];
      for (let w = 0; w < wordEls.length; w += 1) {
        const el = wordEls[w];
        const wBegin = parseTtmlTime(el.getAttribute("begin"));
        const wEnd = parseTtmlTime(el.getAttribute("end"));
        if (wBegin < 0) continue;
        // Re-attach the trailing whitespace that TTML strips so words
        // render with their natural spacing.
        const raw = (el.textContent ?? "").replace(/\s+/g, " ");
        const trailing = el.nextSibling?.nodeType === Node.TEXT_NODE ? " " : "";
        words.push({
          timeMs: wBegin,
          endMs: wEnd >= 0 ? wEnd : -1,
          text: raw + trailing,
        });
      }
      if (words.length === 0) words = undefined;
      text = (words ?? [])
        .map((w) => w.text)
        .join("")
        .trim();
    } else {
      text = (p.textContent ?? "").replace(/\s+/g, " ").trim();
    }

    if (!text && (!words || words.length === 0)) continue;
    out.push({
      timeMs: lineBegin,
      endMs: lineEnd >= 0 ? lineEnd : -1,
      text,
      words,
    });
  }

  out.sort((a, b) => a.timeMs - b.timeMs);
  return fillEndTimestamps(out);
}

/**
 * Parse a TTML `begin`/`end` clock value into milliseconds. Accepts:
 *   - `HH:MM:SS.mmm`
 *   - `MM:SS.mmm` / `MM:SS`
 *   - `123.5s` (seconds, decimal allowed)
 *   - `1500ms`
 *   - bare seconds (`"5"` → 5000 ms)
 * Returns -1 for null / empty / unparseable input.
 */
function parseTtmlTime(value: string | null): number {
  if (value == null) return -1;
  const s = value.trim();
  if (!s) return -1;

  if (s.endsWith("ms")) {
    const n = Number(s.slice(0, -2));
    return Number.isFinite(n) ? Math.round(n) : -1;
  }
  if (s.endsWith("s")) {
    const n = Number(s.slice(0, -1));
    return Number.isFinite(n) ? Math.round(n * 1000) : -1;
  }

  if (s.includes(":")) {
    const parts = s.split(":");
    if (parts.length === 2) {
      const [mm, ss] = parts;
      const m = Number(mm);
      const sec = Number(ss);
      if (Number.isFinite(m) && Number.isFinite(sec)) {
        return Math.round(m * 60_000 + sec * 1000);
      }
      return -1;
    }
    if (parts.length === 3) {
      const [hh, mm, ss] = parts;
      const h = Number(hh);
      const m = Number(mm);
      const sec = Number(ss);
      if (Number.isFinite(h) && Number.isFinite(m) && Number.isFinite(sec)) {
        return Math.round(h * 3_600_000 + m * 60_000 + sec * 1000);
      }
      return -1;
    }
    return -1;
  }

  const n = Number(s);
  return Number.isFinite(n) ? Math.round(n * 1000) : -1;
}

/**
 * Fill each line's `endMs` with the next line's `timeMs` (and the last
 * word of each line gets the line's `endMs`). Pure helper used by every
 * parser so the karaoke view can interpolate without special-casing the
 * last entry.
 */
function fillEndTimestamps(lines: LyricsLine[]): LyricsLine[] {
  for (let i = 0; i < lines.length; i += 1) {
    if (lines[i].endMs < 0) {
      lines[i].endMs = i + 1 < lines.length ? lines[i + 1].timeMs : -1;
    }
    const words = lines[i].words;
    if (words && words.length > 0) {
      for (let w = 0; w < words.length; w += 1) {
        if (words[w].endMs < 0) {
          words[w].endMs =
            w + 1 < words.length ? words[w + 1].timeMs : lines[i].endMs;
        }
      }
    }
  }
  return lines;
}

/**
 * Dispatcher consumed by every UI component. Picks the right parser
 * for `format`. Plain text returns a single line at t=0 with no
 * `words`. Unknown / empty content returns an empty array.
 */
export function parseLyrics(
  content: string,
  format: LyricsFormat,
): LyricsLine[] {
  if (!content.trim()) return [];
  switch (format) {
    case "lrc":
      return parseLrc(content);
    case "enhanced_lrc":
      return parseEnhancedLrc(content);
    case "ttml":
      return parseTtml(content);
    case "plain":
    default:
      return [];
  }
}

/**
 * Serialize a list of word-stamped lines back to Enhanced LRC text.
 * Lines without `words` fall back to a plain `[mm:ss.xx]` entry.
 * Used by the editor when the user saves a word-timed track — TTML
 * round-trip isn't part of v1, so we always export to Enhanced LRC.
 *
 * Words with `timeMs < 0` (not yet captured) are emitted **without**
 * an inline stamp — their text is folded into the previous word so a
 * half-finished line doesn't ship phantom `<00:00.00>word` stamps
 * that would mis-sync on the next load. The user can re-open the
 * editor and finish stamping later.
 */
export function serializeEnhancedLrc(lines: LyricsLine[]): string {
  return lines
    .map((line) => {
      const stamp =
        line.timeMs < 0 ? "[--:--.--]" : formatLrcTimestamp(line.timeMs);
      if (!line.words || line.words.length === 0) {
        return `${stamp}${line.text}`;
      }
      const parts: string[] = [];
      for (const w of line.words) {
        if (w.timeMs >= 0) {
          parts.push(`${formatTimestamp(w.timeMs, "<", ">")}${w.text}`);
        } else {
          // Uncaptured — append the text to the previous segment so
          // it survives the round-trip without acquiring a fake
          // zero-second stamp.
          if (parts.length > 0) {
            parts[parts.length - 1] += w.text;
          } else {
            parts.push(w.text);
          }
        }
      }
      return `${stamp}${parts.join("")}`;
    })
    .join("\n");
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
  lines: LyricsLine[],
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

/**
 * Find the index of the active word within a line's `words` array
 * given a playback position. Returns `-1` when the position is before
 * the first word (so the line is highlighted but no word yet).
 */
export function findActiveWordIndex(
  words: LyricsWord[],
  positionMs: number,
  hint = 0,
): number {
  if (words.length === 0 || positionMs < words[0].timeMs) return -1;
  let i = Math.max(0, Math.min(hint, words.length - 1));
  while (i > 0 && words[i].timeMs > positionMs) i--;
  while (i + 1 < words.length && words[i + 1].timeMs <= positionMs) i++;
  return i;
}
