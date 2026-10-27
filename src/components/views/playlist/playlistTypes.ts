import { type Track } from "../../../lib/tauri/track";
import { type LibrarySource } from "../../../lib/tauri/browse";

/**
 * Sort modes for the playlist track list. "custom" preserves the
 * user-curated drag-and-drop order stored as `playlist_track.position` —
 * any other mode is a display-only client-side sort that doesn't touch
 * the DB. Switching back to "custom" restores the persisted order
 * verbatim, Spotify-style.
 */
export type PlaylistSortMode =
  | "custom"
  | "title"
  | "artist"
  | "album"
  | "added_at"
  | "duration_ms"
  | "filename";

/**
 * A track row in this view, from the device or from the bound server.
 *
 * The remote fields are additions rather than reuses on purpose: the
 * server's ids are strings and the local ones are rowids, so carrying a
 * server id in `artist_id` would need a cast that satisfies the compiler
 * and converts nothing, leaving every comparison quietly false.
 */
export type PlaylistTrack = Track & {
  /** Absent on a local track, which is the unmarked case. */
  source?: LibrarySource;
  /** The server's track id: what plays, what the like acts on, and what
   *  the local `id` deliberately is not. */
  remote_id?: string;
  /** Remote only: resolved through the server cover cache. */
  artwork_hash?: string | null;
  /** Remote only: the server's ids for the artist / album links. */
  remote_artist_id?: string | null;
  remote_album_id?: string | null;
  /** Remote only: the synced favourite flag. The local liked-id set is
   *  keyed by rowid and knows nothing about a server track. */
  remote_starred?: boolean;
};
