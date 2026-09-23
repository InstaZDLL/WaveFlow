import { ChevronLeft } from "lucide-react";
import { useTranslation } from "react-i18next";
import { usePlayer } from "../../hooks/usePlayer";

/**
 * Spotify-style floating tab on the right edge that opens the Now Playing
 * panel — and, while another right-edge panel is showing, switches to it.
 *
 * It hides only when Now Playing is itself the open panel, because this
 * tab is that panel's only reliable entry point: the player bar carries a
 * Lyrics and a Queue button but none for Now Playing, and the cover
 * thumbnail opens it only when `layout.coverAction` is set to
 * `now_playing` (it defaults to the immersive view). Hiding the tab for
 * *any* open panel therefore left no way back — reaching the artwork from
 * the lyrics meant closing the panel and reopening it from here.
 *
 * Every other direction already worked, through the player bar's own
 * toggles: each sets its own panel rather than closing the current one,
 * so Now Playing → Lyrics and Lyrics → Queue switch in a single click.
 */
export function NowPlayingChevronTab() {
  const { t } = useTranslation();
  const { isNowPlayingOpen, toggleNowPlaying } = usePlayer();

  if (isNowPlayingOpen) return null;

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
