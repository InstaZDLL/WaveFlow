import { invoke } from "@tauri-apps/api/core";

export type StatsRange = "7d" | "30d" | "90d" | "1y" | "all";

export interface StatsOverview {
  total_plays: number;
  total_ms: number;
  unique_tracks: number;
  unique_artists: number;
  /** 0..1 — share of plays where the track was finished. */
  completion_rate: number;
}

export interface TopTrackRow {
  track_id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  artist_ids: string | null;
  album_id: number | null;
  album_title: string | null;
  plays: number;
  listened_ms: number;
  artwork_path: string | null;
}

export interface TopArtistRow {
  artist_id: number;
  name: string;
  plays: number;
  listened_ms: number;
  picture_url: string | null;
  picture_path: string | null;
}

export interface TopAlbumRow {
  album_id: number;
  title: string;
  artist_id: number | null;
  artist_name: string | null;
  plays: number;
  listened_ms: number;
  artwork_path: string | null;
}

export interface ListeningByDayRow {
  /** "YYYY-MM-DD" in local timezone. */
  day: string;
  plays: number;
  listened_ms: number;
}

export function statsOverview(range: StatsRange): Promise<StatsOverview> {
  return invoke<StatsOverview>("stats_overview", { range });
}

export function statsTopTracks(
  range: StatsRange,
  limit: number,
): Promise<TopTrackRow[]> {
  return invoke<TopTrackRow[]>("stats_top_tracks", { range, limit });
}

export function statsTopArtists(
  range: StatsRange,
  limit: number,
): Promise<TopArtistRow[]> {
  return invoke<TopArtistRow[]>("stats_top_artists", { range, limit });
}

export function statsTopAlbums(
  range: StatsRange,
  limit: number,
): Promise<TopAlbumRow[]> {
  return invoke<TopAlbumRow[]>("stats_top_albums", { range, limit });
}

export function statsListeningByDay(
  range: StatsRange,
): Promise<ListeningByDayRow[]> {
  return invoke<ListeningByDayRow[]>("stats_listening_by_day", { range });
}

/** Returns 24 entries — index = local hour-of-day. */
export function statsListeningByHour(range: StatsRange): Promise<number[]> {
  return invoke<number[]>("stats_listening_by_hour", { range });
}
