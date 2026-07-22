import { invoke } from "@tauri-apps/api/core";

/** Album row returned by `list_albums`. UI-facing shape — paths are
 *  reconstructed by the wrapper from the wire-format slim row. */
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

/** Artist row returned by `list_artists`. UI-facing shape. */
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

/**
 * Wire-format slim row shipped by `list_albums`. The per-profile
 * artwork directory is shared at response level via
 * [`ListAlbumsResponse.artwork_base`] so the ~70-char prefix isn't
 * repeated on every row. [`expandAlbumRow`] stitches the absolute
 * paths back together so every UI consumer keeps the full `AlbumRow`
 * shape unchanged.
 */
interface AlbumRowSlim {
  id: number;
  title: string;
  artist_name: string | null;
  year: number | null;
  track_count: number;
  total_duration_ms: number;
  artwork_hash: string | null;
  artwork_format: string | null;
  artwork_has_1x: boolean;
  artwork_has_2x: boolean;
  max_bit_depth: number | null;
  max_sample_rate: number | null;
}

interface ListAlbumsResponse {
  artwork_base: string;
  items: AlbumRowSlim[];
}

interface ArtistRowSlim {
  id: number;
  name: string;
  track_count: number;
  album_count: number;
  artwork_hash: string | null;
  artwork_format: string | null;
  artwork_has_1x: boolean;
  artwork_has_2x: boolean;
  picture_hash: string | null;
  picture_has_1x: boolean;
  picture_has_2x: boolean;
  picture_url: string | null;
}

interface ListArtistsResponse {
  artwork_base: string;
  /** Deezer picture cache dir — separate from `artwork_base` since the
   *  cache is shared across profiles. */
  metadata_artwork_base: string;
  items: ArtistRowSlim[];
}

function pathSep(base: string): string {
  return base.includes("\\") ? "\\" : "/";
}

function expandAlbumRow(
  item: AlbumRowSlim,
  base: string,
  sep: string,
): AlbumRow {
  const artwork_path =
    item.artwork_hash && item.artwork_format
      ? `${base}${sep}${item.artwork_hash}.${item.artwork_format}`
      : null;
  const artwork_path_1x =
    item.artwork_hash && item.artwork_has_1x
      ? `${base}${sep}${item.artwork_hash}_1x.jpg`
      : null;
  const artwork_path_2x =
    item.artwork_hash && item.artwork_has_2x
      ? `${base}${sep}${item.artwork_hash}_2x.jpg`
      : null;
  return {
    id: item.id,
    title: item.title,
    artist_name: item.artist_name,
    year: item.year,
    track_count: item.track_count,
    total_duration_ms: item.total_duration_ms,
    artwork_path,
    artwork_path_1x,
    artwork_path_2x,
    max_bit_depth: item.max_bit_depth,
    max_sample_rate: item.max_sample_rate,
  };
}

function expandArtistRow(
  item: ArtistRowSlim,
  artworkBase: string,
  metadataBase: string,
  artworkSep: string,
  metadataSep: string,
): ArtistRow {
  const artwork_path =
    item.artwork_hash && item.artwork_format
      ? `${artworkBase}${artworkSep}${item.artwork_hash}.${item.artwork_format}`
      : null;
  const artwork_path_1x =
    item.artwork_hash && item.artwork_has_1x
      ? `${artworkBase}${artworkSep}${item.artwork_hash}_1x.jpg`
      : null;
  const artwork_path_2x =
    item.artwork_hash && item.artwork_has_2x
      ? `${artworkBase}${artworkSep}${item.artwork_hash}_2x.jpg`
      : null;
  // Deezer cache is jpg-only — `<hash>.jpg` for the full, same suffix
  // pattern as the local artwork for the thumbnails.
  const picture_path = item.picture_hash
    ? `${metadataBase}${metadataSep}${item.picture_hash}.jpg`
    : null;
  const picture_path_1x =
    item.picture_hash && item.picture_has_1x
      ? `${metadataBase}${metadataSep}${item.picture_hash}_1x.jpg`
      : null;
  const picture_path_2x =
    item.picture_hash && item.picture_has_2x
      ? `${metadataBase}${metadataSep}${item.picture_hash}_2x.jpg`
      : null;
  return {
    id: item.id,
    name: item.name,
    track_count: item.track_count,
    album_count: item.album_count,
    artwork_path,
    artwork_path_1x,
    artwork_path_2x,
    picture_url: item.picture_url,
    picture_path,
    picture_path_1x,
    picture_path_2x,
  };
}

/** Genre row returned by `list_genres`. UI-facing shape — paths are
 *  reconstructed by the wrapper from the wire-format slim row. */
export interface GenreRow {
  id: number;
  name: string;
  track_count: number;
  /** Absolute filesystem path to a manually-set genre picture, if any. */
  artwork_path: string | null;
  artwork_path_1x: string | null;
  artwork_path_2x: string | null;
}

interface GenreRowSlim {
  id: number;
  name: string;
  track_count: number;
  artwork_hash: string | null;
  artwork_format: string | null;
  artwork_has_1x: boolean;
  artwork_has_2x: boolean;
}

interface ListGenresResponse {
  artwork_base: string;
  items: GenreRowSlim[];
}

function expandGenreRow(item: GenreRowSlim, base: string, sep: string): GenreRow {
  const artwork_path =
    item.artwork_hash && item.artwork_format
      ? `${base}${sep}${item.artwork_hash}.${item.artwork_format}`
      : null;
  const artwork_path_1x =
    item.artwork_hash && item.artwork_has_1x
      ? `${base}${sep}${item.artwork_hash}_1x.jpg`
      : null;
  const artwork_path_2x =
    item.artwork_hash && item.artwork_has_2x
      ? `${base}${sep}${item.artwork_hash}_2x.jpg`
      : null;
  return {
    id: item.id,
    name: item.name,
    track_count: item.track_count,
    artwork_path,
    artwork_path_1x,
    artwork_path_2x,
  };
}

/** Folder row returned by `list_folders`. */
export interface FolderRow {
  id: number;
  path: string;
  last_scanned_at: number | null;
  is_watched: number;
  track_count: number;
}

export async function listAlbums(
  libraryId: number | null,
  options?: {
    orderBy?: string;
    direction?: "asc" | "desc";
  },
): Promise<AlbumRow[]> {
  const resp = await invoke<ListAlbumsResponse>("list_albums", {
    libraryId,
    orderBy: options?.orderBy ?? null,
    direction: options?.direction ?? null,
  });
  const sep = pathSep(resp.artwork_base);
  return resp.items.map((item) => expandAlbumRow(item, resp.artwork_base, sep));
}

export async function listArtists(
  libraryId: number | null,
  sort?: { orderBy?: string; direction?: "asc" | "desc" },
): Promise<ArtistRow[]> {
  const resp = await invoke<ListArtistsResponse>("list_artists", {
    libraryId,
    orderBy: sort?.orderBy ?? null,
    direction: sort?.direction ?? null,
  });
  const artSep = pathSep(resp.artwork_base);
  const metaSep = pathSep(resp.metadata_artwork_base);
  return resp.items.map((item) =>
    expandArtistRow(
      item,
      resp.artwork_base,
      resp.metadata_artwork_base,
      artSep,
      metaSep,
    ),
  );
}

/** Search albums by name for the global top-bar search. Same slim
 *  wire shape as `list_albums`, so the rows expand to the full
 *  `AlbumRow`. `limit` is clamped server-side (default 8). */
export async function searchAlbums(
  query: string,
  libraryId: number | null,
  limit?: number,
): Promise<AlbumRow[]> {
  const resp = await invoke<ListAlbumsResponse>("search_albums", {
    query,
    libraryId,
    limit: limit ?? null,
  });
  const sep = pathSep(resp.artwork_base);
  return resp.items.map((item) => expandAlbumRow(item, resp.artwork_base, sep));
}

/** Search artists by name for the global top-bar search. Mirror of
 *  `searchAlbums` over `list_artists`'s shape. */
export async function searchArtists(
  query: string,
  libraryId: number | null,
  limit?: number,
): Promise<ArtistRow[]> {
  const resp = await invoke<ListArtistsResponse>("search_artists", {
    query,
    libraryId,
    limit: limit ?? null,
  });
  const artSep = pathSep(resp.artwork_base);
  const metaSep = pathSep(resp.metadata_artwork_base);
  return resp.items.map((item) =>
    expandArtistRow(
      item,
      resp.artwork_base,
      resp.metadata_artwork_base,
      artSep,
      metaSep,
    ),
  );
}

export async function listGenres(libraryId: number | null): Promise<GenreRow[]> {
  const resp = await invoke<ListGenresResponse>("list_genres", { libraryId });
  const sep = pathSep(resp.artwork_base);
  return resp.items.map((item) => expandGenreRow(item, resp.artwork_base, sep));
}

export function setGenreArtworkFromFile(
  genreId: number,
  filePath: string,
): Promise<void> {
  return invoke("set_genre_artwork_from_file", { genreId, filePath });
}

export function clearGenreArtwork(genreId: number): Promise<void> {
  return invoke("clear_genre_artwork", { genreId });
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
