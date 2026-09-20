import { invoke } from "@tauri-apps/api/core";

export interface DeezerAlbumLite {
  deezer_id: number;
  title: string;
  artist: string;
  cover_url: string | null;
}

export function searchAlbumsDeezer(query: string): Promise<DeezerAlbumLite[]> {
  return invoke<DeezerAlbumLite[]>("search_albums_deezer", { query });
}

export function setAlbumArtworkFromDeezer(
  albumId: number,
  deezerAlbumId: number,
): Promise<void> {
  return invoke<void>("set_album_artwork_from_deezer", {
    albumId,
    deezerAlbumId,
  });
}

export function setAlbumArtworkFromFile(
  albumId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_album_artwork_from_file", { albumId, filePath });
}

export function batchFetchMissingAlbumCovers(): Promise<number> {
  return invoke<number>("batch_fetch_missing_album_covers");
}

export function batchFetchMissingArtistPictures(): Promise<number> {
  return invoke<number>("batch_fetch_missing_artist_pictures");
}

export interface DeezerArtistLite {
  deezer_id: number;
  name: string;
  picture_url: string | null;
  nb_fan: number | null;
}

export function searchArtistsDeezer(
  query: string,
): Promise<DeezerArtistLite[]> {
  return invoke<DeezerArtistLite[]>("search_artists_deezer", { query });
}

export function setArtistArtworkFromDeezer(
  artistId: number,
  deezerArtistId: number,
): Promise<void> {
  return invoke<void>("set_artist_artwork_from_deezer", {
    artistId,
    deezerArtistId,
  });
}

export function setArtistArtworkFromFile(
  artistId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_artist_artwork_from_file", { artistId, filePath });
}

export function clearArtistArtwork(artistId: number): Promise<void> {
  return invoke<void>("clear_artist_artwork", { artistId });
}

export interface ArtistBackdropCandidates {
  urls: string[];
  /** `true` when the list is empty because offline mode refused it —
   *  the URLs are remote and the picker would paint them — rather than
   *  because the artist has no fanart. */
  offline: boolean;
}

/**
 * The wide backdrops on offer for an artist — every fanart TheAudioDB
 * listed for it, remote URLs (issue #693). Empty when the artist has
 * none, when nothing has enriched it yet, or when offline mode refused
 * them, which `offline` tells apart.
 */
export function getArtistBackdropCandidates(
  artistId: number,
): Promise<ArtistBackdropCandidates> {
  return invoke<ArtistBackdropCandidates>("get_artist_backdrop_candidates", {
    artistId,
  });
}

/** Download one of those candidates and make it the artist's backdrop. */
export function setArtistBackgroundFromUrl(
  artistId: number,
  url: string,
): Promise<void> {
  return invoke<void>("set_artist_background_from_url", { artistId, url });
}

/** Use a local image file as the artist's backdrop. */
export function setArtistBackgroundFromFile(
  artistId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_artist_background_from_file", {
    artistId,
    filePath,
  });
}

/** Back to the automatic backdrop: the first fanart, or the blurred photo. */
export function clearArtistBackground(artistId: number): Promise<void> {
  return invoke<void>("clear_artist_background", { artistId });
}
