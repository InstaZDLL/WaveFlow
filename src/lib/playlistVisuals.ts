import {
  Music2,
  Heart,
  Star,
  Flame,
  Moon,
  Sun,
  Cloud,
  Coffee,
  Leaf,
  Gift,
  Headphones,
  type LucideIcon,
} from "lucide-react";

/**
 * Shared visual vocabulary for playlists.
 *
 * Extracted out of `CreatePlaylistModal` so that the sidebar row, the
 * `PlaylistView` header and the modal itself can resolve a stored
 * `color_id` / `icon_id` into concrete Tailwind classes and Lucide
 * components without importing the modal file.
 *
 * The actual icon **render** is done by [`PlaylistIcon`](./PlaylistIcon.tsx)
 * — that lives in its own `.tsx` file so this module stays free of JSX
 * and Vite's fast-refresh rule (no mixing of components and constants
 * in the same file) is satisfied.
 */

export interface PlaylistColor {
  /** Stable id written to the database in `playlist.color_id`. */
  id: string;
  /** Solid color swatch used by the picker in the modal. */
  swatch: string;
  /** Ring color when the swatch is selected in the picker. */
  ring: string;
  /** Tile background (light + dark) used for the 40×40 icon square. */
  tileBg: string;
  /** Tile foreground (icon color). */
  tileText: string;
  /** Wider background used for preview cards and the PlaylistView header. */
  previewBg: string;
  /** Button background used by the modal's submit action. */
  button: string;
}

export const PLAYLIST_COLORS: PlaylistColor[] = [
  {
    id: "violet",
    swatch: "bg-violet-500",
    ring: "ring-violet-400",
    tileBg: "bg-violet-100 dark:bg-violet-950/60",
    tileText: "text-violet-500 dark:text-violet-400",
    previewBg: "bg-violet-50 dark:bg-violet-900/20",
    button: "bg-violet-500 hover:bg-violet-400 shadow-violet-500/20",
  },
  {
    id: "emerald",
    swatch: "bg-emerald-500",
    ring: "ring-emerald-400",
    tileBg: "bg-emerald-100 dark:bg-emerald-950/60",
    tileText: "text-emerald-500 dark:text-emerald-400",
    previewBg: "bg-emerald-50 dark:bg-emerald-900/20",
    button: "bg-emerald-500 hover:bg-emerald-400 shadow-emerald-500/20",
  },
  {
    id: "sky",
    swatch: "bg-sky-500",
    ring: "ring-sky-400",
    tileBg: "bg-sky-100 dark:bg-sky-950/60",
    tileText: "text-sky-500 dark:text-sky-400",
    previewBg: "bg-sky-50 dark:bg-sky-900/20",
    button: "bg-sky-500 hover:bg-sky-400 shadow-sky-500/20",
  },
  {
    id: "amber",
    swatch: "bg-amber-500",
    ring: "ring-amber-400",
    tileBg: "bg-amber-100 dark:bg-amber-950/60",
    tileText: "text-amber-500 dark:text-amber-400",
    previewBg: "bg-amber-50 dark:bg-amber-900/20",
    button: "bg-amber-500 hover:bg-amber-400 shadow-amber-500/20",
  },
  {
    id: "rose",
    swatch: "bg-rose-500",
    ring: "ring-rose-400",
    tileBg: "bg-rose-100 dark:bg-rose-950/60",
    tileText: "text-rose-500 dark:text-rose-400",
    previewBg: "bg-rose-50 dark:bg-rose-900/20",
    button: "bg-rose-500 hover:bg-rose-400 shadow-rose-500/20",
  },
  {
    id: "purple",
    swatch: "bg-purple-500",
    ring: "ring-purple-400",
    tileBg: "bg-purple-100 dark:bg-purple-950/60",
    tileText: "text-purple-500 dark:text-purple-400",
    previewBg: "bg-purple-50 dark:bg-purple-900/20",
    button: "bg-purple-500 hover:bg-purple-400 shadow-purple-500/20",
  },
  {
    id: "pink",
    swatch: "bg-pink-500",
    ring: "ring-pink-400",
    tileBg: "bg-pink-100 dark:bg-pink-950/60",
    tileText: "text-pink-500 dark:text-pink-400",
    previewBg: "bg-pink-50 dark:bg-pink-900/20",
    button: "bg-pink-500 hover:bg-pink-400 shadow-pink-500/20",
  },
  {
    id: "teal",
    swatch: "bg-teal-500",
    ring: "ring-teal-400",
    tileBg: "bg-teal-100 dark:bg-teal-950/60",
    tileText: "text-teal-500 dark:text-teal-400",
    previewBg: "bg-teal-50 dark:bg-teal-900/20",
    button: "bg-teal-500 hover:bg-teal-400 shadow-teal-500/20",
  },
  {
    id: "orange",
    swatch: "bg-orange-500",
    ring: "ring-orange-400",
    tileBg: "bg-orange-100 dark:bg-orange-950/60",
    tileText: "text-orange-500 dark:text-orange-400",
    previewBg: "bg-orange-50 dark:bg-orange-900/20",
    button: "bg-orange-500 hover:bg-orange-400 shadow-orange-500/20",
  },
  {
    id: "lime",
    swatch: "bg-lime-500",
    ring: "ring-lime-400",
    tileBg: "bg-lime-100 dark:bg-lime-950/60",
    tileText: "text-lime-500 dark:text-lime-400",
    previewBg: "bg-lime-50 dark:bg-lime-900/20",
    button: "bg-lime-500 hover:bg-lime-400 shadow-lime-500/20",
  },
];

export interface PlaylistIconEntry {
  /** Stable id written to the database in `playlist.icon_id`. */
  id: string;
  Icon: LucideIcon;
}

export const PLAYLIST_ICONS: PlaylistIconEntry[] = [
  { id: "music", Icon: Music2 },
  { id: "heart", Icon: Heart },
  { id: "star", Icon: Star },
  { id: "flame", Icon: Flame },
  { id: "moon", Icon: Moon },
  { id: "sun", Icon: Sun },
  { id: "cloud", Icon: Cloud },
  { id: "coffee", Icon: Coffee },
  { id: "leaf", Icon: Leaf },
  { id: "gift", Icon: Gift },
  { id: "headphones", Icon: Headphones },
];

/** Resolve a stored `color_id` to its visual bundle. Falls back to violet. */
export function resolvePlaylistColor(colorId: string): PlaylistColor {
  return PLAYLIST_COLORS.find((c) => c.id === colorId) ?? PLAYLIST_COLORS[0];
}
