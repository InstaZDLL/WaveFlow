import type { LibrarySource } from "./browse";
import { invoke } from "@tauri-apps/api/core";
import type { Track } from "./track";

// ── Album detail ────────────────────────────────────────────────────

export interface AlbumTrack {
  /** Absent on a local track, which is the unmarked case. */
  source?: LibrarySource;
  /** The server's identifier, present only on a remote track. It is what
   *  plays, what keys the row, and what the local `id` deliberately is not. */
  remote_id?: string;
  /** Remote only: resolved through the server cover cache. */
  artwork_hash?: string | null;
  /**
   * Local rowid. On a remote track this is a negative sentinel that is never
   * read — the type requires a number and there is none, so it is made
   * obviously invalid rather than plausibly wrong. Every site that acts on a
   * rowid checks `source` first.
   */
  id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  artist_ids: string | null;
  duration_ms: number;
  track_number: number | null;
  disc_number: number | null;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  file_path: string;
  bit_depth: number | null;
  sample_rate: number | null;
  codec: string | null;
  /** Carried so AlbumDetailView can build a complete `Track` for the
   *  context menu — the Properties modal reads these, and anything
   *  missing here used to be hard-coded to null there (issue #458). */
  year: number | null;
  bitrate: number | null;
  channels: number | null;
  musical_key: string | null;
  file_size: number;
  added_at: number;
  /** Half-star rating. The context menu's rating submenu is on by
   *  default, so a placeholder here made a rated track read as
   *  unrated on the album page. */
  rating: number | null;
}

export interface AlbumDetail {
  /** Remote only: the album's own cover, resolved through the server cover
   *  cache. A local album's cover is a file, and arrives as `artwork_path`. */
  artwork_hash?: string | null;
  id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  year: number | null;
  track_count: number;
  total_duration_ms: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  label: string | null;
  release_date: string | null;
  genres: string[];
  tracks: AlbumTrack[];
}

export function getAlbumDetail(albumId: number): Promise<AlbumDetail> {
  return invoke<AlbumDetail>("get_album_detail", { albumId });
}

// ── Artist detail ───────────────────────────────────────────────────

export interface ArtistAlbumRow {
  /** Absent on a local album, which is the unmarked case. */
  source?: LibrarySource;
  /** The server's identifier, present only on a remote album. */
  remote_id?: string;
  /** Remote only: resolved through the server cover cache. */
  artwork_hash?: string | null;
  /** Local rowid. Negative and never read on a remote album — see
   *  [`AlbumTrack.id`]. */
  id: number;
  title: string;
  year: number | null;
  track_count: number;
  total_duration_ms: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
}

export interface ArtistDetail {
  /** Remote only: the artist's server-side portrait, resolved through the
   *  cover cache. Everything else on the artist — the photo, the hero
   *  background, the biography — comes from the by-name enrichment on both
   *  sides, because the server carries none of it. */
  artwork_hash?: string | null;
  id: number;
  name: string;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  picture_url: string | null;
  picture_path: string | null;
  picture_path_1x: string | null;
  picture_path_2x: string | null;
  fans_count: number | null;
  bio_short: string | null;
  bio_full: string | null;
  /** Wide TheAudioDB fanart backing the artist hero (issue #482).
   *  `background_path` points into the shared metadata artwork cache;
   *  `background_url` is the remote fallback when the download failed. */
  background_url: string | null;
  background_path: string | null;
  track_count: number;
  album_count: number;
  albums: ArtistAlbumRow[];
}

export function getArtistDetail(artistId: number): Promise<ArtistDetail> {
  return invoke<ArtistDetail>("get_artist_detail", { artistId });
}

// ── Genre detail ────────────────────────────────────────────────────

export interface GenreDetail {
  id: number;
  name: string;
  track_count: number;
  total_duration_ms: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  tracks: Track[];
}

export function getGenreDetail(genreId: number): Promise<GenreDetail> {
  return invoke<GenreDetail>("get_genre_detail", { genreId });
}

// ── Deezer enrichment ───────────────────────────────────────────────

export interface DeezerAlbumEnrichment {
  deezer_id: number | null;
  label: string | null;
  release_date: string | null;
  cover_url: string | null;
  cover_path: string | null;
  cover_path_1x: string | null;
  cover_path_2x: string | null;
}

export interface DeezerArtistEnrichment {
  deezer_id: number | null;
  picture_url: string | null;
  picture_path: string | null;
  picture_path_1x: string | null;
  picture_path_2x: string | null;
  fans_count: number | null;
  /** Short biography from Last.fm (HTML stripped). */
  bio_short: string | null;
  /** Full biography from Last.fm (HTML stripped). */
  bio_full: string | null;
  /** Remote TheAudioDB fanart URL — fallback when the download failed. */
  background_url: string | null;
  /** Locally-cached wide fanart backing the artist hero (issue #482). */
  background_path: string | null;
}

export function enrichAlbumDeezer(
  albumId: number,
): Promise<DeezerAlbumEnrichment> {
  return invoke<DeezerAlbumEnrichment>("enrich_album_deezer", { albumId });
}

export function enrichArtistDeezer(
  artistId: number,
): Promise<DeezerArtistEnrichment> {
  return invoke<DeezerArtistEnrichment>("enrich_artist_deezer", { artistId });
}

/**
 * Enrich an artist by name (Deezer photo + TheAudioDB hero background +
 * Last.fm bio) — for a remote artist (RFC-005) with no local row. Same
 * shared cache as {@link enrichArtistDeezer}; returns empties offline or
 * when nothing matches.
 */
export function enrichArtistByName(
  name: string,
): Promise<DeezerArtistEnrichment> {
  return invoke<DeezerArtistEnrichment>("enrich_artist_by_name", { name });
}

// Re-export Track so views can import everything from one place.
export type { Track };
