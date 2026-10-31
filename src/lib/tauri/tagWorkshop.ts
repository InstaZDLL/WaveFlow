import { invoke } from "@tauri-apps/api/core";

/**
 * The tag workshop: many files' tags edited at once, matched against
 * MusicBrainz if asked, written only after their differences have been
 * previewed, and undone from a journal.
 *
 * Mirrors `commands::tag_workshop`. Nested payloads keep the backend's
 * snake_case names; only the top-level arguments are camelCase.
 */

/** Emitted when a batch is written or undone. */
export const TAG_WORKSHOP_CHANGED_EVENT = "tag-workshop:changed";

/** Every field the workshop edits. The names are the backend's
 *  `tag_workshop::Field` and the journal's. */
export const WORKSHOP_FIELDS = [
  "title",
  "artist",
  "album",
  "album_artist",
  "year",
  "track_number",
  "track_total",
  "disc_number",
  "disc_total",
  "genre",
  "composer",
  "mb_release_id",
  "mb_release_group_id",
  "mb_recording_id",
  "mb_release_track_id",
  "mb_artist_id",
  "mb_album_artist_id",
] as const;

export type WorkshopField = (typeof WORKSHOP_FIELDS)[number];

/** A file's tags, or the tags asked of it. Absent is absent; `""` in a
 *  change clears the field. */
export type WorkshopValues = Partial<Record<WorkshopField, string>>;

export const NUMBER_FIELDS: readonly WorkshopField[] = [
  "year",
  "track_number",
  "track_total",
  "disc_number",
  "disc_total",
];

export const IDENTIFIER_FIELDS: readonly WorkshopField[] = [
  "mb_release_id",
  "mb_release_group_id",
  "mb_recording_id",
  "mb_release_track_id",
  "mb_artist_id",
  "mb_album_artist_id",
];

export type WorkshopSource =
  { kind: "tracks"; track_ids: number[] } | { kind: "album"; album_id: number };

export interface WorkshopRow {
  track_id: number;
  album_id: number | null;
  file_path: string;
  duration_ms: number;
  values: WorkshopValues;
  /** Unreadable, or a container no tag can be written into. */
  error: string | null;
}

/** What the workshop opened: `total` is more than `rows.length` when the
 *  source was cut to the backend's limit. */
export interface WorkshopOpened {
  rows: WorkshopRow[];
  total: number;
}

/** The values asked of one file, for a preview. */
export interface TrackChange {
  track_id: number;
  values: WorkshopValues;
}

/** What a write puts into one file. */
export interface TrackWrite {
  track_id: number;
  values: WorkshopValues;
  /** Each changed field's old value as the preview showed it: the write
   *  refuses a file whose tags moved since. Required. */
  expected: WorkshopValues;
}

export interface FieldDiff {
  field: WorkshopField;
  old: string;
  new: string;
}

export interface FilePreview {
  track_id: number;
  file_path: string;
  diffs: FieldDiff[];
  invalid: { field: WorkshopField; reason: string } | null;
  error: string | null;
}

export interface FileProblem {
  track_id: number | null;
  file_path: string;
  /** `changed_since`, `missing`, `interrupted`, `invalid`,
   *  `database_stale`, or the write's own message. */
  reason: string;
}

export interface ApplyReport {
  batch_id: number | null;
  written: number;
  unchanged: number;
  /** Tracks whose files now say what was asked: written, or already so. */
  done: number[];
  problems: FileProblem[];
  cancelled: boolean;
  covers: number;
}

export interface UndoReport {
  restored: number;
  refused: FileProblem[];
  cancelled: boolean;
  done: boolean;
}

export interface BatchSummary {
  id: number;
  created_at: number;
  label: string;
  release_mbid: string | null;
  undone_at: number | null;
  files: number;
  written: number;
  pending: number;
  failed: number;
  undone: number;
}

export interface CoverRequest {
  release_mbid: string;
  release_group_mbid: string | null;
  has_front_cover: boolean;
}

export interface MatchTrack {
  track_id: number;
  title: string;
  duration_ms: number | null;
  track_number: number | null;
  disc_number: number | null;
}

export type MatchVerdict = "confident" | "doubtful" | "no_match";

export interface ReleaseInfo {
  mbid: string;
  release_group_mbid: string | null;
  title: string;
  artist: string;
  date: string | null;
  country: string | null;
  track_count: number;
  has_front_cover: boolean;
}

export interface AlbumProposal {
  release: ReleaseInfo;
  verdict: MatchVerdict;
  score: { total: number; paired: number; local_tracks: number };
  from_tags: boolean;
  tracks: { track_id: number; score: number; values: WorkshopValues }[];
  unmatched: number[];
}

// Every call but the match names the profile the workshop was opened in:
// the backend refuses it once another profile is active.

export function openWorkshop(
  profileId: number,
  source: WorkshopSource,
): Promise<WorkshopOpened> {
  return invoke<WorkshopOpened>("tag_workshop_open", { profileId, source });
}

export function previewWorkshop(
  profileId: number,
  changes: TrackChange[],
): Promise<FilePreview[]> {
  return invoke<FilePreview[]>("tag_workshop_preview", { profileId, changes });
}

export function applyWorkshop(
  profileId: number,
  changes: TrackWrite[],
  label: string,
  releaseMbid: string | null,
  cover: CoverRequest | null,
): Promise<ApplyReport> {
  return invoke<ApplyReport>("tag_workshop_apply", {
    profileId,
    changes,
    label,
    releaseMbid,
    cover,
  });
}

export function listWorkshopBatches(
  profileId: number,
): Promise<BatchSummary[]> {
  return invoke<BatchSummary[]>("tag_workshop_batches", { profileId });
}

export function undoWorkshopBatch(
  profileId: number,
  batchId: number,
): Promise<UndoReport> {
  return invoke<UndoReport>("tag_workshop_undo", { profileId, batchId });
}

export function matchWorkshop(
  album: string,
  artist: string | null,
  tracks: MatchTrack[],
  releaseMbid: string | null,
  taggedRelease: string | null,
): Promise<AlbumProposal | null> {
  return invoke<AlbumProposal | null>("tag_workshop_match", {
    album,
    artist,
    tracks,
    releaseMbid,
    taggedRelease,
  });
}

const UUID = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
const RELEASE_URL = new RegExp(`/release/(${UUID})(?:[/?#]|$)`, "i");
const BARE_UUID = new RegExp(`^${UUID}$`, "i");

/** The release identifier in a pasted MusicBrainz release link, or the
 *  identifier itself. A link to another entity — a release group, an
 *  artist — names no release, and gives none: its identifier would be
 *  looked up as a release that does not exist. */
export function parseReleaseMbid(input: string): string | null {
  const text = input.trim();
  const fromUrl = RELEASE_URL.exec(text);
  if (fromUrl) return fromUrl[1].toLowerCase();
  return BARE_UUID.test(text) ? text.toLowerCase() : null;
}
