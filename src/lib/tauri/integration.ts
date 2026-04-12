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
