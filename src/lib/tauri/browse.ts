import { invoke } from "@tauri-apps/api/core";

/** Album row returned by `list_albums`. */
export interface AlbumRow {
  id: number;
  title: string;
  artist_name: string | null;
  year: number | null;
  track_count: number;
  total_duration_ms: number;
  /** Absolute filesystem path to the extracted cover image, if any. */
  artwork_path: string | null;
}

/** Artist row returned by `list_artists`. */
export interface ArtistRow {
  id: number;
  name: string;
  track_count: number;
  album_count: number;
}

/** Genre row returned by `list_genres`. */
export interface GenreRow {
  id: number;
  name: string;
  track_count: number;
}

/** Folder row returned by `list_folders`. */
export interface FolderRow {
  id: number;
  path: string;
  last_scanned_at: number | null;
  is_watched: number;
  track_count: number;
}

export function listAlbums(libraryId: number): Promise<AlbumRow[]> {
  return invoke<AlbumRow[]>("list_albums", { libraryId });
}

export function listArtists(libraryId: number): Promise<ArtistRow[]> {
  return invoke<ArtistRow[]>("list_artists", { libraryId });
}

export function listGenres(libraryId: number): Promise<GenreRow[]> {
  return invoke<GenreRow[]>("list_genres", { libraryId });
}

export function listFolders(libraryId: number): Promise<FolderRow[]> {
  return invoke<FolderRow[]>("list_folders", { libraryId });
}

/** Row shape returned by `list_recent_plays`. */
export interface RecentPlay {
  track_id: number;
  title: string;
  artist_name: string | null;
  album_title: string | null;
  duration_ms: number;
  played_at: number;
  artwork_path: string | null;
}

export function listRecentPlays(
  libraryId: number,
  limit: number
): Promise<RecentPlay[]> {
  return invoke<RecentPlay[]>("list_recent_plays", { libraryId, limit });
}

/** Profile-wide counters for the sidebar. */
export interface ProfileStats {
  liked_count: number;
  recent_plays_count: number;
}

export function getProfileStats(): Promise<ProfileStats> {
  return invoke<ProfileStats>("get_profile_stats");
}
