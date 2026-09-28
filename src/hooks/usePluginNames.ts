import { useEffect, useMemo, useRef, useState } from "react";

import { listInstalledPlugins, type PluginInfo } from "../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "./usePluginAvailability";

/**
 * The installed plugins, as `list_installed_plugins` returns them.
 *
 * Refreshed on the same bus as [`usePluginAvailability`]: installing,
 * enabling or uninstalling a plugin changes this list, and the Settings
 * panel already announces that. Empty until the first listing lands,
 * and on failure.
 */
function useInstalledPlugins(): PluginInfo[] {
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  // Per-refresh token. Two events in quick succession start two
  // listings, and the slower one must not overwrite the newer result —
  // the same race `usePluginAvailability` documents, for the same
  // reason.
  const reqRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      const token = ++reqRef.current;
      listInstalledPlugins().then(
        (list) => {
          if (cancelled || token !== reqRef.current) return;
          setPlugins(list);
        },
        () => {
          if (cancelled || token !== reqRef.current) return;
          setPlugins([]);
        },
      );
    };

    refresh();
    window.addEventListener(PLUGIN_AVAILABILITY_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(PLUGIN_AVAILABILITY_EVENT, refresh);
    };
  }, []);

  return plugins;
}

/**
 * Installed plugin ids mapped to the display name their manifest
 * declares — `"apple-lyrics"` → `"Apple Music Lyrics"`.
 *
 * A plugin id is constrained to `[a-z0-9-]+` because it is also a
 * directory name, a log scope and a storage key, so it is never
 * something to show a user. The manifest's `name` is, and the host
 * already returns both from `list_installed_plugins` — nothing new is
 * fetched here, the pairing was simply not being made anywhere.
 *
 * Empty until the first listing lands, and on failure. Callers fall
 * back to the id, which is what they showed before — a name that is
 * merely ugly beats a badge that is empty.
 */
export function usePluginNames(): Record<string, string> {
  const plugins = useInstalledPlugins();
  return useMemo(() => {
    const names: Record<string, string> = {};
    for (const plugin of plugins) {
      // A manifest could in principle carry a blank name; the id is a
      // better badge than an empty one.
      const name = plugin.name?.trim();
      if (name) names[plugin.id] = name;
    }
    return names;
  }, [plugins]);
}

/** The worlds whose plugins the host asks for lyrics. */
const LYRICS_WORLDS = new Set(["waveflow:metadata/v2", "waveflow:metadata/v3"]);

/**
 * The enabled plugins the lyrics waterfall asks, in listing order, for
 * the source picker. Without them there, a plugin was reached only by
 * the automatic waterfall, and a user had no way to ask it about one
 * track — or to tell whether it answers at all.
 *
 * Keyed on the world, the same test the backend enumerates with, so the
 * picker offers exactly the plugins `refetch_lyrics` will run. A plugin
 * of those worlds that serves only animated covers is offered too and
 * simply finds nothing, which is what picking it then says.
 */
export function useLyricsPlugins(): Array<{ id: string; name: string }> {
  const plugins = useInstalledPlugins();
  return useMemo(
    () =>
      plugins
        .filter((plugin) => plugin.enabled && LYRICS_WORLDS.has(plugin.world))
        .map((plugin) => ({
          id: plugin.id,
          name: plugin.name?.trim() || plugin.id,
        })),
    [plugins],
  );
}
