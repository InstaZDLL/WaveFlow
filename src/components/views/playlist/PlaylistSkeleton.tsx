export function PlaylistSkeleton({
  t,
}: {
  t: (key: string, options?: Record<string, unknown>) => string;
}) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={t("playlistView.emptyTitle")}
      className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden animate-pulse"
    >
      {Array.from({ length: 10 }).map((_, i) => (
        <div
          key={i}
          className="grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem] gap-4 px-5 py-2 h-14 items-center border-b border-zinc-100 dark:border-zinc-800/60"
        >
          <div className={`h-3 w-4 rounded ${tile} justify-self-end`} />
          <div className={`w-10 h-10 rounded-md ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 w-10 rounded ${tile} justify-self-end`} />
        </div>
      ))}
    </div>
  );
}
