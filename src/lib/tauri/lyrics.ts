import { invoke } from "@tauri-apps/api/core";

export type LyricsFormat = "plain" | "lrc" | "enhanced_lrc" | "ttml";
export type LyricsSource = "embedded" | "lrc_file" | "api" | "manual";

/**
 * Sub-provider identifier returned by the backend on API-sourced rows.
 * Matches `waveflow_syncedlyrics::Provider::as_str()`. Pre-1.5.1 cached
 * rows AND non-API tiers (embedded / sidecar / manual) leave the field
 * `null` — the UI then renders the broad `source` label without a
 * provider chip.
 */
export type LyricsProvider =
  "lrclib" | "genius" | "net_ease" | "megalobiz" | "musixmatch";

/**
 * Stable list of providers the picker UI cycles through. Mirrors
 * `Provider::defaults()` order on the backend — Musixmatch first
 * because it's the only word-timed source, then LRCLIB, then the
 * query-based fallbacks. `refetchLyrics` accepts any of these
 * strings as the override; an `undefined` override re-runs the full
 * waterfall.
 */
export const LYRICS_PROVIDERS: LyricsProvider[] = [
  "musixmatch",
  "lrclib",
  "net_ease",
  "megalobiz",
  "genius",
];

/**
 * A `lyrics.provider` value naming a plugin rather than one of the
 * built-in network providers — `"plugin:"` followed by the plugin id.
 *
 * The two namespaces share the column, so the prefix is what keeps them
 * apart. Anything matching it is NOT in `LYRICS_PROVIDERS` and must not
 * be looked up there; `refetch_lyrics` accepts it and re-runs that
 * plugin.
 */
export type PluginLyricsProvider = `plugin:${string}`;

/**
 * The prefix that distinguishes a plugin id from a built-in provider id
 * in `LyricsPayload.provider`. Kept beside the type so the two cannot
 * drift, and exported because the source badge has to recognise it.
 */
export const PLUGIN_PROVIDER_PREFIX = "plugin:";

/** What an extra document is, relative to the primary lyrics. */
export type AssociatedLyricsKind = "translation" | "pronunciation";

/**
 * A translation or pronunciation that came with the lyrics, as the
 * provider served it (issue #585).
 *
 * `content` is a document in its own right — parse it with the same
 * `parseLrc` / TTML path as the primary one rather than assuming it
 * lines up positionally: a localized document may omit lines the
 * original has, so pairing them by index is wrong.
 */
export interface AssociatedLyrics {
  kind: AssociatedLyricsKind;
  /** BCP-47 tag when the provider named one. */
  language?: string;
  content: string;
  format: LyricsFormat;
}

export interface LyricsPayload {
  track_id: number;
  content: string;
  format: LyricsFormat;
  source: LyricsSource;
  /**
   * Sub-provider that produced this row when `source === "api"`. One
   * of `LYRICS_PROVIDERS`, or `null` / absent for embedded / sidecar
   * / manual / pre-1.5.1 rows. The lyrics panel reads this to render
   * an accurate badge (e.g. "Genius" instead of the hardcoded
   * "LRCLIB" the panel previously always showed) and to drive the
   * default selection of the provider picker.
   */
  provider?: LyricsProvider | PluginLyricsProvider | null;
  /**
   * Set by `save_lyrics` when the user picked destination = `"tag"`
   * but the audio container can't carry the chosen format (currently
   * TTML on MP3/ID3v2). The DB cache is still updated; the UI
   * surfaces a toast so the user knows the file itself wasn't
   * touched. Absent on every other return path.
   */
  tag_write_skipped?: boolean;
  /**
   * Translations and pronunciations cached with these lyrics. Absent
   * or empty for every source that yields a single document, which is
   * all of them except a `waveflow:metadata/v2` plugin.
   *
   * Cached and replaced as a unit with `content`, so these always come
   * from the same fetch — never a leftover companion from whichever
   * provider answered last time.
   */
  associated?: AssociatedLyrics[];
  /**
   * Set by `save_lyrics` when the user picked destination = `"sidecar"`
   * but the chosen format can't ride a `.lrc` / `.txt` companion
   * (TTML — neither extension carries the XML markup the reader
   * expects). DB cache is still updated. Absent on every other return
   * path.
   */
  sidecar_write_skipped?: boolean;
}

/** Cache-only lookup. Returns null when no row exists yet. */
export function getLyrics(trackId: number): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("get_lyrics", { trackId }).then(
    showableLyrics,
  );
}

/**
 * Three-tier lookup: cache → embedded tag → LRCLIB.
 * Caches the first hit. Returns null if every tier failed.
 */
export function fetchLyrics(trackId: number): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("fetch_lyrics", { trackId }).then(
    showableLyrics,
  );
}

/**
 * Fetch + cache lyrics for a now-playing Web Radio song.
 *
 * Radio has no library row (negative sentinel id, live stream), so the
 * regular `fetchLyrics` waterfall can't help — this keys a dedicated
 * `radio_lyrics` cache by the (artist, title) parsed from the ICY title
 * and queries the external providers. `trackId` is the radio sentinel id
 * (echoed into the payload, never a cache key). Returns null when the
 * song has no lyrics, when offline with nothing cached, or on a
 * transient provider error. The lyrics may be synced (LRC) but the panel
 * renders them statically — the live stream position can't align to a
 * song the listener joined mid-play.
 */
export function fetchRadioLyrics(
  artist: string,
  title: string,
  trackId: number,
): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("fetch_radio_lyrics", {
    artist,
    title,
    trackId,
  }).then(showableLyrics);
}

/**
 * Fetch lyrics for a now-playing remote-source track (RFC-005).
 *
 * The server (`GET /api/v2/tracks/{id}/lyrics`) is the priority source;
 * on a miss the backend falls back to LRCLIB + the query chain by
 * (artist, title, duration). Unlike radio, a remote track has a stable
 * identity and known length, so synced lyrics align and the panel renders
 * the karaoke highlight. `trackId` is the negative sentinel echoed into
 * the payload; `remoteTrackId` is the server UUID the lyrics are keyed by.
 * Returns null when neither source has lyrics. `sync_v2` builds only.
 */
export function fetchRemoteLyrics(
  remoteTrackId: string,
  artist: string,
  title: string,
  durationMs: number,
  trackId: number,
): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("fetch_remote_lyrics", {
    remoteTrackId,
    artist,
    title,
    durationMs,
    trackId,
  }).then(showableLyrics);
}

/**
 * Force a re-fetch for a single track, dropping the cached row first
 * so the waterfall (or single-provider query below) is guaranteed to
 * re-query.
 *
 * - `provider = undefined` → re-runs the full waterfall identical to
 *   the legacy "Clear + Refetch" gesture.
 * - `provider = "<id>"` → bypasses local tiers (embedded / sidecar)
 *   and queries ONLY the named provider. A miss caches an empty row
 *   attributed to the requested provider so the badge reflects what
 *   the user just tried — picking a different provider and trying
 *   again is then the natural next step (issue #284).
 */
export function refetchLyrics(
  trackId: number,
  provider?: LyricsProvider,
): Promise<LyricsPayload | null> {
  return invoke<LyricsPayload | null>("refetch_lyrics", {
    trackId,
    provider: provider ?? null,
  }).then(showableLyrics);
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
  return invoke<LyricsPayload>("import_lrc_file", { trackId, filePath }).then(
    showableLyrics,
  );
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

/**
 * Read the per-profile Musixmatch translation target language.
 * `null` when the user hasn't opted in — the lyrics fetch waterfall
 * then runs without translation enrichment.
 */
export function getLyricsTranslationLang(): Promise<string | null> {
  return invoke<string | null>("get_lyrics_translation_lang");
}

/**
 * Persist the per-profile Musixmatch translation target language.
 * Pass `null` to clear (default: no translation). The backend
 * whitelists the value against the WaveFlow UI locale set; passing
 * anything else rejects with an error so a typo can't silently
 * disable the feature.
 */
export function setLyricsTranslationLang(
  lang: string | null,
): Promise<string | null> {
  return invoke<string | null>("set_lyrics_translation_lang", { lang });
}

/**
 * Read the per-profile "prefer LRCLIB" flag. `true` makes the on-demand
 * lyrics waterfall try the online providers (LRCLIB / Musixmatch / the
 * fallback chain) before the track's own embedded + sidecar lyrics
 * (issue #378). Default `false` — local lyrics win.
 */
export function getPreferLrclib(): Promise<boolean> {
  return invoke<boolean>("get_prefer_lrclib");
}

/**
 * Persist the per-profile "prefer LRCLIB" flag. `false` clears the row
 * and restores the local-first default.
 */
export function setPreferLrclib(enabled: boolean): Promise<void> {
  return invoke<void>("set_prefer_lrclib", { enabled });
}

/**
 * Where the user wants the editor's output to land. The frontend
 * pre-fills the segmented control with the app-wide default
 * (`getLyricsDefaultDestination`) and lets the user override
 * per-save. Backend mirrors the enum in `LyricsDestination`.
 */
export type LyricsDestination = "tag" | "sidecar" | "db_only";

export interface SaveLyricsPayload {
  content: string;
  format: "plain" | "lrc" | "enhanced_lrc" | "ttml";
  /**
   * `tag` embeds in USLT/LYRICS/©lyr (current default, TTML silently
   * skipped on MP3 — see `tag_write_skipped`). `sidecar` writes a
   * sibling `.lrc` / `.txt` next to the audio file (issue #201).
   * `db_only` updates the WaveFlow cache and leaves the filesystem
   * untouched.
   */
  destination: LyricsDestination;
}

/**
 * Persist user-edited lyrics for a track. Always upserts the cache
 * row (source = manual); the on-disk side-effect depends on
 * `destination`.
 */
export function saveLyrics(
  trackId: number,
  payload: SaveLyricsPayload,
): Promise<LyricsPayload> {
  return invoke<LyricsPayload>("save_lyrics", { trackId, payload }).then(
    showableLyrics,
  );
}

/**
 * Read the app-wide default destination the editor pre-fills with.
 * Missing setting → `"tag"` so pre-#201 behaviour is preserved.
 */
export function getLyricsDefaultDestination(): Promise<LyricsDestination> {
  return invoke<LyricsDestination>("get_lyrics_default_destination");
}

/**
 * Persist the app-wide default destination. Backend rejects unknown
 * values; the caller should pass one of the three enum variants
 * literally.
 */
export function setLyricsDefaultDestination(
  destination: LyricsDestination,
): Promise<LyricsDestination> {
  return invoke<LyricsDestination>("set_lyrics_default_destination", {
    destination,
  });
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
/**
 * A second reading of a line — today a romanization — which may or may
 * not be broken into timed words.
 *
 * `text` is always usable; `words` is the bonus. When present it is
 * measured to match the line word for word, sharing each word's
 * `timeMs` / `endMs` exactly: across a full Apple document there is one
 * transliterated span per original span with identical bounds. That is
 * why the karaoke fill needs no second timing pass — a romanized word
 * is driven by the clock of the word it reads out. Nothing upstream
 * promises that, though, so a line whose counts disagree keeps its
 * text and loses only the word split.
 */
export interface LyricsReading {
  text: string;
  words?: LyricsWord[];
}

export interface LyricsLine {
  timeMs: number;
  /** End of this line in ms. -1 if unknown (e.g. last line). */
  endMs: number;
  /** Plain text — for word-timed lines, this is the joined word text. */
  text: string;
  /** Per-word timestamps when the source format provides them. */
  words?: LyricsWord[];
  /**
   * Latin-script transliteration of this line, when the document
   * carries one (Apple TTML `<transliterations>`).
   *
   * `words` is present only when the document is word-timed, which is
   * a property of the document and not of the transliteration: Apple
   * serves both a line-timed and a syllable-timed lyric, and localizes
   * whichever was asked for. A line-timed one romanizes the line and
   * stops there, so the text always reads even when nothing can be
   * highlighted inside it.
   */
  romanization?: LyricsReading;
  /**
   * Line-level translation, when the document carries one. Apple gives
   * these as whole lines with no word split — which is also how they
   * read on screen, one block under the line.
   */
  translation?: string;
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
 * An element's name without its namespace prefix.
 *
 * `localName` is the right question to ask of an XML document, and
 * `getElementsByTagName` is the wrong one: it matches the QUALIFIED
 * name, so it finds `<translation>` and misses `<itunes:translation>`
 * even though they are the same element. Lowercased as a courtesy to
 * the HTML-ish documents users import by hand; XML itself is
 * case-sensitive, and TTML spells these lowercase.
 */
function localNameOf(el: Element): string {
  return (el.localName || el.tagName).toLowerCase();
}

/** Every descendant with this local name, prefix or not. */
function byLocalName(root: Element | Document, local: string): Element[] {
  return Array.from(root.getElementsByTagName("*")).filter(
    (el) => localNameOf(el) === local,
  );
}

/** Apple's private TTML namespace, where line keys and timing live. */
const ITUNES_NS = "http://music.apple.com/lyric-ttml-internal";

/**
 * The direct `<span>` children of `el` as karaoke words, or `undefined`
 * when it has none.
 *
 * Nested spans are folded into their parent's text — TTML allows
 * `<span>` inside `<span>` for char-level timing, which we don't
 * animate. Shared by the lines themselves and by their transliterated
 * twins, which Apple gives the same shape.
 */
function ttmlWords(el: Element): LyricsWord[] | undefined {
  const words: LyricsWord[] = [];
  for (const child of Array.from(el.children)) {
    if (localNameOf(child) !== "span") continue;
    const begin = parseTtmlTime(child.getAttribute("begin"));
    if (begin < 0) continue;
    const end = parseTtmlTime(child.getAttribute("end"));
    // Re-attach what sits between this span and the next, so words
    // render with their natural spacing. Normalised rather than
    // replaced by a fixed space: the usual case is XML indentation,
    // which collapses to exactly that anyway, but a document that puts
    // punctuation between the spans instead of inside them would
    // otherwise lose it.
    const raw = (child.textContent ?? "").replace(/\s+/g, " ");
    const next = child.nextSibling;
    const trailing =
      next?.nodeType === Node.TEXT_NODE
        ? (next.textContent ?? "").replace(/\s+/g, " ")
        : "";
    words.push({
      timeMs: begin,
      endMs: end >= 0 ? end : -1,
      text: raw + trailing,
    });
  }
  return words.length > 0 ? words : undefined;
}

/**
 * Whether two readings of a line say the same thing.
 *
 * Apple localizes every line of a song, including the ones already in
 * the target script or language: in a Korean song, an English line
 * comes back transliterated as itself and translated to English as
 * itself. Shown, that is a line printed twice — and worse than a plain
 * duplicate when the syllable split differs from the word split, since
 * the eye reads a discrepancy where there is none. Apple Music hides
 * these, and so do we.
 *
 * Spacing is ignored precisely because the split is where the two
 * disagree; case is ignored because neither a transliterator nor a
 * translator has reason to preserve it.
 */
function saysTheSameThing(one: string, other: string): boolean {
  const strip = (value: string) => value.replace(/\s+/g, "").toLowerCase();
  return strip(one) === strip(other);
}

/**
 * The romanization to actually attach to a line, or `undefined`.
 *
 * Two things can be wrong with one, and they cost different amounts.
 *
 * It may say nothing the line does not already say — Apple localizes
 * every line, including those already in Latin script, so those come
 * back as themselves. Printed under the line that is a duplicate, and a
 * near-duplicate whenever the syllable split differs from the word
 * split, where the eye reads a discrepancy that is not there. Nothing
 * is lost by dropping it entirely.
 *
 * Or its word split may not match the line's. Measured on a full Apple
 * document it always matches — same count, same bounds — but nothing
 * upstream promises it, and pairing words up as far as they go would
 * put the highlight on the wrong one, which reads as a broken
 * transliteration rather than a missing feature. That costs the split
 * only: the text still reads correctly under the line, so it is kept
 * unsplit rather than thrown away.
 */
function usableRomanization(
  romanized: LyricsReading | undefined,
  words: LyricsWord[] | undefined,
  text: string,
): LyricsReading | undefined {
  if (!romanized || saysTheSameThing(romanized.text, text)) return undefined;
  if (!romanized.words) return romanized;
  if (words && romanized.words.length === words.length) return romanized;
  return { text: romanized.text };
}

interface ParsedLocalizations {
  translationByKey: Map<string, string>;
  romanizationByKey: Map<string, LyricsReading>;
}

/**
 * Read the `<translations>` and `<transliterations>` Apple hides in
 * `<head>`, keyed by the line key each entry points at.
 *
 * Both are optional, and a document that asked for neither still
 * carries an empty `<translations>` container — so absence is the
 * normal case, not an error.
 *
 * Only the FIRST of each is read, and within it the first entry per
 * line key. Apple returns one container per request because the
 * language is chosen in the call; a document carrying several would
 * need the user to pick one, which is a question this parser has no way
 * to ask. The per-key half of the rule matters less but has to agree
 * with it: `Map.set` alone would keep the LAST entry for a duplicated
 * key, which is the opposite policy in the same function.
 */
function readTtmlLocalizations(doc: Document): ParsedLocalizations {
  const translationByKey = new Map<string, string>();
  const romanizationByKey = new Map<string, LyricsReading>();

  const translation = byLocalName(doc, "translation")[0];
  if (translation) {
    for (const entry of byLocalName(translation, "text")) {
      const key = entry.getAttribute("for");
      const text = (entry.textContent ?? "").replace(/\s+/g, " ").trim();
      // First entry wins, matching the "first container wins" rule
      // above. Nothing should point two entries at one line, but
      // `Map.set` would silently keep the last, which is the opposite
      // policy in the same function.
      if (key && text && !translationByKey.has(key)) {
        translationByKey.set(key, text);
      }
    }
  }

  const transliteration = byLocalName(doc, "transliteration")[0];
  if (transliteration) {
    for (const entry of byLocalName(transliteration, "text")) {
      const key = entry.getAttribute("for");
      if (!key) continue;
      // Spans are how a word-timed document splits the reading, and a
      // line-timed one has none. Keep the text either way: it reads
      // perfectly well under the line, it just cannot be highlighted
      // word by word — which is equally true of the line above it in
      // such a document.
      const words = ttmlWords(entry);
      const text = words
        ? words
            .map((word) => word.text)
            .join("")
            .trim()
        : (entry.textContent ?? "").replace(/\s+/g, " ").trim();
      if (text && !romanizationByKey.has(key)) {
        romanizationByKey.set(key, { text, words });
      }
    }
  }

  return { translationByKey, romanizationByKey };
}

/**
 * The text of a TTML document none of whose lines carries a `begin`,
 * as plain lyrics: one line per `<p>`, a blank line between the `<div>`
 * stanzas that hold them. `null` when the document does not parse, is
 * not rooted at `<tt>`, or has no text in its lines.
 *
 * Apple serves lyrics it has no timing for this way
 * (`itunes:timing="None"`). {@link parseTtml} skips every untimed line,
 * so such a document parsed to nothing, and every surface that falls
 * back to the raw content for unsynced lyrics showed the XML itself.
 *
 * The root check matters because the backend's format sniff accepts any
 * content opening with `<?xml`: an XHTML error page labelled `ttml` would
 * otherwise have its paragraphs shown as lyrics.
 */
function untimedTtmlText(content: string): string | null {
  if (typeof DOMParser === "undefined") return null;
  const doc = new DOMParser().parseFromString(content, "application/xml");
  if (doc.querySelector("parsererror")) return null;
  if (localNameOf(doc.documentElement) !== "tt") return null;
  const stanzas: string[][] = [];
  let stanzaOf: Element | null = null;
  for (const p of byLocalName(doc, "p")) {
    const text = (p.textContent ?? "").replace(/\s+/g, " ").trim();
    if (!text) continue;
    let div: Element | null = p.parentElement;
    while (div && localNameOf(div) !== "div") div = div.parentElement;
    if (stanzas.length === 0 || div !== stanzaOf) {
      stanzas.push([]);
      stanzaOf = div;
    }
    stanzas[stanzas.length - 1].push(text);
  }
  if (stanzas.length === 0) return null;
  return stanzas.map((lines) => lines.join("\n")).join("\n\n");
}

/**
 * Every payload the backend returns passes through here before a surface
 * sees it. A TTML document with no timed line is handed on as the plain
 * text it is, so the panel, the immersive view, the mini-player and the
 * editor all read it as unsynced lyrics instead of each rendering the
 * markup. The stored document is untouched: a timed TTML, or one this
 * cannot read, goes through as it came.
 */
function showableLyrics<T extends LyricsPayload | null>(payload: T): T {
  if (!payload || payload.format !== "ttml") return payload;
  if (parseTtml(payload.content).length > 0) return payload;
  const text = untimedTtmlText(payload.content);
  if (text === null) return payload;
  return { ...payload, format: "plain", content: text };
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

  const localizations = readTtmlLocalizations(doc);
  const out: LyricsLine[] = [];
  // By local name like everything else here. A document that prefixes
  // its TTML elements parsed to nothing before, so this loses no
  // behaviour — but a parser that understood prefixes for the
  // localizations and not for the lines they attach to would be the
  // kind of half-rule that reads as a bug later.
  const paragraphs = byLocalName(doc, "p");
  for (const p of paragraphs) {
    const lineBegin = parseTtmlTime(p.getAttribute("begin"));
    if (lineBegin < 0) continue;
    const lineEnd = parseTtmlTime(p.getAttribute("end"));

    const words = ttmlWords(p);
    const text =
      words !== undefined
        ? words
            .map((w) => w.text)
            .join("")
            .trim()
        : (p.textContent ?? "").replace(/\s+/g, " ").trim();

    if (!text && (!words || words.length === 0)) continue;

    // Apple keys every line so its localizations can point back at it.
    // Joining on that key rather than on position is the whole reason
    // this is safe: a localized document may omit a line, and matching
    // by index would then shift every following translation onto the
    // wrong line without anything looking broken.
    const key =
      p.getAttributeNS(ITUNES_NS, "key") ?? p.getAttribute("itunes:key");
    const romanized = key
      ? localizations.romanizationByKey.get(key)
      : undefined;
    const translated = key
      ? localizations.translationByKey.get(key)
      : undefined;

    out.push({
      timeMs: lineBegin,
      endMs: lineEnd >= 0 ? lineEnd : -1,
      text,
      words,
      romanization: usableRomanization(romanized, words, text),
      translation:
        translated && !saysTheSameThing(translated, text)
          ? translated
          : undefined,
    });
  }

  out.sort((a, b) => a.timeMs - b.timeMs);
  fillEndTimestamps(out);

  // Only now, because `fillEndTimestamps` closes the `endMs` a source
  // left open — on the line's own words, which it knows about, and not
  // on the romanization, which it does not. Copying the bounds after it
  // has run makes `LyricsReading`'s promise true by construction rather
  // than by trusting the document: the two are measured identical
  // anyway, so this changes no timing. It removes the way they could
  // quietly stop being identical.
  //
  // Nothing reads these bounds today — the rendering follows
  // `activeWordIndex`, computed from the line. They are here for
  // whatever gives the romanization its own progressive fill, which is
  // precisely the reader a leftover `-1` would bite.
  for (const line of out) {
    const romanized = line.romanization?.words;
    if (!romanized || !line.words || romanized.length !== line.words.length) {
      continue;
    }
    for (let i = 0; i < romanized.length; i += 1) {
      romanized[i].timeMs = line.words[i].timeMs;
      romanized[i].endMs = line.words[i].endMs;
    }
  }

  return out;
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
