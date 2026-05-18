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
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Best-quality bit depth across the album's tracks, used by the
   *  Hi-Res badge on the cover. `null` when no track has a known
   *  bit depth (e.g. all-MP3 album). */
  max_bit_depth: number | null;
  max_sample_rate: number | null;
}

/** Artist row returned by `list_artists`. */
export interface ArtistRow {
  id: number;
  name: string;
  track_count: number;
  album_count: number;
  /** Absolute filesystem path to a locally-extracted artist image
   *  (sidecar `artist.jpg` / `<name>.jpg` next to the tracks). Prefer
   *  this over `picture_path` / `picture_url` when present. */
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  /** Deezer CDN URL, populated after the artist has been enriched at least once. */
  picture_url: string | null;
  /** Absolute filesystem path to the locally-cached Deezer picture, when available. */
  picture_path: string | null;
  picture_path_1x: string | null;
  picture_path_2x: string | null;
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

export function listAlbums(
  libraryId: number | null,
  options?: {
    filterNoCover?: boolean;
    orderBy?: string;
    direction?: "asc" | "desc";
  },
): Promise<AlbumRow[]> {
  return invoke<AlbumRow[]>("list_albums", {
    libraryId,
    filterNoCover: options?.filterNoCover ?? null,
    orderBy: options?.orderBy ?? null,
    direction: options?.direction ?? null,
  });
}

export function listArtists(
  libraryId: number | null,
  sort?: { orderBy?: string; direction?: "asc" | "desc" },
): Promise<ArtistRow[]> {
  return invoke<ArtistRow[]>("list_artists", {
    libraryId,
    orderBy: sort?.orderBy ?? null,
    direction: sort?.direction ?? null,
  });
}

export function listGenres(libraryId: number | null): Promise<GenreRow[]> {
  return invoke<GenreRow[]>("list_genres", { libraryId });
}

export function listFolders(libraryId: number | null): Promise<FolderRow[]> {
  return invoke<FolderRow[]>("list_folders", { libraryId });
}

/** Row shape returned by `list_recent_plays`. */
export interface RecentPlay {
  track_id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  artist_ids: string | null;
  album_id: number | null;
  album_title: string | null;
  duration_ms: number;
  played_at: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  file_path: string;
}

export function listRecentPlays(
  libraryId: number | null,
  limit: number,
): Promise<RecentPlay[]> {
  return invoke<RecentPlay[]>("list_recent_plays", { libraryId, limit });
}

/** Row shape returned by `list_play_history` — one entry per
 *  play_event (no per-track dedup). */
export interface PlayHistoryRow {
  event_id: number;
  played_at: number;
  listened_ms: number;
  completed: boolean;
  track_id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  artist_ids: string | null;
  album_id: number | null;
  album_title: string | null;
  duration_ms: number;
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
  file_path: string;
}

/** One bucket per (year, month) for the history scrubber. */
export interface PlayHistoryMonth {
  year: number;
  month: number;
  /** Unix epoch ms at the first instant of this month (UTC). */
  start_ms: number;
  plays: number;
}

export function listPlayHistory(args: {
  beforeMs?: number | null;
  afterMs?: number | null;
  limit: number;
}): Promise<PlayHistoryRow[]> {
  return invoke<PlayHistoryRow[]>("list_play_history", {
    beforeMs: args.beforeMs ?? null,
    afterMs: args.afterMs ?? null,
    limit: args.limit,
  });
}

export function playHistoryMonths(): Promise<PlayHistoryMonth[]> {
  return invoke<PlayHistoryMonth[]>("play_history_months");
}

/** Profile-wide counters for the sidebar. */
export interface ProfileStats {
  liked_count: number;
  recent_plays_count: number;
}

export function getProfileStats(): Promise<ProfileStats> {
  return invoke<ProfileStats>("get_profile_stats");
}
