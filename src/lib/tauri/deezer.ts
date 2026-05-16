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
