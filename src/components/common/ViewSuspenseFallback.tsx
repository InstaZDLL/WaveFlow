/**
 * Generic skeleton shown while a lazily-loaded view's JS chunk
 * downloads. Replaces the tiny centered spinner that previously
 * looked like an unhandled blank screen on first navigation to a
 * route. Most navigations skip this entirely because [`AppLayout`]
 * pre-warms every view chunk at idle time after first mount.
 */
export function ViewSuspenseFallback() {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label="Loading"
      className="space-y-8 animate-pulse pb-12"
    >
      <div className="flex items-start space-x-5">
        <div className={`w-20 h-20 rounded-2xl ${tile}`} />
        <div className="flex-1 space-y-3 pt-2">
          <div className={`h-7 w-1/3 rounded ${tile}`} />
          <div className={`h-3 w-1/4 rounded ${tile}`} />
        </div>
      </div>
      <div className="space-y-3">
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            className="grid grid-cols-[2.75rem_1fr_1fr_5rem] gap-4 items-center"
          >
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
