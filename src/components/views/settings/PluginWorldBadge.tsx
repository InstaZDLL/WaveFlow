import { useTranslation } from "react-i18next";

/** WIT world prefix → the `settings.plugins.worlds.*` key naming the
 *  role it plays. The prefixes are disjoint, so order does not matter.
 *  A world with no entry falls back to its raw WIT label — which is
 *  how `waveflow:canvas/v1` reached the screen as `WAVEFLOW:CANVAS/V1`
 *  for as long as the Canvas world has existed, and then again in the
 *  store, whose own copy of this table had been left behind. One table
 *  for both cards, so a new world is labelled everywhere or nowhere. */
const WORLD_LABEL_KEYS: [string, string][] = [
  ["waveflow:source", "source"],
  ["waveflow:metadata", "metadata"],
  ["waveflow:ui", "ui"],
  ["waveflow:canvas", "canvas"],
];

/** The world chip shown next to a plugin's name, in the installed list
 *  ({@link PluginsCard}) and in the store ({@link PluginStoreCard}). */
export function PluginWorldBadge({ world }: { world: string }) {
  const { t } = useTranslation();
  const entry = WORLD_LABEL_KEYS.find(([prefix]) => world.startsWith(prefix));
  const label = entry ? t(`settings.plugins.worlds.${entry[1]}`) : world;
  return (
    <span className="text-[10px] uppercase tracking-wide font-medium px-1.5 py-0.5 rounded bg-emerald-50 text-emerald-700 dark:bg-emerald-950/30 dark:text-emerald-400">
      {label}
    </span>
  );
}
