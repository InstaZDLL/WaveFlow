import { ChevronLeft } from "lucide-react";
import { useTranslation } from "react-i18next";
import { usePlayer } from "../../hooks/usePlayer";

/**
 * Spotify-style floating tab on the right edge that opens the Now Playing
 * panel. Shown only when no right-edge panel is currently open — once any
 * panel slides in, the in-panel close button takes over.
 */
export function NowPlayingChevronTab() {
  const { t } = useTranslation();
  const { isQueueOpen, isNowPlayingOpen, isLyricsOpen, toggleNowPlaying } =
    usePlayer();

  const anyOpen = isQueueOpen || isNowPlayingOpen || isLyricsOpen;
  if (anyOpen) return null;

  return (
    <button
      type="button"
      onClick={toggleNowPlaying}
      aria-label={t("playerBar.nowPlaying")}
      title={t("playerBar.nowPlaying")}
      className="absolute right-0 top-1/2 -translate-y-1/2 z-30 h-12 w-6 flex items-center justify-center rounded-l-md border border-r-0 border-zinc-200 bg-white/90 text-zinc-500 backdrop-blur shadow-lg transition-colors hover:bg-white hover:text-zinc-800 dark:border-zinc-800 dark:bg-zinc-900/90 dark:text-zinc-400 dark:hover:bg-zinc-900 dark:hover:text-zinc-100"
    >
      <ChevronLeft size={16} />
    </button>
  );
}
