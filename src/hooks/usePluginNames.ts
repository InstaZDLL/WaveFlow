import { useEffect, useRef, useState } from "react";

import { listInstalledPlugins } from "../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "./usePluginAvailability";

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
 * Refreshed on the same bus as [`usePluginAvailability`]: installing or
 * uninstalling a plugin changes this map, and the Settings panel
 * already announces that.
 *
 * Returns an empty map until the first listing lands, and on failure.
 * Callers fall back to the id, which is what they showed before — a
 * name that is merely ugly beats a badge that is empty.
 */
export function usePluginNames(): Record<string, string> {
  const [names, setNames] = useState<Record<string, string>>({});
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
        (plugins) => {
          if (cancelled || token !== reqRef.current) return;
          const next: Record<string, string> = {};
          for (const plugin of plugins) {
            // A manifest could in principle carry a blank name; the id
            // is a better badge than an empty one.
            const name = plugin.name?.trim();
            if (name) next[plugin.id] = name;
          }
          setNames(next);
        },
        () => {
          if (cancelled || token !== reqRef.current) return;
          setNames({});
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

  return names;
}
