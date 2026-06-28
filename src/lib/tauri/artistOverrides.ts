import { invoke } from "@tauri-apps/api/core";

/** One curated similar-artist entry, resolved for the editor chips. */
export interface ArtistOverrideSimilar {
  artist_id: number;
  name: string;
  picture_url: string | null;
  picture_path: string | null;
}

/** Current per-artist override state, used to pre-fill the editor. */
export interface ArtistOverrides {
  /** `null` when no bio override is set (the fetched bio is used). */
  custom_bio: string | null;
  /** Empty when no similar override is set (the online list is used). */
  similar: ArtistOverrideSimilar[];
}

/** Read the override state (custom bio + curated similar list) for one artist. */
export function getArtistOverrides(artistId: number): Promise<ArtistOverrides> {
  return invoke<ArtistOverrides>("get_artist_overrides", { artistId });
}

/**
 * Set or clear the biography override. Pass `null` (or a blank string)
 * to drop the override and fall back to the fetched bio.
 */
export function setArtistBioOverride(
  artistId: number,
  bio: string | null,
): Promise<void> {
  return invoke<void>("set_artist_bio_override", { artistId, bio });
}

/**
 * Replace the curated similar-artist list. Pass `null` or an empty
 * array to drop the override (the online list takes over). Order is
 * preserved; self-references and duplicates are dropped server-side.
 */
export function setArtistSimilarOverride(
  artistId: number,
  similarIds: number[] | null,
): Promise<void> {
  return invoke<void>("set_artist_similar_override", { artistId, similarIds });
}
