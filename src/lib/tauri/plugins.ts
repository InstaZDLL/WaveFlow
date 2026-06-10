import { invoke } from "@tauri-apps/api/core";

/**
 * Plugin information exposed by the Plugin SDK backend (Phase 3.1).
 *
 * Mirrors `commands::plugins::PluginInfo` from the Rust side —
 * the backend uses `#[serde(rename_all = "camelCase")]` so the
 * shape matches verbatim. Fields are read-only from the frontend;
 * mutations go through `setPluginEnabled` / `uninstallPlugin`.
 */
export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  author: string;
  /** Manifest world label — one of `waveflow:source/v1`,
   *  `waveflow:metadata/v1`, `waveflow:ui/v1`. */
  world: string;
  description: string | null;
  homepage: string | null;
  license: string | null;
  permissions: PluginPermissionsInfo;
  assets: PluginAssetInfo[];
  /** Resolved from `app_setting['plugin.<id>.enabled']`. Defaults
   *  to `true` for a freshly-installed plugin (no setting row). */
  enabled: boolean;
}

export interface PluginPermissionsInfo {
  /** HTTP allowlist patterns, e.g. `https://*.radio-browser.info/**`. */
  http: string[];
  storageRead: boolean;
  storageState: boolean;
}

export interface PluginAssetInfo {
  filename: string;
  description: string | null;
}

/**
 * List every plugin installed under `<app-data>/waveflow/plugins/`.
 * Subdirectories with a missing or malformed manifest are silently
 * skipped backend-side, so the UI only sees plugins the runtime
 * could actually load.
 */
export async function listInstalledPlugins(): Promise<PluginInfo[]> {
  return invoke<PluginInfo[]>("list_installed_plugins");
}

/**
 * Single-row fetch. Returns `null` when the id doesn't resolve to
 * a valid plugin (the install dir is gone, the manifest doesn't
 * parse, or the manifest's declared id doesn't match the param).
 */
export async function getPluginInfo(
  pluginId: string,
): Promise<PluginInfo | null> {
  return invoke<PluginInfo | null>("get_plugin_info", { pluginId });
}

/**
 * Toggle the per-plugin enabled flag. Backend refuses to write
 * the setting if the plugin isn't actually installed (missing or
 * mismatched manifest), so the UI doesn't need to pre-check.
 */
export async function setPluginEnabled(
  pluginId: string,
  enabled: boolean,
): Promise<void> {
  return invoke<void>("set_plugin_enabled", { pluginId, enabled });
}

/**
 * Remove the plugin's install dir + scratch dir + the
 * `app_setting` enabled row. UI MUST confirm with the user before
 * calling — the command itself takes no "are you sure" parameter,
 * same convention as `deleteProfile`.
 */
export async function uninstallPlugin(pluginId: string): Promise<void> {
  return invoke<void>("uninstall_plugin", { pluginId });
}
