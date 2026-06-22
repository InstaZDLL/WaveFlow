import { invoke } from "@tauri-apps/api/core";

/**
 * Release channel the in-app updater follows. `beta` opts the user
 * into pre-release builds served from the rolling `beta-channel`
 * manifest; `stable` (default) tracks `/releases/latest`. Persisted
 * app-wide in `app_setting['updater.channel']`.
 */
export type UpdateChannel = "stable" | "beta";

/** Metadata for an available update (returned by `check_for_update`). */
export interface UpdateInfo {
  version: string;
  notes: string | null;
}

/** Download progress payload emitted on the `updater:progress` event. */
export interface UpdateProgress {
  downloaded: number;
  /** `0` until the server reports a Content-Length. */
  total: number;
}

/** Tauri event name carrying {@link UpdateProgress} during install. */
export const UPDATER_PROGRESS_EVENT = "updater:progress";

/**
 * Window (DOM) event the Settings beta toggle fires after switching
 * channels so the single mounted `useUpdater` (in the UpdateBanner)
 * re-checks against the new endpoint without a relaunch.
 */
export const UPDATER_RECHECK_EVENT = "waveflow:updater-recheck";

export function getUpdateChannel(): Promise<UpdateChannel> {
  return invoke<UpdateChannel>("get_update_channel");
}

export function setUpdateChannel(channel: UpdateChannel): Promise<void> {
  return invoke<void>("set_update_channel", { channel });
}

/**
 * Ask the backend to query the channel-appropriate manifest. Resolves
 * to the update metadata when a newer build exists, or `null` when up
 * to date / the updater is unavailable (dev, app-store builds). The
 * verified `Update` object stays in Rust until {@link installUpdate}.
 */
export function checkForUpdate(): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>("check_for_update");
}

/**
 * Download + install the update found by the last {@link checkForUpdate}.
 * Progress arrives via {@link UPDATER_PROGRESS_EVENT}; on Windows the
 * installer launches and the app exits before this resolves.
 */
export function installUpdate(): Promise<void> {
  return invoke<void>("install_update");
}
