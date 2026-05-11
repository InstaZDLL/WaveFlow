import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { Clock } from "lucide-react";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import {
  listPlayHistory,
  playHistoryMonths,
  type PlayHistoryMonth,
  type PlayHistoryRow,
} from "../../lib/tauri/browse";
import {
  formatDuration,
  listLikedTrackIds,
  type Track,
} from "../../lib/tauri/track";

interface HistoryViewProps {
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

/** Page size for the infinite scroll fetcher. */
const PAGE_SIZE = 100;

const RANGES = [
  { id: "all" as const, days: null },
  { id: "7d" as const, days: 7 },
  { id: "30d" as const, days: 30 },
  { id: "90d" as const, days: 90 },
  { id: "1y" as const, days: 365 },
];
type RangeId = (typeof RANGES)[number]["id"];

function rangeStartMs(range: RangeId): number | null {
  const cfg = RANGES.find((r) => r.id === range);
  if (!cfg || cfg.days == null) return null;
  return Date.now() - cfg.days * 24 * 60 * 60 * 1000;
}

/** Lift a `PlayHistoryRow` to a `Track` for the context menu. */
function rowToTrack(row: PlayHistoryRow): Track {
  return {
    id: row.track_id,
    library_id: 0,
    title: row.title,
    album_id: row.album_id,
    album_title: row.album_title,
    artist_id: row.artist_id,
    artist_name: row.artist_name,
    artist_ids: row.artist_ids,
    duration_ms: row.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: null,
    musical_key: null,
    file_path: row.file_path,
    file_size: 0,
    added_at: 0,
    artwork_path: row.artwork_path,
    artwork_path_1x: row.artwork_path_1x,
    artwork_path_2x: row.artwork_path_2x,
    rating: null,
  };
}

/** "YYYY-MM-DD" in local time — day-grouping key. */
function dayKey(ts: number): string {
  const d = new Date(ts);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function startOfDay(ts: number): number {
  const d = new Date(ts);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/**
 * Last.fm-style chronological listening history. One row per
 * play_event (no per-track dedup), grouped by day with sticky day
 * headers, infinite scroll, and a vertical month scrubber on the
 * right that lets the user jump backwards in time without scrolling
 * through everything in between.
 */
export function HistoryView({
  onNavigateToAlbum,
  onNavigateToArtist,
}: HistoryViewProps) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;
  const { playbackState, currentTrack, playTracks } = usePlayer();
  const { createPlaylist } = usePlaylist();

  const [rows, setRows] = useState<PlayHistoryRow[]>([]);
  const [months, setMonths] = useState<PlayHistoryMonth[]>([]);
  const [range, setRange] = useState<RangeId>("all");
  const [isLoading, setIsLoading] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  // `beforeMs` for the next page: the `played_at` of the oldest row
  // we already have. `null` = first page.
  const [cursor, setCursor] = useState<number | null>(null);

  const sentinelRef = useRef<HTMLDivElement | null>(null);

  // ── Initial / refetch loaders ──────────────────────────────────
  const afterMs = useMemo(() => rangeStartMs(range), [range]);

  const reload = useCallback(async () => {
    setIsLoading(true);
    try {
      const list = await listPlayHistory({
        beforeMs: null,
        afterMs,
        limit: PAGE_SIZE,
      });
      setRows(list);
      setCursor(list.length > 0 ? list[list.length - 1].played_at : null);
      setHasMore(list.length === PAGE_SIZE);
    } catch (err) {
      console.error("[HistoryView] initial load failed", err);
      setRows([]);
      setCursor(null);
      setHasMore(false);
    } finally {
      setIsLoading(false);
    }
  }, [afterMs]);

  // Refetch on range change + when a track ends + on tag edits.
  // `reload` is the entry point of an async fetch pipeline (DB
  // query → setState), so the lint rule against synchronous
  // setState-in-effect doesn't apply here.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    reload();
  }, [reload, playbackState]);
  useTrackUpdated(useCallback(() => reload(), [reload]));

  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, []);

  useEffect(() => {
    playHistoryMonths()
      .then(setMonths)
      .catch((err) =>
        console.error("[HistoryView] play_history_months failed", err),
      );
  }, [playbackState]);

  // ── Infinite scroll: load next page when sentinel intersects ────
  const loadMore = useCallback(async () => {
    if (cursor == null || !hasMore || isLoading) return;
    setIsLoading(true);
    try {
      const more = await listPlayHistory({
        beforeMs: cursor,
        afterMs,
        limit: PAGE_SIZE,
      });
      if (more.length === 0) {
        setHasMore(false);
      } else {
        setRows((prev) => [...prev, ...more]);
        setCursor(more[more.length - 1].played_at);
        setHasMore(more.length === PAGE_SIZE);
      }
    } catch (err) {
      console.error("[HistoryView] loadMore failed", err);
    } finally {
      setIsLoading(false);
    }
  }, [cursor, hasMore, isLoading, afterMs]);

  useEffect(() => {
    const target = sentinelRef.current;
    if (!target) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          loadMore();
        }
      },
      { rootMargin: "300px" },
    );
    observer.observe(target);
    return () => observer.disconnect();
  }, [loadMore]);

  // ── Group consecutive rows by day key ──────────────────────────
  const dayGroups = useMemo(() => {
    const groups: { key: string; ts: number; rows: PlayHistoryRow[] }[] = [];
    for (const row of rows) {
      const key = dayKey(row.played_at);
      const last = groups[groups.length - 1];
      if (last && last.key === key) {
        last.rows.push(row);
      } else {
        groups.push({ key, ts: row.played_at, rows: [row] });
      }
    }
    return groups;
  }, [rows]);

  const trackContextMenu = useTrackContextMenu({
    likedIds,
    onLikedChanged: (trackId, nowLiked) =>
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      }),
    onCreatePlaylist: () => setIsCreatePlaylistModalOpen(true),
    onNavigateToAlbum,
    onNavigateToArtist,
  });

  // ── Month scrubber: clicking a month re-anchors the cursor at
  //    the END of that month and reloads from the top. Cleaner than
  //    scrolling through hundreds of intervening rows.
  const handleJumpToMonth = useCallback(
    async (m: PlayHistoryMonth) => {
      const start = m.start_ms;
      const next = new Date(m.year, m.month, 1).getTime();
      setIsLoading(true);
      try {
        const list = await listPlayHistory({
          beforeMs: next,
          afterMs: Math.max(start, afterMs ?? -Infinity),
          limit: PAGE_SIZE,
        });
        setRows(list);
        setCursor(list.length > 0 ? list[list.length - 1].played_at : null);
        setHasMore(list.length === PAGE_SIZE);
        // Scroll the page to the top so the user lands at the start
        // of the requested month rather than wherever they were.
        window.requestAnimationFrame(() => {
          window.scrollTo({ top: 0, behavior: "smooth" });
        });
      } catch (err) {
        console.error("[HistoryView] jump-to-month failed", err);
      } finally {
        setIsLoading(false);
      }
    },
    [afterMs],
  );

  return (
    <div className="flex gap-6 pb-20 animate-fade-in">
      <div className="flex-1 min-w-0 space-y-6">
        {/* Header */}
        <div className="flex items-center space-x-6">
          <div className="w-24 h-24 rounded-2xl bg-blue-100 text-blue-500 flex items-center justify-center shadow-sm dark:bg-blue-950/60 dark:text-blue-400">
            <Clock size={48} />
          </div>
          <div>
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("history.badge")}
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              {t("history.title")}
            </h1>
            <div className="text-sm text-zinc-500 mt-1">
              {t("history.shownCount", { count: rows.length })}
            </div>
          </div>
        </div>

        {/* Range filters */}
        <div className="flex flex-wrap gap-2">
          {RANGES.map((r) => (
            <button
              key={r.id}
              type="button"
              onClick={() => setRange(r.id)}
              className={`px-3 py-1.5 rounded-full text-xs font-medium transition-colors ${
                range === r.id
                  ? "bg-emerald-500 text-white"
                  : "bg-zinc-100 text-zinc-600 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
              }`}
            >
              {t(`history.range.${r.id}`)}
            </button>
          ))}
        </div>

        {/* Empty state */}
        {!isLoading && rows.length === 0 ? (
          <EmptyState
            icon={<Clock size={40} />}
            title={t("history.emptyTitle")}
            description={t("history.emptyDescription")}
            accent="blue"
            shape="circle"
            className="py-20"
          />
        ) : (
          <div>
            {dayGroups.map((group) => (
              <DayGroup
                key={group.key}
                group={group}
                locale={locale}
                currentTrackId={currentTrack?.id ?? null}
                onNavigateToArtist={onNavigateToArtist}
                onContextMenu={(e, row) =>
                  trackContextMenu.open(e, rowToTrack(row))
                }
                onPlay={(row) =>
                  // Single-track play — uses the row's track_id and a
                  // synthetic queue of just this track. Source type is
                  // "manual" because the user clicked from a chronological
                  // list, not a playlist or radio.
                  playTracks([rowToTrack(row)], 0, {
                    type: "manual",
                    id: null,
                  })
                }
              />
            ))}
            {/* Infinite-scroll sentinel + spinner */}
            <div ref={sentinelRef} className="py-6 text-center">
              {isLoading && (
                <span className="text-xs text-zinc-400">
                  {t("history.loadingMore")}
                </span>
              )}
              {!isLoading && !hasMore && rows.length > 0 && (
                <span className="text-xs text-zinc-400">
                  {t("history.endReached")}
                </span>
              )}
            </div>
          </div>
        )}
      </div>

      {/* Month scrubber — only meaningful with at least 2 months of data */}
      {months.length > 1 && (
        <aside className="hidden lg:block w-36 shrink-0 sticky top-6 self-start">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-3">
            {t("history.timeline")}
          </div>
          <ul className="space-y-1 max-h-[70vh] overflow-y-auto pr-2">
            {[...months].reverse().map((m) => (
              <MonthEntry
                key={`${m.year}-${m.month}`}
                month={m}
                maxPlays={Math.max(...months.map((mm) => mm.plays))}
                locale={locale}
                onClick={() => handleJumpToMonth(m)}
              />
            ))}
          </ul>
        </aside>
      )}

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
        onCreate={async (data) => {
          try {
            await createPlaylist({
              name: data.name,
              description: data.description || null,
              color_id: data.colorId,
              icon_id: data.iconId,
            });
          } catch (err) {
            console.error("[HistoryView] create playlist failed", err);
          }
        }}
      />

      {trackContextMenu.render()}
    </div>
  );
}

function dayLabel(ts: number, locale: string, t: ReturnType<typeof useTranslation>["t"]): string {
  const today = startOfDay(Date.now());
  const dayStart = startOfDay(ts);
  if (dayStart === today) return t("history.today");
  if (dayStart === today - 24 * 60 * 60 * 1000) return t("history.yesterday");
  return new Intl.DateTimeFormat(locale, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: dayStart < today - 365 * 24 * 60 * 60 * 1000 ? "numeric" : undefined,
  }).format(new Date(ts));
}

function timeLabel(ts: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(ts));
}

interface DayGroupProps {
  group: { key: string; ts: number; rows: PlayHistoryRow[] };
  locale: string;
  currentTrackId: number | null;
  onContextMenu: (e: React.MouseEvent, row: PlayHistoryRow) => void;
  onPlay: (row: PlayHistoryRow) => void;
  onNavigateToArtist: (artistId: number) => void;
}

function DayGroup({
  group,
  locale,
  currentTrackId,
  onContextMenu,
  onPlay,
  onNavigateToArtist,
}: DayGroupProps) {
  const { t } = useTranslation();
  return (
    <section>
      {/* Opaque background covers any row scrolling under the
          sticky header. `-top-px` and the matching `-mt-px` align
          the header's bottom border with the next row so there's
          no perceived gap; the header itself stays tight (py-1)
          against the previous row above it. */}
      <div className="sticky -top-px z-20 bg-white dark:bg-surface-dark px-3 py-1 border-b border-zinc-100 dark:border-zinc-800 flex items-baseline gap-3">
        <h2 className="text-sm font-semibold text-zinc-700 dark:text-zinc-200 capitalize">
          {dayLabel(group.ts, locale, t)}
        </h2>
        <span className="text-[11px] text-zinc-400">
          {t("history.plays", { count: group.rows.length })}
        </span>
      </div>
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-800/60">
        {group.rows.map((row) => {
          const isCurrent = row.track_id === currentTrackId;
          return (
            <li
              key={row.event_id}
              onContextMenu={(e) => onContextMenu(e, row)}
              onDoubleClick={() => onPlay(row)}
              className={`grid grid-cols-[3rem_1fr_1fr_5rem_4rem] gap-4 px-3 py-2 items-center rounded-lg transition-colors cursor-default ${
                isCurrent
                  ? "bg-emerald-50 dark:bg-emerald-900/20"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
              }`}
            >
              <Artwork
                path={row.artwork_path}
                className="w-10 h-10"
                iconSize={18}
                alt={row.album_title ?? row.title}
                rounded="md"
              />
              <div className="min-w-0">
                <div
                  className={`text-sm truncate ${
                    isCurrent
                      ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                      : "text-zinc-800 dark:text-zinc-200"
                  }`}
                >
                  {row.title}
                </div>
                <ArtistLink
                  name={row.artist_name}
                  artistIds={row.artist_ids}
                  onNavigate={onNavigateToArtist}
                  fallback="—"
                  className="text-xs text-zinc-500 truncate block"
                />
              </div>
              <span className="text-sm text-zinc-500 truncate">
                {row.album_title ?? "—"}
              </span>
              <span className="text-xs tabular-nums text-zinc-400 text-right">
                {timeLabel(row.played_at, locale)}
              </span>
              <span className="text-xs tabular-nums text-zinc-400 text-right">
                {formatDuration(row.duration_ms)}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

interface MonthEntryProps {
  month: PlayHistoryMonth;
  maxPlays: number;
  locale: string;
  onClick: () => void;
}

function MonthEntry({ month, maxPlays, locale, onClick }: MonthEntryProps) {
  const label = useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        month: "short",
        year: "2-digit",
      }).format(new Date(month.year, month.month - 1, 1)),
    [month, locale],
  );
  const fillPct = maxPlays > 0 ? Math.round((month.plays / maxPlays) * 100) : 0;
  return (
    <li>
      <button
        type="button"
        onClick={onClick}
        className="w-full text-left px-2 py-1.5 rounded-md hover:bg-zinc-100 dark:hover:bg-zinc-800/60 transition-colors group"
        title={`${month.plays}`}
      >
        <div className="flex items-center justify-between text-xs text-zinc-500 dark:text-zinc-400 group-hover:text-zinc-800 dark:group-hover:text-zinc-100">
          <span className="font-medium uppercase tracking-wider">{label}</span>
          <span className="tabular-nums text-[10px]">{month.plays}</span>
        </div>
        {/* Sparkline-style fill, scaled against the busiest month */}
        <div className="mt-1 h-0.5 w-full rounded bg-zinc-100 dark:bg-zinc-800 overflow-hidden">
          <div
            className="h-full bg-emerald-500/60"
            style={{ width: `${fillPct}%` }}
          />
        </div>
      </button>
    </li>
  );
}
