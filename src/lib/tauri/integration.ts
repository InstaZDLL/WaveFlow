import { invoke } from "@tauri-apps/api/core";

/**
 * Read the stored Last.fm API key from `app_setting`. Returns `null`
 * when the user has never configured one (or cleared it).
 */
export function getLastfmApiKey(): Promise<string | null> {
  return invoke<string | null>("get_lastfm_api_key");
}

/**
 * Upsert the Last.fm API key. Passing an empty string removes the
 * stored value so the backend treats it as "not configured".
 */
export function setLastfmApiKey(apiKey: string): Promise<void> {
  return invoke<void>("set_lastfm_api_key", { apiKey });
}

export function getLastfmApiSecret(): Promise<string | null> {
  return invoke<string | null>("get_lastfm_api_secret");
}

export function setLastfmApiSecret(apiSecret: string): Promise<void> {
  return invoke<void>("set_lastfm_api_secret", { apiSecret });
}

/**
 * Snapshot of the Last.fm linkage state for the active profile.
 * `configured` answers "is the API key + secret pair stored?" and
 * `connected` answers "did we successfully exchange a session key?".
 */
export interface LastfmStatus {
  configured: boolean;
  connected: boolean;
  username: string | null;
}

export function lastfmGetStatus(): Promise<LastfmStatus> {
  return invoke<LastfmStatus>("lastfm_get_status");
}

/**
 * Trade username + password for a long-lived session key via Last.fm's
 * `auth.getMobileSession`. The password is sent over HTTPS, signed
 * with the shared secret, and never persisted; only the resulting
 * session key lives in the per-profile `auth_credential` table.
 */
export function lastfmLogin(
  username: string,
  password: string,
): Promise<LastfmStatus> {
  return invoke<LastfmStatus>("lastfm_login", { username, password });
}

export function lastfmLogout(): Promise<void> {
  return invoke<void>("lastfm_logout");
}
