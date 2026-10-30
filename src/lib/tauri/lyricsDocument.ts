import { invoke } from "@tauri-apps/api/core";

import type { LyricsFormat } from "./lyrics";

// Mirrors of the lyrics document model (`waveflow_core::lyrics::model`).
// Every time is in milliseconds and optional: a document is edited while
// it is still being synced, and the writers decide what an untimed unit
// means for their format.

export type LyricsAgentKind = "person" | "group" | "other";

/** A voice that sings some of the lines: one singer, or several together. */
export interface LyricsAgent {
  /** The id lines refer to (`v1`, `v2`, …). Unique within a document. */
  id: string;
  kind: LyricsAgentKind;
  name?: string;
}

/** One timed unit of a line: a word, or a syllable of one. */
export interface LyricsUnit {
  text: string;
  beginMs?: number;
  endMs?: number;
  /** No space before the next unit: syllables of one word. */
  joined?: boolean;
}

/** A line's text in another language. */
export interface LyricsLineTranslation {
  /** BCP 47 tag, when known. */
  lang?: string;
  text: string;
}

export interface LyricsDocumentLine {
  beginMs?: number;
  endMs?: number;
  /** Id of the agent singing it; absent is the lead. */
  agent?: string;
  /** Song section (`Verse`, `Chorus`, …), carried by TTML only. */
  part?: string;
  words: LyricsUnit[];
  /** Backing vocals sung over this line, timed on their own. */
  background?: LyricsUnit[];
  translations?: LyricsLineTranslation[];
  /** Latin-script reading of the line (romaji, pinyin, …). */
  romanization?: string;
}

export interface LyricsMetadata {
  title?: string;
  artist?: string;
  album?: string;
  /** BCP 47 tag of the sung language. */
  language?: string;
  songwriters?: string[];
}

export interface LyricsDocument {
  metadata: LyricsMetadata;
  agents: LyricsAgent[];
  lines: LyricsDocumentLine[];
}

export const EMPTY_LYRICS_DOCUMENT: LyricsDocument = {
  metadata: {},
  agents: [],
  lines: [],
};

/**
 * Read stored lyrics into the model. `format` is the payload's; without
 * it the content is sniffed. Rejects a TTML document it cannot parse.
 */
export function readLyricsDocument(
  content: string,
  format: LyricsFormat | null,
): Promise<{ format: LyricsFormat; document: LyricsDocument }> {
  return invoke("read_lyrics_document", { content, format });
}

export interface LyricsWriteOptions {
  /** LRC: translations as lines stamped at their original's time. */
  translations?: boolean;
}

/**
 * The model written in a format, or the first unit that keeps it from
 * being written (indexes from zero; `word` absent when the line itself
 * has no time).
 */
export type LyricsDocumentWrite =
  | { kind: "written"; content: string }
  | { kind: "unsynced"; line: number; word?: number };

export function writeLyricsDocument(
  document: LyricsDocument,
  format: LyricsFormat,
  options?: LyricsWriteOptions,
): Promise<LyricsDocumentWrite> {
  return invoke("write_lyrics_document", { document, format, options });
}
