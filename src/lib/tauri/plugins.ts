import { invoke } from "@tauri-apps/api/core";

import type { LocalizedText } from "../localizedText";

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
  /** Manifest-authored: a plain string, or a `{ lang: text }` map.
   *  Resolve with `useLocalizedText()` before rendering. */
  description: LocalizedText | null;
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
  /** `true` when the manifest declares `[[options]]` — the ⚙️ gear +
   *  options panel are shown for these. */
  hasOptions: boolean;
}

export interface PluginPermissionsInfo {
  /** HTTP allowlist patterns, e.g. `https://*.radio-browser.info/**`. */
  http: string[];
  storageRead: boolean;
  storageState: boolean;
  /** `ui`-world redacted artist read (`library.read_artists`) — names +
   *  aggregate counts + opaque ids only, no file paths. */
  libraryReadArtists: boolean;
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

// ----- plugin store / marketplace (Phase 2) ------------------------------
//
// The curated registry (InstaZDLL/waveflow-plugins) surfaced in-app.
// Mirrors `commands::plugin_store::MarketplaceEntry` (camelCase). The
// backend fetches the catalogue (app endpoint → raw GitHub → jsDelivr
// fallbacks), verifies each download's blake3 against the registry, and
// stage-swaps the plugin into the sideload root for the runtime to load.

/** One store row: registry fields + install/update/compat state
 *  resolved against what's on disk and this build's version. */
export interface MarketplaceEntry {
  id: string;
  name: string;
  /** Registry-authored: a plain string, or a `{ lang: text }` map.
   *  Resolve with `useLocalizedText()` before rendering. */
  description: LocalizedText;
  author: string;
  /** GitHub `owner/name` hosting the plugin's releases. */
  repo: string;
  homepage: string | null;
  /** WIT world, e.g. `waveflow:source@1.0.0`. */
  world: string;
  /** Registry-pinned version (what installing would land). */
  version: string;
  /** Allowlisted outbound hosts — shown before install, enforced at runtime. */
  http: string[];
  storageRead: boolean;
  storageState: boolean;
  tags: string[];
  /** First-party plugin maintained by the WaveFlow team. */
  official: boolean;
  installed: boolean;
  /** Version on disk (`null` when not installed). */
  installedVersion: string | null;
  /** Registry version differs from the installed one. */
  updateAvailable: boolean;
  /** This build satisfies the entry's `min_app_version`. */
  compatible: boolean;
}

/**
 * Fetch the curated catalogue, resolved against locally-installed
 * plugins. Rejects when offline mode is on or no registry source is
 * reachable — the caller surfaces the error rather than an empty store.
 */
export async function listPluginMarketplace(): Promise<MarketplaceEntry[]> {
  return invoke<MarketplaceEntry[]>("list_plugin_marketplace");
}

/**
 * Install (or update — same path, overwrites) a plugin by id. Downloads
 * the registry-pinned release, verifies its blake3, and hot-loads it.
 * Throws on a hash/manifest mismatch, an incompatible app version, or a
 * bundled id. Re-fetch {@link listInstalledPlugins} after resolve.
 */
export async function installPluginFromRegistry(
  pluginId: string,
): Promise<void> {
  return invoke<void>("install_plugin_from_registry", { pluginId });
}

// ----- animated album artwork (Phase 3) ----------------------------------
//
// Motion covers (Apple Music-style looping video) resolved via enabled
// `waveflow:metadata` plugins. Mirrors `commands::motion_artwork::MotionArtwork`.

/** A resolved motion cover for an album. `squareUrl` / `tallUrl` are
 *  directly-playable video URLs (mp4 for cross-webview compatibility —
 *  the desktop webview has no HLS.js). */
export interface MotionArtwork {
  squareUrl: string;
  tallUrl: string | null;
  /** Which plugin produced it. */
  pluginId: string;
}

/**
 * Ask enabled metadata plugins for an album's motion artwork. Resolves
 * `null` when offline, when no metadata plugin is installed, or when none
 * has motion for this album — callers fall back to the static cover.
 *
 * `albumId`, when given, is checked first against a manual override
 * (issue #408) — see `setAlbumMotionArtworkFromFile` — before any plugin
 * runs. Omit it for surfaces with no album row (e.g. Web Radio).
 */
export async function fetchAlbumMotionArtwork(
  artist: string,
  album: string,
  albumId?: number | null,
): Promise<MotionArtwork | null> {
  return invoke<MotionArtwork | null>("fetch_album_motion_artwork", {
    artist,
    album,
    albumId: albumId ?? null,
  });
}

/**
 * Set `albumId`'s animated cover from a local mp4 file (issue #408). Wins
 * over any plugin-resolved motion cover and survives plugin re-fetches —
 * same precedence rule as a manual static cover. Rejects non-mp4 files
 * (validated by magic bytes) and files above the backend's size cap.
 */
export function setAlbumMotionArtworkFromFile(
  albumId: number,
  filePath: string,
): Promise<void> {
  return invoke<void>("set_album_motion_artwork_from_file", {
    albumId,
    filePath,
  });
}

/**
 * Clear `albumId`'s manual motion cover, if any — the next lookup falls
 * back to the plugin result. A no-op when there was nothing to clear.
 */
export function clearAlbumMotionArtwork(albumId: number): Promise<void> {
  return invoke<void>("clear_album_motion_artwork", { albumId });
}

/**
 * Opt-in local motion-artwork cache state (Phase 1). When `enabled`, the
 * backend downloads each resolved mp4 into an app-wide LRU cache (1 GB) under
 * `<app-data>/waveflow/motion_cache/` and serves the local copy — offline and
 * with no re-download on the next play. Mirrors
 * `commands::motion_artwork::MotionCacheInfo` (camelCase).
 */
export interface MotionCacheInfo {
  enabled: boolean;
  sizeBytes: number;
  fileCount: number;
}

/** Read the cache toggle + current on-disk footprint. */
export async function getMotionCacheInfo(): Promise<MotionCacheInfo> {
  return invoke<MotionCacheInfo>("get_motion_cache_info");
}

/** Toggle the local cache. Turning it off does NOT purge existing files. */
export async function setMotionCacheEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_motion_cache_enabled", { enabled });
}

/** Delete every cached motion mp4. */
export async function clearMotionCache(): Promise<void> {
  return invoke<void>("clear_motion_cache");
}

/**
 * Metadata-world plugins can supply motion covers, so they get the host's
 * local motion-cache option (and thus the ⚙️ options panel). Shared predicate
 * so the gear-visibility check (PluginsCard) and the panel body (PluginOptions)
 * never drift apart. Widen this when manifest-declared plugin options land.
 */
export function isMetadataPlugin(plugin: Pick<PluginInfo, "world">): boolean {
  return plugin.world.startsWith("waveflow:metadata");
}

// ----- manifest-declared plugin options (Phase 2) ------------------------
//
// A plugin declares `[[options]]` in its manifest; the user sets them in the
// ⚙️ panel and the values reach the guest via `waveflow:host/config`. Mirrors
// `commands::plugins::PluginOption` (camelCase).

/** One configurable option: manifest declaration + current stored value. */
export interface PluginOption {
  key: string;
  /** Control type: `"bool"` | `"enum"` | `"text"`. */
  type: string;
  /** Manifest-authored: a plain string, or a `{ lang: text }` map.
   *  Resolve with `useLocalizedText()` before rendering. */
  label: LocalizedText;
  /** Manifest default (string form); `null` = none. */
  default: string | null;
  /** Allowed values for an `enum` option. */
  choices: string[];
  /** Manifest-authored: a plain string, or a `{ lang: text }` map. */
  description: LocalizedText | null;
  /** Current stored value; `null` = unset (the plugin uses `default`). */
  value: string | null;
}

/** List a plugin's declared options merged with the user's current values. */
export async function getPluginOptions(
  pluginId: string,
): Promise<PluginOption[]> {
  return invoke<PluginOption[]>("get_plugin_options", { pluginId });
}

/** Set (or reset with `value = null`) one option. Validated backend-side. */
export async function setPluginOption(
  pluginId: string,
  key: string,
  value: string | null,
): Promise<void> {
  return invoke<void>("set_plugin_option", { pluginId, key, value });
}

// ----- waveflow:ui/v1 view descriptor + invocation (Phase 5) --------------
//
// A ui-world plugin renders a custom view WITHOUT shipping React: the
// guest returns a JSON *view descriptor* (this shape) that the host
// draws with native components. `render` / `event` return the RAW
// descriptor string; `parsePluginUiDescriptor` validates schemaVersion
// + parses. The security boundary is that the guest never sends
// HTML/CSS/JS — only this declarative tree.

/** A user action on a descriptor. `kind: "event"` round-trips through
 *  the plugin (→ next descriptor); `kind: "open-url"` is handled host-
 *  side (opens `url` in the OS browser) and never reaches the guest. */
export interface PluginUiAction {
  kind: "event" | "open-url";
  label: string;
  /** Set when `kind === "event"`: opaque event name echoed to the plugin. */
  event?: string | null;
  /** Set when `kind === "event"`: opaque payload echoed to the plugin. */
  payload?: string | null;
  /** Set when `kind === "open-url"`: the external URL to open. */
  url?: string | null;
}

/** One card in a section. All display fields but `id`/`title` are
 *  optional so a plugin can render a lean list or a rich one. */
export interface PluginUiItem {
  id: string;
  title: string;
  subtitle?: string;
  /** Right-aligned detail, e.g. a release date. */
  detail?: string;
  /** Remote artwork URL, used directly as an `<img src>`. */
  imageUrl?: string | null;
  /** Pill chips, e.g. `["Album"]`. */
  badges?: string[];
  /** Per-item action buttons. */
  actions?: PluginUiAction[];
}

export interface PluginUiSection {
  /** Section heading; rendered when non-empty. */
  title?: string;
  items: PluginUiItem[];
}

/** The full view a plugin returns from `render` / `on-event`. */
export interface PluginUiDescriptor {
  /** Descriptor schema version. Only `1` is understood today. */
  schemaVersion: number;
  title: string;
  subtitle?: string;
  /** Freeform status hint, e.g. `"fresh"` | `"cached"` | `"error"`. */
  status?: string;
  /** Epoch ms of the plugin's last refresh, if it tracks one. */
  lastUpdatedAt?: number | null;
  /** Header-level action buttons. */
  actions?: PluginUiAction[];
  sections?: PluginUiSection[];
  /** Shown when there are no sections/items. */
  emptyTitle?: string;
  emptyHint?: string;
}

/** One enabled ui-world plugin + its sidebar registration. Mirrors
 *  `commands::plugins::PluginUiRegistration`. */
export interface PluginUiRegistration {
  pluginId: string;
  mountPoint: {
    sidebarLabel: string;
    /** lucide-react icon name (e.g. `"radar"`); resolved against the
     *  host's curated set, falling back to a generic glyph. */
    sidebarIcon: string | null;
    initialPath: string;
  };
}

/** Enumerate enabled ui-world plugins + their sidebar mount points.
 *  A plugin whose `manifest()` traps is skipped backend-side. */
export async function listUiPlugins(): Promise<PluginUiRegistration[]> {
  return invoke<PluginUiRegistration[]>("list_ui_plugins");
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** A field that's optional in the descriptor but rendered as text when
 *  present must be a string — a non-string (object) would make React
 *  throw "Objects are not valid as a React child". `undefined` / `null`
 *  are treated as absent. */
function assertOptionalString(v: unknown, where: string): void {
  if (v !== undefined && v !== null && typeof v !== "string") {
    throw new Error(`${where} must be a string`);
  }
}

/** Validate one action: a known `kind` discriminator + the field that
 *  kind requires (a `label`, plus `url` for open-url / `event` for
 *  event). An unknown kind or a missing required field is rejected. */
function validateUiAction(a: unknown, where: string): void {
  if (!isRecord(a)) throw new Error(`${where}: action must be an object`);
  if (a.kind !== "event" && a.kind !== "open-url") {
    throw new Error(`${where}: unknown action kind ${JSON.stringify(a.kind)}`);
  }
  if (typeof a.label !== "string") {
    throw new Error(`${where}: action.label must be a string`);
  }
  if (a.kind === "open-url" && typeof a.url !== "string") {
    throw new Error(`${where}: open-url action requires a url string`);
  }
  if (a.kind === "event" && typeof a.event !== "string") {
    throw new Error(`${where}: event action requires an event string`);
  }
}

/** Validate one item: required `id` + `title`, and — where present —
 *  `badges` / `actions` must be arrays (the renderer `.map`s them, so a
 *  non-array would throw mid-render). */
function validateUiItem(item: unknown, where: string): void {
  if (!isRecord(item)) throw new Error(`${where}: item must be an object`);
  if (typeof item.id !== "string") {
    throw new Error(`${where}: item.id must be a string`);
  }
  if (typeof item.title !== "string") {
    throw new Error(`${where}: item.title must be a string`);
  }
  assertOptionalString(item.subtitle, `${where}: item.subtitle`);
  assertOptionalString(item.detail, `${where}: item.detail`);
  if (item.badges !== undefined) {
    if (!Array.isArray(item.badges)) {
      throw new Error(`${where}: item.badges must be an array`);
    }
    item.badges.forEach((b, i) =>
      assertOptionalString(b, `${where}: item.badges[${i}]`),
    );
  }
  if (item.actions !== undefined) {
    if (!Array.isArray(item.actions)) {
      throw new Error(`${where}: item.actions must be an array`);
    }
    item.actions.forEach((a, i) =>
      validateUiAction(a, `${where}.actions[${i}]`),
    );
  }
}

/** Parse + validate a raw descriptor string from the backend. Beyond
 *  the `schemaVersion` gate it walks the whole tree — sections, items,
 *  actions, and their discriminators — and throws on the first
 *  malformed node, so `PluginUIView` never receives a structure that
 *  could throw during render (a non-array `sections`/`items`/`badges`
 *  would break `.map`, an unknown action `kind` would misroute). The
 *  caller surfaces the throw as an error banner. */
export function parsePluginUiDescriptor(raw: string): PluginUiDescriptor {
  const parsed: unknown = JSON.parse(raw);
  if (!isRecord(parsed)) {
    throw new Error("plugin view descriptor must be an object");
  }
  if (parsed.schemaVersion !== 1) {
    throw new Error(
      `unsupported plugin view schemaVersion: ${JSON.stringify(parsed.schemaVersion)}`,
    );
  }
  if (typeof parsed.title !== "string") {
    throw new Error("plugin view descriptor: title must be a string");
  }
  assertOptionalString(parsed.subtitle, "plugin view descriptor: subtitle");
  assertOptionalString(parsed.emptyTitle, "plugin view descriptor: emptyTitle");
  assertOptionalString(parsed.emptyHint, "plugin view descriptor: emptyHint");
  if (parsed.actions !== undefined) {
    if (!Array.isArray(parsed.actions)) {
      throw new Error("plugin view descriptor: actions must be an array");
    }
    parsed.actions.forEach((a, i) => validateUiAction(a, `actions[${i}]`));
  }
  if (parsed.sections !== undefined) {
    if (!Array.isArray(parsed.sections)) {
      throw new Error("plugin view descriptor: sections must be an array");
    }
    parsed.sections.forEach((section, si) => {
      if (!isRecord(section)) {
        throw new Error(`sections[${si}]: must be an object`);
      }
      assertOptionalString(section.title, `sections[${si}].title`);
      if (!Array.isArray(section.items)) {
        throw new Error(`sections[${si}]: items must be an array`);
      }
      section.items.forEach((item, ii) =>
        validateUiItem(item, `sections[${si}].items[${ii}]`),
      );
    });
  }
  return parsed as unknown as PluginUiDescriptor;
}

/** Render a ui plugin's view for an internal `path`. */
export async function pluginUiRender(
  pluginId: string,
  path: string,
): Promise<PluginUiDescriptor> {
  const raw = await invoke<string>("plugin_ui_render", { pluginId, path });
  return parsePluginUiDescriptor(raw);
}

/** Round-trip a user action; returns the next descriptor. */
export async function pluginUiEvent(
  pluginId: string,
  event: string,
  payload: string,
): Promise<PluginUiDescriptor> {
  const raw = await invoke<string>("plugin_ui_event", {
    pluginId,
    event,
    payload,
  });
  return parsePluginUiDescriptor(raw);
}

/** `true` for a ui-world plugin — the gear/options predicate + any
 *  ui-specific affordances key off this, mirroring {@link isMetadataPlugin}. */
export function isUiPlugin(plugin: Pick<PluginInfo, "world">): boolean {
  return plugin.world.startsWith("waveflow:ui");
}
