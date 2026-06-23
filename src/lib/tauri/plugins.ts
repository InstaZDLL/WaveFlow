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
  /** `true` when the plugin ships inside the WaveFlow installer and
   *  is re-seeded at every boot. The Settings UI hides "Uninstall"
   *  for these rows; the backend mirrors this by refusing
   *  `uninstall_plugin` on bundled ids. */
  bundled: boolean;
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

// ----- waveflow:source/provider invocation surface -----------------------
//
// The host's source-v1 binding exposes three exports (list-entries,
// resolve, stream-url) the frontend reaches through these wrappers.
// Each call reloads + reinstantiates the wasm component server-
// side — Phase 5 will cache the instance per plugin id when a real
// perf complaint surfaces.

/** Top-level category the plugin exposes through `list-entries`. */
export interface PluginEntry {
  label: string;
  /** Opaque token the host hands back to `pluginResolve` to ask
   *  for this entry's tracks. Treat as a black box. */
  query: string;
  iconUrl: string | null;
}

/** One playable item the plugin returns from `resolve`. */
export interface PluginTrack {
  id: string;
  title: string;
  artist: string;
  album: string | null;
  /** `0` for live streams (radio); the UI hides the seek bar and
   *  shows "LIVE" in that case. */
  durationMs: number;
  artworkUrl: string | null;
  icyUrl: string | null;
}

/** List the plugin's top-level categories. */
export async function pluginListEntries(
  pluginId: string,
): Promise<PluginEntry[]> {
  return invoke<PluginEntry[]>("plugin_list_entries", { pluginId });
}

/** Resolve a category token (or a free-form search) to tracks. */
export async function pluginResolve(
  pluginId: string,
  query: string,
): Promise<PluginTrack[]> {
  return invoke<PluginTrack[]>("plugin_resolve", { pluginId, query });
}

/** Mint the playable stream URL for one track. */
export async function pluginStreamUrl(
  pluginId: string,
  trackId: string,
): Promise<string> {
  return invoke<string>("plugin_stream_url", { pluginId, trackId });
}

// ----- source plugin favorites -------------------------------------------
//
// Per-profile saved items for a source plugin (issue #289). The
// backend stores the array verbatim in
// `profile_setting['plugin.<id>.favorites']`; the host owns ordering +
// dedup. A favorite carries everything needed to re-render AND replay
// the row offline — `id` is the plugin's playable token (`url:<stream>`
// for Web Radio), so `pluginStreamUrl` resolves it without a network
// hit.

/** One saved station/track for a source plugin. Subset of
 *  {@link PluginTrack} — the fields needed to list + replay it. */
export interface PluginFavorite {
  id: string;
  title: string;
  artist: string;
  album: string | null;
  artworkUrl: string | null;
}

/** Read the active profile's favorites for a plugin (empty when none). */
export async function getPluginFavorites(
  pluginId: string,
): Promise<PluginFavorite[]> {
  return invoke<PluginFavorite[]>("get_plugin_favorites", { pluginId });
}

/** Replace the active profile's favorites for a plugin. */
export async function setPluginFavorites(
  pluginId: string,
  favorites: PluginFavorite[],
): Promise<void> {
  return invoke<void>("set_plugin_favorites", { pluginId, favorites });
}
