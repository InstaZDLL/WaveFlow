import type { RecentPlay } from "../../../lib/tauri/browse";
import type { Track } from "../../../lib/tauri/track";

/**
 * `RecentPlay` is a flat row from the SQL query and lacks the codec /
 * quality columns the player uses for diagnostics. Filling them with
 * nulls is fine — the player already treats those as optional.
 */
export function recentPlayToTrack(rp: RecentPlay): Track {
  return {
    id: rp.track_id,
    library_id: 0,
    title: rp.title,
    album_id: rp.album_id,
    album_title: rp.album_title,
    artist_id: rp.artist_id,
    artist_name: rp.artist_name,
    artist_ids: rp.artist_ids,
    duration_ms: rp.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: null,
    musical_key: null,
    file_path: rp.file_path,
    file_size: 0,
    added_at: 0,
    artwork_path: rp.artwork_path,
    artwork_path_1x: rp.artwork_path_1x,
    artwork_path_2x: rp.artwork_path_2x,
    rating: null,
  };
}
