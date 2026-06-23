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

/** Artist-bio provider: Last.fm (English, needs a key) or TheAudioDB
 *  (multi-language, no key). Persisted app-wide. */
export type BioSource = "lastfm" | "theaudiodb";

/** TheAudioDB biography languages exposed in Settings (the client
 *  falls back to English for any unmapped UI locale). */
export const BIO_LANGUAGES = [
  "en",
  "fr",
  "de",
  "es",
  "it",
  "pt",
  "nl",
  "ru",
  "ja",
  "zh",
] as const;
export type BioLanguage = (typeof BIO_LANGUAGES)[number];

export function getBioSource(): Promise<BioSource> {
  return invoke<BioSource>("get_bio_source");
}

export function setBioSource(source: BioSource): Promise<void> {
  return invoke<void>("set_bio_source", { source });
}

export function getBioLanguage(): Promise<BioLanguage> {
  // The backend clamps to BIO_LANGUAGES (or "en"), so the value is
  // always a valid BioLanguage at this boundary.
  return invoke<BioLanguage>("get_bio_language");
}

export function setBioLanguage(language: BioLanguage): Promise<void> {
  return invoke<void>("set_bio_language", { language });
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

/**
 * Read the Discord Rich Presence opt-in flag. Returns `false` (off)
 * when the user has never enabled it.
 */
export function getDiscordRpcEnabled(): Promise<boolean> {
  return invoke<boolean>("get_discord_rpc_enabled");
}

/**
 * Flip the Discord Rich Presence flag. The backend updates its
 * persisted setting and notifies the running presence worker so the
 * Discord activity card appears or disappears immediately.
 */
export function setDiscordRpcEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_discord_rpc_enabled", { enabled });
}

/**
 * Read the native track-change notification opt-in flag. Returns
 * `false` (off) when never enabled — toast notifications are
 * intrusive and we require explicit opt-in.
 */
export function getNotificationsTrackChange(): Promise<boolean> {
  return invoke<boolean>("get_notifications_track_change");
}

/**
 * Toggle native track-change toasts. Takes effect on the **next**
 * track change; no toast fires for the currently playing track to
 * avoid spamming the user right after the flip.
 */
export function setNotificationsTrackChange(enabled: boolean): Promise<void> {
  return invoke<void>("set_notifications_track_change", { enabled });
}
