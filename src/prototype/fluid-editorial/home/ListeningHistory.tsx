import { fixtures } from "../fixtures";
import { duration, relativeTime } from "../format";
import { useFe } from "../useFe";
import { Cover } from "../ui/Cover";
import { SectionHeader } from "../ui/SectionHeader";

/**
 * Recently played as a two-column list, not a grid of 160 px tiles:
 * the history is a log, and a log wants rows — title, artist, album,
 * when, how long — twelve of them in the height the old grid spent on
 * six. Clicking a row plays it.
 */
export function ListeningHistory() {
  const { copy, lang, play, current } = useFe();
  const rows = fixtures.recentPlays.slice(0, 12);
  const half = Math.ceil(rows.length / 2);
  const columns = [rows.slice(0, half), rows.slice(half)];

  return (
    <section aria-labelledby="fe-history-title">
      <SectionHeader
        title={copy.history.title}
        className="mb-4"
        action={
          <button type="button" className="fe-link text-[0.8125rem]">
            {copy.seeAll}
          </button>
        }
      />
      <span id="fe-history-title" className="sr-only">
        {copy.history.title}
      </span>
      <div className="grid gap-x-12 md:grid-cols-2">
        {columns.map((col, ci) => (
          <ol key={ci} className="divide-y divide-white/[0.06]">
            {col.map((t) => {
              const active = t.trackId === current.trackId;
              return (
                <li key={`${t.trackId}-${t.playedAt}`}>
                  <button
                    type="button"
                    onClick={() => play(t)}
                    className="group flex w-full items-center gap-3.5 py-2.5 text-left"
                    aria-current={active ? "true" : undefined}
                  >
                    <Cover
                      src={t.cover}
                      palette={t.palette}
                      alt={t.album ?? t.title}
                      decorative
                      className="h-10 w-10 rounded-md!"
                    />
                    <span className="min-w-0 flex-1">
                      <span
                        className={`block truncate text-[0.875rem] font-medium group-hover:text-[var(--fe-accent)] ${active ? "text-[var(--fe-accent)]" : "text-[var(--fe-ink)]"}`}
                      >
                        {t.title}
                      </span>
                      <span className="fe-ink-3 block truncate text-[0.8125rem]">
                        {t.artist}
                        {t.album && t.album !== t.title && (
                          <>
                            <span className="mx-1.5 opacity-50">·</span>
                            {t.album}
                          </>
                        )}
                      </span>
                    </span>
                    <span className="fe-ink-3 fe-mono shrink-0 text-right text-[0.75rem] leading-tight">
                      <span className="block">
                        {relativeTime(t.playedAt, lang)}
                      </span>
                      <span className="block opacity-70">
                        {duration(t.durationMs)}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
        ))}
      </div>
    </section>
  );
}
