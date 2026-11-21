import type { ReactNode, Ref } from "react";
import { useTranslation } from "react-i18next";

interface NowPlayingTitleLinkProps {
  /** The library album of the playing track. `null` for a track with no
   *  album row, and for every source that is not a library track (Web
   *  Radio, a server track, an episode, a file opened without importing
   *  it), which plays with no `album_id`. */
  albumId: number | null | undefined;
  onNavigate: (albumId: number) => void;
  /** Applied whether or not the title is a link. The text colour goes
   *  here rather than on the children, so the link's hover colour can
   *  reach the text. */
  className?: string;
  /** Lets a caller that handles keys on an ancestor tell this button's
   *  keystrokes from a child control's. */
  buttonRef?: Ref<HTMLButtonElement>;
  children: ReactNode;
}

/**
 * The now-playing title as a link to its album, the way the artist under
 * it opens the artist. Wraps its children (the title is usually a
 * `MarqueeText`) where `AlbumLink` renders a string, and renders them in a
 * plain span when there is no album to open, so a radio stream or an
 * episode keeps a plain title.
 *
 * Shrinks to its text (`self-start`, `max-w-full`) so the empty space
 * beside a short title is not a link, and keeps `min-w-0` so a long title
 * inside it still truncates or scrolls rather than widening the column.
 */
export function NowPlayingTitleLink({
  albumId,
  onNavigate,
  className = "",
  buttonRef,
  children,
}: NowPlayingTitleLinkProps) {
  const { t } = useTranslation();
  if (albumId == null) {
    return <span className={`block min-w-0 ${className}`}>{children}</span>;
  }
  return (
    <button
      ref={buttonRef}
      type="button"
      // Lets a skin that inks every player-bar button leave the title in
      // the title's colour (Liquid).
      data-now-playing-title=""
      title={t("trackActions.goToAlbum")}
      onClick={(e) => {
        e.stopPropagation();
        onNavigate(albumId);
      }}
      className={`block self-start max-w-full min-w-0 text-start cursor-pointer hover:underline hover:text-emerald-600 dark:hover:text-emerald-400 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded ${className}`}
    >
      {children}
    </button>
  );
}
