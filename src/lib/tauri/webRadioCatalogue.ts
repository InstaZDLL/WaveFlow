import { invoke } from "@tauri-apps/api/core";
import type { PluginTrack } from "./plugins";

/**
 * Offline Web Radio catalogue — native counterpart to the `web-radio`
 * plugin. The user downloads the radio-browser station directory into a
 * local FTS-indexed app.db table (Settings → Data); the Web Radio view then
 * browses + searches it without network when offline, or always when
 * `localFirst` is on. `resolveRadioCatalogue` answers the SAME opaque query
 * tokens as `pluginResolve` and returns the SAME `PluginTrack` shape, so the
 * view swaps one call for the other transparently.
 */
export interface RadioCatalogueStatus {
  /** Stations currently stored; 0 when never downloaded. */
  count: number;
  /** Epoch millis of the last successful download, or null. */
  lastSyncedAt: number | null;
  /** Browse + search the local catalogue first even while online. */
  localFirst: boolean;
}

/** Progress event payload for `radio_catalogue:progress`. */
export interface RadioCatalogueProgress {
  phase: "download" | "insert";
  current: number;
  total: number;
}

export function radioCatalogueStatus(): Promise<RadioCatalogueStatus> {
  return invoke<RadioCatalogueStatus>("radio_catalogue_status");
}

export function setRadioCatalogueLocalFirst(enabled: boolean): Promise<void> {
  return invoke<void>("set_radio_catalogue_local_first", { enabled });
}

export function clearRadioCatalogue(): Promise<void> {
  return invoke<void>("clear_radio_catalogue");
}

/** Download the full directory + rebuild the catalogue. Returns the count. */
export function downloadRadioCatalogue(): Promise<number> {
  return invoke<number>("download_radio_catalogue");
}

/**
 * Resolve a plugin query token (`top` / `trending` / `tag:<name>` /
 * `country:<ISO2>` / free text) against the local catalogue. `limit`
 * defaults to 100 backend-side.
 */
export function resolveRadioCatalogue(
  query: string,
  limit?: number,
): Promise<PluginTrack[]> {
  return invoke<PluginTrack[]>("resolve_radio_catalogue", { query, limit });
}
