import { useTranslation } from "react-i18next";
import { Artwork } from "../../../common/Artwork";
import { ArtistLink } from "../../../common/ArtistLink";
import type { RecentPlay } from "../../../../lib/tauri/browse";
import { clock, relativeTime } from "./format";
import { SectionHeader } from "./SectionHeader";

interface ListeningHistoryProps {
  plays: RecentPlay[];
  loading: boolean;
  currentTrackId: number | null;
  onPlay: (index: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onSeeAll: () => void;
}

/**
 * Recently played as a two-column list, not a grid of 160 px tiles:
 * the history is a log, and a log wants rows — title, artist · album,
 * when, how long — twelve of them in the height the old grid spent on
 * six. Clicking a row plays it, from that row, as the old grid did.
 */
export function ListeningHistory({
  plays,
  loading,
  currentTrackId,
  onPlay,
  onNavigateToArtist,
  onSeeAll,
}: ListeningHistoryProps) {
  const { t, i18n } = useTranslation();
  if (!loading && plays.length === 0) return null;
  const rows = plays.slice(0, 12);
  const half = Math.ceil(rows.length / 2);
  const columns = [rows.slice(0, half), rows.slice(half)];

  return (
    <section aria-labelledby="fe-history-title">
      <SectionHeader
        title={t("home.recentlyPlayed.title")}
        className="mb-4"
        action={
          rows.length > 0 ? (
            <button
              type="button"
              className="fe-link text-[0.8125rem]"
              onClick={onSeeAll}
            >
              {t("home.seeAll")}
            </button>
          ) : undefined
        }
      />
      <span id="fe-history-title" className="sr-only">
        {t("home.recentlyPlayed.title")}
      </span>
      {loading && rows.length === 0 ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("home.recentlyPlayed.title")}
          className="grid animate-pulse gap-x-12 md:grid-cols-2"
        >
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={i} className="flex items-center gap-3.5 py-2.5">
              <div className="h-10 w-10 rounded-md bg-white/[0.05]" />
              <div className="flex-1 space-y-2">
                <div className="h-3 w-2/3 rounded bg-white/[0.05]" />
                <div className="h-2.5 w-1/3 rounded bg-white/[0.05]" />
              </div>
            </div>
          ))}
        </div>
      ) : (
        <div className="grid gap-x-12 md:grid-cols-2">
          {columns.map((col, ci) => (
            <ol key={ci} className="divide-y divide-white/[0.06]">
              {col.map((p, i) => {
                const index = ci * half + i;
                const active = p.track_id === currentTrackId;
                return (
                  <li key={`${p.track_id}-${p.played_at}`}>
                    <div
                      role="button"
                      tabIndex={0}
                      onClick={() => onPlay(index)}
                      onKeyDown={(e) => {
                        // The artist link inside is a button of its own:
                        // its Enter stays its Enter.
                        if (e.target !== e.currentTarget) return;
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          onPlay(index);
                        }
                      }}
                      className="group flex w-full cursor-pointer items-center gap-3.5 py-2.5 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--fe-accent)]"
                      aria-current={active ? "true" : undefined}
                    >
                      <Artwork
                        path={p.artwork_path}
                        path1x={p.artwork_path_1x}
                        path2x={p.artwork_path_2x}
                        size="1x"
                        alt=""
                        className="fe-tile h-10 w-10"
                        iconSize={16}
                        rounded="md"
                      />
                      <span className="min-w-0 flex-1">
                        <span
                          className={`block truncate text-[0.875rem] font-medium group-hover:text-[var(--fe-accent)] ${active ? "text-[var(--fe-accent)]" : "fe-ink"}`}
                        >
                          {p.title}
                        </span>
                        <span className="fe-ink-3 block truncate text-[0.8125rem]">
                          <ArtistLink
                            name={p.artist_name}
                            artistIds={p.artist_ids}
                            onNavigate={onNavigateToArtist}
                            fallback="—"
                          />
                          {p.album_title && p.album_title !== p.title && (
                            <>
                              <span className="mx-1.5 opacity-50">·</span>
                              {p.album_title}
                            </>
                          )}
                        </span>
                      </span>
                      <span className="fe-ink-3 fe-mono shrink-0 text-right text-[0.75rem] leading-tight">
                        <span className="block">
                          {relativeTime(p.played_at, i18n.language)}
                        </span>
                        <span className="block opacity-70">
                          {clock(p.duration_ms)}
                        </span>
                      </span>
                    </div>
                  </li>
                );
              })}
            </ol>
          ))}
        </div>
      )}
    </section>
  );
}
