import { invoke } from "@tauri-apps/api/core";

export interface SpotifyStatus {
  configured: boolean;
  connected: boolean;
  username: string | null;
  product: string | null;
}

export interface SpotifyAccessToken {
  access_token: string;
  expires_at: number;
}

export interface SpotifyArtistLite {
  id: string;
  name: string;
  uri: string;
  image_url: string | null;
}

export interface SpotifyAlbumLite {
  id: string;
  name: string;
  uri: string;
  image_url: string | null;
  artist_name: string | null;
  release_date: string | null;
}

export interface SpotifyTrackLite {
  id: string;
  name: string;
  uri: string;
  duration_ms: number;
  explicit: boolean;
  artist_name: string | null;
  album_name: string | null;
  image_url: string | null;
}

export interface SpotifyPlaylistLite {
  id: string;
  name: string;
  uri: string;
  description: string | null;
  image_url: string | null;
  owner_name: string | null;
  track_count: number;
}

export interface SpotifySearchResults {
  tracks: SpotifyTrackLite[];
  albums: SpotifyAlbumLite[];
  artists: SpotifyArtistLite[];
}

export function getSpotifyClientId(): Promise<string | null> {
  return invoke<string | null>("get_spotify_client_id");
}

export function setSpotifyClientId(clientId: string): Promise<void> {
  return invoke<void>("set_spotify_client_id", { clientId });
}

export function spotifyGetStatus(): Promise<SpotifyStatus> {
  return invoke<SpotifyStatus>("spotify_get_status");
}

export function spotifyLogin(): Promise<SpotifyStatus> {
  return invoke<SpotifyStatus>("spotify_login");
}

export function spotifyLogout(): Promise<void> {
  return invoke<void>("spotify_logout");
}

export function spotifyGetAccessToken(): Promise<SpotifyAccessToken> {
  return invoke<SpotifyAccessToken>("spotify_get_access_token");
}

export function spotifyListPlaylists(): Promise<SpotifyPlaylistLite[]> {
  return invoke<SpotifyPlaylistLite[]>("spotify_list_playlists");
}

export function spotifyGetPlaylistTracks(
  playlistId: string,
): Promise<SpotifyTrackLite[]> {
  return invoke<SpotifyTrackLite[]>("spotify_get_playlist_tracks", {
    playlistId,
  });
}

export function spotifySearch(query: string): Promise<SpotifySearchResults> {
  return invoke<SpotifySearchResults>("spotify_search", { query });
}

export function spotifyPauseLocal(): Promise<void> {
  return invoke<void>("spotify_pause_local");
}
