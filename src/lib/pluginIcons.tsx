import type { ReactNode } from "react";
import {
  Album,
  Calendar,
  Clock,
  Compass,
  Disc3,
  Flame,
  Globe,
  Headphones,
  Heart,
  LayoutGrid,
  ListMusic,
  Mic2,
  Music,
  Newspaper,
  Puzzle,
  Radar,
  Radio,
  Rss,
  Sparkles,
  Star,
  Tag,
  TrendingUp,
  type LucideIcon,
} from "lucide-react";

/**
 * Curated lucide icon set a ui plugin's `manifest().sidebarIcon` may
 * name. The host resolves ONLY from this allowlist — an unknown (or
 * null) name falls back to the generic Puzzle glyph, so a plugin can
 * never point the host at arbitrary icon data, only pick from a set
 * WaveFlow already bundles. Keys are lowercase; a plugin author writes
 * e.g. `sidebar_icon = "radar"` in its manifest.
 */
const PLUGIN_ICONS: Record<string, LucideIcon> = {
  radar: Radar,
  radio: Radio,
  music: Music,
  disc: Disc3,
  calendar: Calendar,
  star: Star,
  heart: Heart,
  sparkles: Sparkles,
  newspaper: Newspaper,
  compass: Compass,
  list: ListMusic,
  grid: LayoutGrid,
  headphones: Headphones,
  mic: Mic2,
  album: Album,
  tag: Tag,
  globe: Globe,
  flame: Flame,
  "trending-up": TrendingUp,
  clock: Clock,
  rss: Rss,
};

/**
 * Resolve a plugin-declared icon name to a rendered lucide glyph,
 * falling back to the generic plugin icon for an unknown or absent
 * name.
 */
export function resolvePluginIcon(
  name: string | null | undefined,
  size = 18,
): ReactNode {
  const Icon = (name && PLUGIN_ICONS[name.toLowerCase()]) || Puzzle;
  return <Icon size={size} />;
}
