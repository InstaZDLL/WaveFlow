import { useEffect, useRef, useState } from "react";

import { listInstalledPlugins } from "../lib/tauri/plugins";

/// DOM event the Settings → Plugins UI dispatches whenever the user
/// flips a plugin's enabled toggle or uninstalls a plugin. Hooks
/// listening on this event re-fetch their availability snapshot —
/// same lightweight bus pattern the Sidebar uses for the Spotify
/// visibility toggle.
export const PLUGIN_AVAILABILITY_EVENT = "waveflow:plugin-availability-changed";

/// Resolve once at mount + every time someone dispatches
/// [`PLUGIN_AVAILABILITY_EVENT`]. Returns `true` only when the
/// plugin is installed AND its enabled flag is on. A missing or
/// disabled plugin yields `false`; a backend error during the
/// `list_installed_plugins` call also yields `false` (we'd rather
/// hide a feature than crash the layout).
///
/// The Settings → Plugins panel is the only writer that fires the
/// event so consumers don't poll. WebRadioView + Sidebar both
/// consume this hook to gate their Web Radio surface.
export function usePluginAvailability(pluginId: string): boolean {
  const [available, setAvailable] = useState(false);
  // Per-effect token counter. A rapid disable → enable sequence
  // dispatches two events and starts two `listInstalledPlugins`
  // calls in parallel; the slower one would otherwise resolve last
  // and overwrite the fresh value with stale data. Each refresh
  // captures its token, the `.then` continuation bails when the
  // ref has moved on. The existing `cancelled` flag still covers
  // unmount, but it can't distinguish two in-flight refreshes
  // inside the same effect lifetime — hence the token.
  const reqRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      const token = ++reqRef.current;
      listInstalledPlugins().then(
        (plugins) => {
          if (cancelled || token !== reqRef.current) return;
          setAvailable(plugins.some((p) => p.id === pluginId && p.enabled));
        },
        () => {
          if (cancelled || token !== reqRef.current) return;
          setAvailable(false);
        },
      );
    };
    refresh();
    window.addEventListener(PLUGIN_AVAILABILITY_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(PLUGIN_AVAILABILITY_EVENT, refresh);
    };
  }, [pluginId]);

  return available;
}
