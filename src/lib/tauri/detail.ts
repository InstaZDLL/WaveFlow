import { invoke } from "@tauri-apps/api/core";
import type { Track } from "./track";

// ── Album detail ────────────────────────────────────────────────────

export interface AlbumTrack {
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

// Re-export Track so views can import everything from one place.
export type { Track };
