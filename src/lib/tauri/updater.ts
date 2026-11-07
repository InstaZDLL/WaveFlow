import { invoke } from "@tauri-apps/api/core";

import { announceSettingWrite } from "../settingWrites";

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

/**
 * Window (DOM) event a check made by hand in Settings fires when it
 * finds a newer build, with the {@link UpdateInfo} as its `detail`, so
 * the UpdateBanner offers it like one it found itself.
 */
export const UPDATER_FOUND_EVENT = "waveflow:updater-found";

/**
 * The error `check_for_update` answers a check asked for by hand while
 * offline mode is on (`OFFLINE_MESSAGE` in `commands/updater.rs`).
 */
export const UPDATER_OFFLINE_ERROR = "updater: offline mode is on";

/**
 * The error a build without the updater (dev, package-manager builds)
 * answers a check asked for by hand, and any install
 * (`UNAVAILABLE_MESSAGE` in `commands/updater.rs`).
 */
export const UPDATER_UNAVAILABLE_ERROR = "updater: unavailable in this build";

export function getUpdateChannel(): Promise<UpdateChannel> {
  return invoke<UpdateChannel>("get_update_channel");
}

export function setUpdateChannel(channel: UpdateChannel): Promise<void> {
  return announceSettingWrite(invoke<void>("set_update_channel", { channel }));
}

/** Whether the updater looks for a new version on its own, at launch and
 *  after a channel switch. App-wide, `app_setting['updater.auto_check']`. */
export function getUpdateAutoCheck(): Promise<boolean> {
  return invoke<boolean>("get_update_auto_check");
}

export function setUpdateAutoCheck(enabled: boolean): Promise<void> {
  return announceSettingWrite(
    invoke<void>("set_update_auto_check", { enabled }),
  );
}

/**
 * Ask the backend to query the channel-appropriate manifest. Resolves
 * to the update metadata when a newer build exists, or `null` when up
 * to date / the updater is unavailable (dev, app-store builds). The
 * verified `Update` object stays in Rust until {@link installUpdate}.
 *
 * `automatic` marks a check nobody asked for (launch, channel switch):
 * the backend answers `null` without a request when automatic checks
 * are off or offline mode is on. A check asked for by hand
 * (`automatic: false`) rejects with {@link UPDATER_OFFLINE_ERROR} in
 * offline mode, and with {@link UPDATER_UNAVAILABLE_ERROR} in a build
 * without the updater.
 */
export function checkForUpdate(options: {
  automatic: boolean;
}): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>("check_for_update", {
    automatic: options.automatic,
  });
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Whether a rejected {@link checkForUpdate} was refused for offline mode. */
export function isUpdaterOfflineError(err: unknown): boolean {
  return errorText(err) === UPDATER_OFFLINE_ERROR;
}

/** Whether a rejected {@link checkForUpdate} came from a build that has no
 *  updater, where nothing could be checked. */
export function isUpdaterUnavailableError(err: unknown): boolean {
  return errorText(err) === UPDATER_UNAVAILABLE_ERROR;
}

/**
 * Download + install the update found by the last {@link checkForUpdate}.
 * Progress arrives via {@link UPDATER_PROGRESS_EVENT}; on Windows the
 * installer launches and the app exits before this resolves.
 */
export function installUpdate(): Promise<void> {
  return invoke<void>("install_update");
}
