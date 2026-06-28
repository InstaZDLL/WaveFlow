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
 * Set or clear **both** overrides in a single backend transaction so a
 * failure can't leave a half-applied state. Pass `null`/blank `bio` to
 * drop the bio override; pass `null`/empty `similarIds` to drop the
 * similar override. Similar order is preserved; self-references and
 * duplicates are dropped server-side.
 */
export function setArtistMetadataOverrides(
  artistId: number,
  bio: string | null,
  similarIds: number[] | null,
): Promise<void> {
  return invoke<void>("set_artist_metadata_overrides", {
    artistId,
    bio,
    similarIds,
  });
}
