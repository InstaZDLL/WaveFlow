import { invoke } from "@tauri-apps/api/core";

/** Emitted by the backend after every artist a scan records, and when
 *  the scan ends. */
export const RELEASE_RADAR_CHANGED = "release-radar:changed";

/** One release, as `release_radar_list` returns it (mirrors
 *  `commands::release_radar::RadarRelease`). */
export interface RadarRelease {
  /** The MusicBrainz release-group id. */
  id: string;
  title: string;
  /** The library artist it was found for. */
  artist_id: number;
  artist_name: string;
  /** The whole credit, as MusicBrainz writes it ("A feat. B"). */
  artist_credit: string;
  /** `YYYY-MM-DD`, `YYYY-MM` or `YYYY`. */
  first_release_date: string;
  primary_type: string | null;
  secondary_types: string[];
  /** The cached Cover Art Archive front; `null` without one. */
  cover_path: string | null;
  musicbrainz_url: string;
  /** The local tracks matched to the release, in play order. Empty means
   *  the release is only discovered, and is never offered as playable. */
  track_ids: number[];
  /** `album`: the tracks are a local album of that title, to be played
   *  as a record. `track`: a single, found by a track's title. */
  matched_by: "album" | "track" | null;
  discovered_at: number;
}

export interface ReleaseRadarSnapshot {
  /** Newest first. */
  releases: RadarRelease[];
  /** Artists the radar follows. */
  followed: number;
  /** Of those, the ones MusicBrainz has answered for. */
  checked: number;
  /** Of those, the ones the next scan asks about. */
  due: number;
  last_checked_at: number | null;
  scanning: boolean;
  playlist_id: number | null;
}

export type ReleaseRadarScanOutcome =
  | "done"
  | "cancelled"
  | "rate_limited"
  | "offline"
  | "failed"
  | "profile_changed";

export interface ReleaseRadarScanSummary {
  outcome: ReleaseRadarScanOutcome;
  checked: number;
  due: number;
}

/** What `release-radar:changed` carries. */
export interface ReleaseRadarProgress {
  checked: number;
  due: number;
  running: boolean;
}

/** Everything the Release Radar view draws. Local only: answers offline. */
export function releaseRadarList(): Promise<ReleaseRadarSnapshot> {
  return invoke<ReleaseRadarSnapshot>("release_radar_list");
}

/**
 * Ask MusicBrainz about the followed artists that are due (all of them
 * with `rescan`), then rewrite the Release Radar playlist. Resolves when
 * the scan ends; it is stopped from the task bar. Answers `offline`
 * without a request when offline mode is on.
 */
export function releaseRadarScan(
  rescan: boolean,
  playlistDescription: string,
): Promise<ReleaseRadarScanSummary> {
  return invoke<ReleaseRadarScanSummary>("release_radar_scan", {
    rescan,
    playlistDescription,
  });
}

/** Rewrite the Release Radar playlist from what is already known — no
 *  request goes out. Resolves to its id, `null` when nothing is playable. */
export function releaseRadarRefreshPlaylist(
  playlistDescription: string,
): Promise<number | null> {
  return invoke<number | null>("release_radar_refresh_playlist", {
    playlistDescription,
  });
}
