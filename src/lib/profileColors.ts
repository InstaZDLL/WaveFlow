/**
 * Tailwind class lookup for every supported profile color.
 *
 * Kept as a static table so Tailwind's static class analysis can see every
 * permutation — dynamic `bg-${id}-500` would get pruned at build time.
 */
export interface ProfileColor {
  /** Identifier stored in the `profile.color_id` column. */
  id: string;
  /** Solid swatch shown in color pickers (`bg-<c>-500`). */
  swatch: string;
  /** Ring used when a swatch is selected. */
  ring: string;
  /** Soft glow behind the big avatar in the create modal. */
  glow: string;
  /** Border applied to the avatar tile in the create modal. */
  iconBorder: string;
  /** Accent text color used by the large avatar placeholder. */
  iconText: string;
  /** Full button classes for the create CTA in the profile modal. */
  button: string;
  /** Background for the small round avatar (sidebar tile, topbar dropdown). */
  avatarBg: string;
  /** Foreground color for the small round avatar. */
  avatarText: string;
  /** Tiny status dot next to the sidebar profile tile. */
  dot: string;
}

export const PROFILE_COLORS: ProfileColor[] = [
  {
    id: "emerald",
    swatch: "bg-emerald-500",
    ring: "ring-emerald-400",
    glow: "bg-emerald-500/25",
    iconBorder: "border-emerald-500/40",
    iconText: "text-emerald-400",
    button: "bg-emerald-500 hover:bg-emerald-400 shadow-emerald-500/20",
    avatarBg: "bg-emerald-500",
    avatarText: "text-white",
    dot: "bg-emerald-500",
  },
  {
    id: "violet",
    swatch: "bg-violet-500",
    ring: "ring-violet-400",
    glow: "bg-violet-500/25",
    iconBorder: "border-violet-500/40",
    iconText: "text-violet-400",
    button: "bg-violet-500 hover:bg-violet-400 shadow-violet-500/20",
    avatarBg: "bg-violet-500",
    avatarText: "text-white",
    dot: "bg-violet-500",
  },
  {
    id: "sky",
    swatch: "bg-sky-500",
    ring: "ring-sky-400",
    glow: "bg-sky-500/25",
    iconBorder: "border-sky-500/40",
    iconText: "text-sky-400",
    button: "bg-sky-500 hover:bg-sky-400 shadow-sky-500/20",
    avatarBg: "bg-sky-500",
    avatarText: "text-white",
    dot: "bg-sky-500",
  },
  {
    id: "amber",
    swatch: "bg-amber-500",
    ring: "ring-amber-400",
    glow: "bg-amber-500/25",
    iconBorder: "border-amber-500/40",
    iconText: "text-amber-400",
    button: "bg-amber-500 hover:bg-amber-400 shadow-amber-500/20",
    avatarBg: "bg-amber-500",
    avatarText: "text-white",
    dot: "bg-amber-500",
  },
  {
    id: "red",
    swatch: "bg-red-500",
    ring: "ring-red-400",
    glow: "bg-red-500/25",
    iconBorder: "border-red-500/40",
    iconText: "text-red-400",
    button: "bg-red-500 hover:bg-red-400 shadow-red-500/20",
    avatarBg: "bg-red-500",
    avatarText: "text-white",
    dot: "bg-red-500",
  },
  {
    id: "indigo",
    swatch: "bg-indigo-500",
    ring: "ring-indigo-400",
    glow: "bg-indigo-500/25",
    iconBorder: "border-indigo-500/40",
    iconText: "text-indigo-400",
    button: "bg-indigo-500 hover:bg-indigo-400 shadow-indigo-500/20",
    avatarBg: "bg-indigo-500",
    avatarText: "text-white",
    dot: "bg-indigo-500",
  },
  {
    id: "lime",
    swatch: "bg-lime-500",
    ring: "ring-lime-400",
    glow: "bg-lime-500/25",
    iconBorder: "border-lime-500/40",
    iconText: "text-lime-400",
    button: "bg-lime-500 hover:bg-lime-400 shadow-lime-500/20",
    avatarBg: "bg-lime-500",
    avatarText: "text-white",
    dot: "bg-lime-500",
  },
  {
    id: "orange",
    swatch: "bg-orange-500",
    ring: "ring-orange-400",
    glow: "bg-orange-500/25",
    iconBorder: "border-orange-500/40",
    iconText: "text-orange-400",
    button: "bg-orange-500 hover:bg-orange-400 shadow-orange-500/20",
    avatarBg: "bg-orange-500",
    avatarText: "text-white",
    dot: "bg-orange-500",
  },
  {
    id: "rose",
    swatch: "bg-rose-500",
    ring: "ring-rose-400",
    glow: "bg-rose-500/25",
    iconBorder: "border-rose-500/40",
    iconText: "text-rose-400",
    button: "bg-rose-500 hover:bg-rose-400 shadow-rose-500/20",
    avatarBg: "bg-rose-500",
    avatarText: "text-white",
    dot: "bg-rose-500",
  },
  {
    id: "teal",
    swatch: "bg-teal-500",
    ring: "ring-teal-400",
    glow: "bg-teal-500/25",
    iconBorder: "border-teal-500/40",
    iconText: "text-teal-400",
    button: "bg-teal-500 hover:bg-teal-400 shadow-teal-500/20",
    avatarBg: "bg-teal-500",
    avatarText: "text-white",
    dot: "bg-teal-500",
  },
];

export const DEFAULT_PROFILE_COLOR_ID = PROFILE_COLORS[0].id;

/**
 * Look up a color by id, falling back to the default palette entry if the id
 * is unknown (e.g. a legacy color was removed from the codebase).
 */
export function getProfileColor(colorId: string | null | undefined): ProfileColor {
  if (!colorId) return PROFILE_COLORS[0];
  return PROFILE_COLORS.find((c) => c.id === colorId) ?? PROFILE_COLORS[0];
}

/**
 * One-character initial used on round avatars. Falls back to `"?"` for
 * empty strings (shouldn't happen — the backend rejects empty names).
 */
export function profileInitial(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "?";
  return trimmed[0]?.toUpperCase() ?? "?";
}
