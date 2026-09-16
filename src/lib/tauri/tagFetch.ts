import { invoke } from "@tauri-apps/api/core";

/**
 * Fetching an album's tags from Deezer, for review (#599).
 *
 * Two steps on purpose. A title and an artist match several releases of
 * the same record — an original, a remaster, a deluxe edition with four
 * more tracks — and they carry different track lists, so which release
 * this is stays the user's call.
 *
 * Neither call writes anything. What the review screen accepts is
 * applied through `updateTrackTags`, the path that pauses playback
 * before opening the file, writes through the concrete tag so
 * non-standard frames survive, re-hashes and relinks the rows.
 */

/** One catalogue release that might be this record. */
export interface AlbumSource {
  deezer_id: number;
  title: string;
  artist: string | null;
  track_count: number | null;
  year: number | null;
  cover_url: string | null;
}

/**
 * The values a fetch can offer for one track.
 *
 * Every field is optional on both sides: the file may not carry it and
 * the catalogue may not either. Composer, genre and disc number are
 * absent because Deezer cannot fill them reliably, and a review screen
 * that lists a field the source cannot fill invites accepting a blank
 * over something the user typed.
 */
export interface TagValues {
  title: string | null;
  artist: string | null;
  album: string | null;
  year: number | null;
  track_number: number | null;
}

/** The fields the review screen can accept, one at a time. */
export const TAG_FIELDS = [
  "title",
  "artist",
  "album",
  "year",
  "track_number",
] as const;
export type TagField = (typeof TAG_FIELDS)[number];

/** How sure the matcher is about a pairing. */
export type MatchConfidence = "confident" | "doubtful";

export interface TrackProposal {
  track_id: number;
  /** Shown for a track nothing matched, where a title alone would not
   *  tell the user which file is meant. */
  file_name: string;
  current: TagValues;
  /** `null` when nothing in the release matched this file well enough.
   *  A real answer — the screen must not apply "the best available". */
  fetched: TagValues | null;
  score: number | null;
  confidence: MatchConfidence | null;
}

export interface AlbumProposals {
  album_id: number;
  deezer_id: number;
  /** Tracks in the album's own order, matched or not. */
  tracks: TrackProposal[];
  /** Catalogue tracks no local file claimed — a deluxe edition's
   *  extras, or the songs a partial rip is missing. */
  unmatched_remote: string[];
}

/** Catalogue releases that might be this album. */
export function searchAlbumTagSources(albumId: number): Promise<AlbumSource[]> {
  return invoke<AlbumSource[]>("search_album_tag_sources", { albumId });
}

/** Pair the chosen release's tracks with the local files. */
export function fetchAlbumTagProposals(
  albumId: number,
  deezerAlbumId: number,
): Promise<AlbumProposals> {
  return invoke<AlbumProposals>("fetch_album_tag_proposals", {
    albumId,
    deezerAlbumId,
  });
}
