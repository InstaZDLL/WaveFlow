import { type LibraryTrackRow } from "../../../lib/tauri/browse";
import { type Track } from "../../../lib/tauri/track";

// =============================================================================
// Tab-specific list components
// =============================================================================

/**
 * A local library row as the `Track` the player, the selection and the
 * playlist calls all speak.
 *
 * Not a cast. The row's identifiers are **text** — the two sources do not
 * share an identifier type, so the unified listing hands both back as strings
 * — and handing that object straight to code that compares `id` numerically
 * makes every comparison silently false: no row ever reads as selected, and
 * the queue is built from tracks the engine cannot match. Only ever called for
 * a row whose source is local; a server row has no `Track` to become.
 */
export function toLocalTrack(row: LibraryTrackRow): Track {
  return {
    id: Number(row.id),
    library_id: row.library_id ?? 0,
    title: row.title,
    album_id: row.album_id != null ? Number(row.album_id) : null,
    album_title: row.album_title,
    artist_id: row.artist_id != null ? Number(row.artist_id) : null,
    artist_name: row.artist_name,
    artist_ids: row.artist_ids,
    duration_ms: row.duration_ms,
    track_number: row.track_number,
    disc_number: row.disc_number,
    year: row.year,
    bitrate: row.bitrate,
    sample_rate: row.sample_rate,
    channels: row.channels,
    bit_depth: row.bit_depth,
    codec: row.codec,
    musical_key: row.musical_key,
    file_path: row.file_path ?? "",
    file_size: row.file_size ?? 0,
    added_at: row.added_at,
    artwork_path: row.artwork_path,
    artwork_path_1x: row.artwork_path_1x,
    artwork_path_2x: row.artwork_path_2x,
    rating: row.rating,
  };
}
