import type { MouseEvent } from "react";

interface AlbumLinkProps {
  title: string | null | undefined;
  /** Backend `track.album_id`. When null the title renders as plain text
   *  (loose tracks without an album row, e.g. single-file imports). */
  albumId: number | null | undefined;
  onNavigate: (albumId: number) => void;
  fallback?: string;
  className?: string;
}

/**
 * Clickable album-title cell. Mirrors the pattern of [`ArtistLink`] —
 * stops propagation so the click doesn't trigger the row's
 * double-click-to-play handler. When `albumId` is missing the title
 * falls back to a plain span so we never navigate to a stale id.
 */
export function AlbumLink({
  title,
  albumId,
  onNavigate,
  fallback = "—",
  className = "",
}: AlbumLinkProps) {
  if (!title || !title.trim()) {
    return <span className={className}>{fallback}</span>;
  }
  if (albumId == null) {
    return <span className={className}>{title}</span>;
  }
  const handleClick = (e: MouseEvent) => {
    e.stopPropagation();
    onNavigate(albumId);
  };
  return (
    <span className={className}>
      <button
        type="button"
        onClick={handleClick}
        className="hover:underline hover:text-emerald-600 dark:hover:text-emerald-400 transition-colors cursor-pointer text-left truncate max-w-full"
      >
        {title}
      </button>
    </span>
  );
}
