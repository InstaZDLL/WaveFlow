import type { Track } from "./tauri/track";
import type { QueueTrackPayload } from "./tauri/player";

/**
 * Widen a lock-free `QueueTrackPayload` (the slim shape the engine ships
 * for queue rows + the current-track event) into a full `Track`. Fields
 * the queue payload doesn't carry — `rating`, `track_number`,
 * `library_id`, `added_at`, … — default to `null`/`0`, and the track
 * context menu omits the actions that need them.
 *
 * `album_id` IS carried, and must be: the now-playing surfaces key the
 * album's manual motion cover by it. Defaulted to `null`, every playing
 * track asked for its motion cover by title alone, the backend skipped
 * the manual override, and a hand-set cover never showed (#766).
 */
export function queuePayloadToTrack(payload: QueueTrackPayload): Track {
  return {
    id: payload.id,
    library_id: 0,
    title: payload.title,
    album_id: payload.album_id,
    album_title: payload.album_title,
    artist_id: payload.artist_id,
    artist_name: payload.artist_name,
    artist_ids: payload.artist_ids,
    duration_ms: payload.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: payload.bitrate,
    sample_rate: payload.sample_rate,
    channels: payload.channels,
    bit_depth: payload.bit_depth,
    codec: payload.codec,
    musical_key: null,
    file_path: payload.file_path,
    file_size: payload.file_size,
    added_at: 0,
    artwork_path: payload.artwork_path,
    artwork_path_1x: payload.artwork_path_1x,
    artwork_path_2x: payload.artwork_path_2x,
    rating: null,
  };
}
