/**
 * Detail-page skeleton shared by AlbumDetailView, ArtistDetailView and
 * GenreDetailView. Mimics the "big cover / artist photo + header on the
 * left, track list below" layout so the swap to live data is a content
 * change instead of a blank → populated jump.
 *
 * `ariaLabel` is announced by screen readers via `role="status"` while
 * the page chunk + SQL fetch resolve.
 */
export function DetailViewSkeleton({
  ariaLabel,
  shape = "square",
}: {
  ariaLabel: string;
  /** Cover shape — `square` for albums/genres, `circle` for artists. */
  shape?: "square" | "circle";
}) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  const shapeClass = shape === "circle" ? "rounded-full" : "rounded-2xl";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={ariaLabel}
      className="space-y-8 animate-pulse pb-12"
    >
      <div className="flex items-end space-x-6">
        <div className={`w-44 h-44 ${shapeClass} ${tile}`} />
        <div className="flex-1 space-y-3 pb-4">
          <div className={`h-3 w-16 rounded ${tile}`} />
          <div className={`h-10 w-2/3 rounded ${tile}`} />
          <div className={`h-3 w-1/3 rounded ${tile}`} />
          <div className="flex space-x-3 pt-2">
            <div className={`h-10 w-28 rounded-xl ${tile}`} />
            <div className={`h-10 w-10 rounded-xl ${tile}`} />
            <div className={`h-10 w-10 rounded-xl ${tile}`} />
          </div>
        </div>
      </div>
      <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
        {Array.from({ length: 8 }).map((_, i) => (
          <div
            key={i}
            className="grid grid-cols-[3rem_2.75rem_1fr_1fr_5rem] gap-4 px-5 py-2 h-14 items-center border-b border-zinc-100 dark:border-zinc-800/60"
          >
            <div className={`h-3 w-4 rounded ${tile} justify-self-end`} />
            <div className={`w-10 h-10 rounded-md ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 w-10 rounded ${tile} justify-self-end`} />
          </div>
        ))}
      </div>
    </div>
  );
}
