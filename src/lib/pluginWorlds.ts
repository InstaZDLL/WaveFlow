/** WIT world prefix → the `settings.plugins.worlds.*` key naming the
 *  role it plays. The prefixes are disjoint, so order does not matter.
 *  A world with no entry falls back to its raw WIT label — which is
 *  how `waveflow:canvas/v1` reached the screen as `WAVEFLOW:CANVAS/V1`
 *  for as long as the Canvas world has existed, and then again in the
 *  store, whose own copy of this table had been left behind. One table
 *  for both cards and the store's search, so a new world is labelled
 *  (and found) everywhere or nowhere. */
const WORLD_LABEL_KEYS: [string, string][] = [
  ["waveflow:source", "source"],
  ["waveflow:metadata", "metadata"],
  ["waveflow:ui", "ui"],
  ["waveflow:canvas", "canvas"],
];

/** The i18n key naming a world's role, or `null` for a world this build
 *  has no label for (shown raw). */
export function pluginWorldLabelKey(world: string): string | null {
  const entry = WORLD_LABEL_KEYS.find(([prefix]) => world.startsWith(prefix));
  return entry ? `settings.plugins.worlds.${entry[1]}` : null;
}
