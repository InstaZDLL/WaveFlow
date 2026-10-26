import { useTranslation } from "react-i18next";

import { pluginWorldLabelKey } from "../../../lib/pluginWorlds";

/** The world chip shown next to a plugin's name, in the installed list
 *  ({@link PluginsCard}) and in the store ({@link PluginStoreCard}). */
export function PluginWorldBadge({ world }: { world: string }) {
  const { t } = useTranslation();
  const key = pluginWorldLabelKey(world);
  const label = key ? t(key) : world;
  return (
    <span className="text-[10px] uppercase tracking-wide font-medium px-1.5 py-0.5 rounded bg-emerald-50 text-emerald-700 dark:bg-emerald-950/30 dark:text-emerald-400">
      {label}
    </span>
  );
}
