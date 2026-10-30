import { invoke } from "@tauri-apps/api/core";

/**
 * The library enrichment engine: MusicBrainz identifiers for the local
 * library, and the gaps they fill — genres, release dates, credits,
 * covers, artist facts.
 *
 * The engine writes the database only, never an audio file. A sure match
 * is applied on its own; a doubtful album, and every artist, waits for
 * the review these calls drive.
 */

/** Emitted whenever a decision changes. */
export const ENRICHMENT_CHANGED_EVENT = "enrichment:changed";

export type DecisionStatus = "applied" | "review" | "rejected" | "no_match";

export interface EnrichmentStats {
  albums_applied: number;
  albums_review: number;
  albums_rejected: number;
  albums_no_match: number;
  artists_applied: number;
  artists_review: number;
  artists_rejected: number;
  artists_no_match: number;
}

export interface EnrichmentStatus {
  running: boolean;
  stats: EnrichmentStats;
}

/** A local album beside the release it was matched with. */
export interface AlbumDecision {
  album_id: number;
  status: DecisionStatus;
  /** 0–1. */
  score: number | null;
  local_title: string;
  local_artist: string | null;
  local_tracks: number;
  local_year: number | null;
  release_mbid: string | null;
  release_title: string | null;
  release_artist: string | null;
  release_country: string | null;
  release_track_count: number | null;
  original_date: string | null;
  /** A JSON array of genre names. */
  genres: string | null;
  matched_at: number;
}

/** A library artist beside the MusicBrainz artist proposed for it. */
export interface ArtistDecision {
  artist_id: number;
  local_name: string;
  status: DecisionStatus;
  score: number | null;
  artist_mbid: string | null;
  name: string | null;
  disambiguation: string | null;
  /** MusicBrainz's artist type: "Person", "Group", "Orchestra"… */
  kind: string | null;
  country: string | null;
  /** How many matched albums credit this artist under this name. */
  album_evidence: number;
  matched_at: number;
}

/** What an accepted artist shows on its page. */
export interface ArtistFacts {
  artist_mbid: string | null;
  name: string | null;
  disambiguation: string | null;
  kind: string | null;
  country: string | null;
  area: string | null;
  begin_date: string | null;
  end_date: string | null;
  /** A JSON array of genre names. */
  genres: string | null;
}

export interface TrackCredit {
  /** MusicBrainz's relationship type: composer, producer, vocal… */
  role: string;
  name: string;
  /** The instrument or vocal part; empty when the role has none. */
  detail: string;
  artist_mbid: string | null;
}

export function getEnrichmentStatus(): Promise<EnrichmentStatus> {
  return invoke<EnrichmentStatus>("enrichment_status");
}

/** Start a pass now, or right after the running one. No-op while the
 *  engine is off for the active profile. */
export function runEnrichment(): Promise<void> {
  return invoke<void>("enrichment_run");
}

export function cancelEnrichment(): Promise<boolean> {
  return invoke<boolean>("enrichment_cancel");
}

export function getAlbumDecisions(
  status: DecisionStatus,
): Promise<AlbumDecision[]> {
  return invoke<AlbumDecision[]>("enrichment_album_decisions", { status });
}

export function acceptAlbumMatch(albumId: number): Promise<void> {
  return invoke<void>("enrichment_accept_album", { albumId });
}

export function rejectAlbumMatch(albumId: number): Promise<void> {
  return invoke<void>("enrichment_reject_album", { albumId });
}

export function undoAlbumMatch(albumId: number): Promise<void> {
  return invoke<void>("enrichment_undo_album", { albumId });
}

export function forgetAlbumMatch(albumId: number): Promise<void> {
  return invoke<void>("enrichment_forget_album", { albumId });
}

export function getArtistDecisions(
  status: DecisionStatus,
): Promise<ArtistDecision[]> {
  return invoke<ArtistDecision[]>("enrichment_artist_decisions", { status });
}

/** Resolves to how many were accepted. */
export function acceptArtistMatches(artistIds: number[]): Promise<number> {
  return invoke<number>("enrichment_accept_artists", { artistIds });
}

export function rejectArtistMatch(artistId: number): Promise<void> {
  return invoke<void>("enrichment_reject_artist", { artistId });
}

export function forgetArtistMatch(artistId: number): Promise<void> {
  return invoke<void>("enrichment_forget_artist", { artistId });
}

export function getArtistMusicBrainz(
  artistId: number,
): Promise<ArtistFacts | null> {
  return invoke<ArtistFacts | null>("get_artist_musicbrainz", { artistId });
}

export function getTrackMusicBrainzCredits(
  trackId: number,
): Promise<TrackCredit[]> {
  return invoke<TrackCredit[]>("get_track_musicbrainz_credits", { trackId });
}

/** A stored JSON array of genre names, or nothing. */
export function parseGenres(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((g): g is string => typeof g === "string")
      : [];
  } catch {
    return [];
  }
}

export function musicBrainzUrl(
  entity: "release" | "artist",
  mbid: string,
): string {
  return `https://musicbrainz.org/${entity}/${encodeURIComponent(mbid)}`;
}
