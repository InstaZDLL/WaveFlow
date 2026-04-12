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
  type LucideProps,
} from "lucide-react";

/**
 * Render a playlist icon by its stored `icon_id`.
 *
 * Implemented as a static dispatcher (single switch) so React's
 * `react-hooks/static-components` lint rule stays happy — callers don't
 * have to assign a Lucide component to a local `const` and `<LocalConst />`
 * it. Lives in its own `.tsx` file so [playlistVisuals.ts](./playlistVisuals.ts)
 * can stay JSX-free and Vite's fast-refresh rule (no mixing of components
 * and constants in the same file) is satisfied.
 */
export function PlaylistIcon({
  iconId,
  ...props
}: { iconId: string } & LucideProps) {
  switch (iconId) {
    case "music":
      return <Music2 {...props} />;
    case "heart":
      return <Heart {...props} />;
    case "star":
      return <Star {...props} />;
    case "flame":
      return <Flame {...props} />;
    case "moon":
      return <Moon {...props} />;
    case "sun":
      return <Sun {...props} />;
    case "cloud":
      return <Cloud {...props} />;
    case "coffee":
      return <Coffee {...props} />;
    case "leaf":
      return <Leaf {...props} />;
    case "gift":
      return <Gift {...props} />;
    case "headphones":
      return <Headphones {...props} />;
    default:
      return <Music2 {...props} />;
  }
}
