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
