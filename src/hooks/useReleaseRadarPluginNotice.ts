import { useCallback, useEffect, useState } from "react";

import { listInstalledPlugins } from "../lib/tauri/plugins";
import { RELEASE_RADAR_PLUGIN_ID } from "../lib/tauri/releaseRadar";
import { PLUGIN_AVAILABILITY_EVENT } from "./usePluginAvailability";
import { useProfileBooleanSetting } from "./useProfileSetting";

const KEY = "release_radar.plugin_notice_seen";

/** Broadcast after a successful write so a second mounted view re-reads. */
export const RELEASE_RADAR_PLUGIN_NOTICE_EVENT =
  "waveflow:release-radar-plugin-notice";

/** Not seen until it has been shown once. */
const DEFAULT_SEEN = false;

export interface ReleaseRadarPluginNotice {
  /** Draw the notice: the plugin is installed (enabled or not) and the
   *  notice is the one this profile has not seen before. */
  visible: boolean;
  dismiss: () => void;
}

/**
 * The notice that the Release Radar plugin is no longer needed, shown once
 * per profile to a user who still has it installed.
 *
 * "Once" is counted when it is shown, not when it is dismissed: the flag
 * is written as soon as the notice is drawn, and the notice stays up for
 * the rest of that visit, until it is closed or the plugin is uninstalled.
 * A notice that waited for a click would come back every visit for a user
 * who never closed it.
 *
 * Not a preference, so not in the settings registry: it is state the view
 * keeps, as the artist page's collapsed biography, and Settings neither
 * shows it nor offers to reset it.
 */
export function useReleaseRadarPluginNotice(): ReleaseRadarPluginNotice {
  const {
    value: seen,
    ready,
    setValue: setSeen,
  } = useProfileBooleanSetting({
    key: KEY,
    defaultValue: DEFAULT_SEEN,
    event: RELEASE_RADAR_PLUGIN_NOTICE_EVENT,
    label: "useReleaseRadarPluginNotice",
  });
  const installed = usePluginInstalled(RELEASE_RADAR_PLUGIN_ID);

  // Kept once raised, so the write below does not take the notice down
  // the moment it lands. Lowered while a profile is being read, so the
  // next profile decides for itself.
  const due = ready && !seen && installed;
  const [raised, setRaised] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  if (due && !raised) setRaised(true);
  if (!ready && (raised || dismissed)) {
    setRaised(false);
    setDismissed(false);
  }

  useEffect(() => {
    // Fire-and-forget: the shared hook never rejects.
    if (due) void setSeen(true);
  }, [due, setSeen]);

  const dismiss = useCallback(() => setDismissed(true), []);
  return { visible: raised && !dismissed && installed, dismiss };
}

/** Whether the plugin is on disk, enabled or not: a disabled copy is still
 *  one to uninstall. Re-read on every enable / disable / uninstall. */
function usePluginInstalled(pluginId: string): boolean {
  const [installed, setInstalled] = useState(false);
  useEffect(() => {
    let cancelled = false;
    // A token per read, so a slow answer never paints over a newer one.
    let token = 0;
    const refresh = () => {
      const mine = ++token;
      listInstalledPlugins().then(
        (plugins) => {
          if (cancelled || mine !== token) return;
          setInstalled(plugins.some((p) => p.id === pluginId));
        },
        () => {
          if (cancelled || mine !== token) return;
          setInstalled(false);
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
  return installed;
}
