import { invoke } from "@tauri-apps/api/core";
import type { TopAlbumRow, TopArtistRow, TopTrackRow } from "./stats";

export interface MonthBucket {
  plays: number;
  listened_ms: number;
}

export interface ActiveDay {
  /** Local-time "YYYY-MM-DD". */
  day: string;
  plays: number;
  listened_ms: number;
}

export interface MoodProfile {
  avg_bpm: number | null;
  avg_lufs: number | null;
  /** One of "chill" / "warm" / "groove" / "energetic" / "fire", or null
   *  when no analysed track was played that year. */
  energy: string | null;
}

export interface FirstListen {
  track_id: number;
  title: string;
  artist_name: string | null;
  played_at: number;
}

export interface Streak {
  days: number;
  start: string;
  end: string;
}

export interface WrappedPayload {
  year: number;
  total_plays: number;
  total_listened_ms: number;
  unique_tracks: number;
  unique_artists: number;
  unique_albums: number;
  top_tracks: TopTrackRow[];
  top_artists: TopArtistRow[];
  top_albums: TopAlbumRow[];
  /** Calendar order — index 0 = January, 11 = December. */
  by_month: MonthBucket[];
  /** 24 buckets — index = local hour-of-day. */
  by_hour: number[];
  most_active_day: ActiveDay | null;
  mood: MoodProfile;
  first_listen: FirstListen | null;
  streak: Streak | null;
}

export function getWrapped(year: number): Promise<WrappedPayload> {
  return invoke<WrappedPayload>("get_wrapped", { year });
}

export function availableWrappedYears(): Promise<number[]> {
  return invoke<number[]>("available_wrapped_years");
}

export function wrappedCurrentYear(): Promise<number> {
  return invoke<number>("wrapped_current_year");
}

/**
 * Persist a Wrapped share PNG (built by the frontend Canvas renderer)
 * at `targetPath`. `bytes` is expected to be a raw PNG byte stream —
 * the backend writes it verbatim, so any image-encoder roundtrip
 * happens upstream.
 */
export function saveWrappedImage(
  bytes: Uint8Array,
  targetPath: string,
): Promise<void> {
  // Tauri's `invoke` serialises Uint8Array as a JSON number array; the
  // Rust side receives `Vec<u8>`. This is the canonical pattern in the
  // tauri-apps docs for the IPC byte channel.
  return invoke<void>("save_wrapped_image", {
    bytes: Array.from(bytes),
    targetPath,
  });
}
