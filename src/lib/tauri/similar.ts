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

/**
 * Similar artists by name — for a remote-source artist (RFC-005) with no
 * local row. Same Last.fm → Deezer cascade + picture enrichment as
 * {@link getSimilarArtists}; suggestions in the user's library still
 * resolve their `library_artist_id`. `sync_v2` builds only.
 */
export function getSimilarArtistsByName(
  name: string,
): Promise<SimilarArtist[]> {
  return invoke<SimilarArtist[]>("get_similar_artists_by_name", { name });
}
