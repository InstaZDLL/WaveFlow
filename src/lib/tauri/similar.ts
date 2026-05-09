import { invoke } from "@tauri-apps/api/core";

/// Mirrors `commands::similar::SimilarArtistDto` on the backend.
export interface SimilarArtist {
  name: string;
  match_score: number;
  picture_url: string | null;
  picture_path: string | null;
  /// Set when the suggested artist matches a row in the user's library.
  /// Click handlers should navigate to that profile-local artist page.
  library_artist_id: number | null;
  /// `lastfm` or `deezer` — surfaced for transparency, not used by the
  /// default UI.
  source: string;
}

export function getSimilarArtists(artistId: number): Promise<SimilarArtist[]> {
  return invoke<SimilarArtist[]>("get_similar_artists", { artistId });
}
