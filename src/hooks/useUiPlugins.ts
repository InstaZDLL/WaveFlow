import { useEffect, useRef, useState } from "react";

import {
  listUiPlugins,
  type PluginUiRegistration,
} from "../lib/tauri/plugins";
import { PLUGIN_AVAILABILITY_EVENT } from "./usePluginAvailability";

/**
 * Enumerate the enabled `ui`-world plugins + their sidebar mount
 * points, so the Sidebar can build one navigation entry per plugin
 * dynamically (label + icon + landing path all come from the plugin's
 * `manifest()`, not a hardcoded table).
 *
 * Re-fetches at mount + every time the Settings → Plugins panel fires
 * [`PLUGIN_AVAILABILITY_EVENT`] (enable / disable / uninstall), same
 * bus + token-guard pattern as {@link usePluginAvailability}: a rapid
 * toggle sequence starts two `list_ui_plugins` calls, and the token
 * makes the slower one bail instead of clobbering the fresh list. A
 * backend error yields an empty list — we'd rather show no plugin
 * entries than crash the sidebar.
 */
export function useUiPlugins(): PluginUiRegistration[] {
  const [plugins, setPlugins] = useState<PluginUiRegistration[]>([]);
  const reqRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      const token = ++reqRef.current;
      listUiPlugins().then(
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
