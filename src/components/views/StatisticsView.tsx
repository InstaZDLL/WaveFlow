import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  BarChart2,
  Clock,
  Disc3,
  Music2,
  PlayCircle,
} from "lucide-react";
import type { ViewId } from "../../types";
import { Artwork } from "../common/Artwork";
import { EmptyState } from "../common/EmptyState";
import {
  statsListeningByDay,
  statsListeningByHour,
  statsOverview,
  statsTopAlbums,
  statsTopArtists,
  statsTopTracks,
  type ListeningByDayRow,
  type StatsOverview,
  type StatsRange,
  type TopAlbumRow,
  type TopArtistRow,
  type TopTrackRow,
} from "../../lib/tauri/stats";
import { RangeSelector } from "./statistics/RangeSelector";
import { KpiCard } from "./statistics/KpiCard";
import { BarChart } from "./statistics/BarChart";
import { TopList, TopRow } from "./statistics/TopList";
import {
  formatCount,
  formatDayShort,
  formatListenTime,
  formatPercent,
} from "./statistics/formatters";

interface StatisticsViewProps {
  onNavigate: (view: ViewId) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

const TOP_LIMIT = 10;

export function StatisticsView({
  onNavigate,
  onNavigateToAlbum,
  onNavigateToArtist,
}: StatisticsViewProps) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language;

  const [range, setRange] = useState<StatsRange>("30d");
  const [overview, setOverview] = useState<StatsOverview | null>(null);
  const [byDay, setByDay] = useState<ListeningByDayRow[]>([]);
  const [byHour, setByHour] = useState<number[]>([]);
  const [topTracks, setTopTracks] = useState<TopTrackRow[]>([]);
  const [topArtists, setTopArtists] = useState<TopArtistRow[]>([]);
  const [topAlbums, setTopAlbums] = useState<TopAlbumRow[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setIsLoading(true);
    Promise.all([
      statsOverview(range),
      statsListeningByDay(range),
      statsListeningByHour(range),
      statsTopTracks(range, TOP_LIMIT),
      statsTopArtists(range, TOP_LIMIT),
      statsTopAlbums(range, TOP_LIMIT),
    ])
      .then(([ov, day, hour, tracks, artists, albums]) => {
        if (cancelled) return;
        setOverview(ov);
        setByDay(day);
        setByHour(hour);
        setTopTracks(tracks);
        setTopArtists(artists);
        setTopAlbums(albums);
      })
      .catch((err) => {
        console.error("[StatisticsView] load failed", err);
      })
      .finally(() => {
        if (!cancelled) setIsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [range]);

  const dayData = useMemo(
    () =>
      byDay.map((row) => ({
        key: row.day,
        label: formatDayShort(row.day, locale),
        value: row.listened_ms,
        tooltip: `${formatDayShort(row.day, locale)} — ${formatListenTime(row.listened_ms)} (${row.plays})`,
      })),
    [byDay, locale],
  );

  const hourData = useMemo(
    () =>
      byHour.map((plays, h) => ({
        key: String(h),
        label: String(h).padStart(2, "0"),
        value: plays,
        tooltip: t("statistics.byHour.tooltip", {
          hour: String(h).padStart(2, "0"),
          plays,
        }),
      })),
    [byHour, t],
  );

  const isEmpty = !isLoading && overview != null && overview.total_plays === 0;

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center justify-between gap-4 flex-wrap">
        <div className="flex items-center space-x-4">
          <button
            type="button"
            onClick={() => onNavigate("home")}
            aria-label={t("common.back")}
            className="p-1 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
          >
            <ArrowLeft size={20} />
          </button>
          <div>
            <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
              {t("statistics.title")}
            </h1>
            <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
          </div>
        </div>
        <RangeSelector value={range} onChange={setRange} />
      </div>

      {isEmpty ? (
        <EmptyState
          icon={<BarChart2 size={40} />}
          title={t("statistics.emptyTitle")}
          description={t("statistics.emptyDescription")}
          className="py-32"
        />
      ) : (
        <>
          {/* KPIs */}
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <KpiCard
              icon={<PlayCircle size={18} />}
              label={t("statistics.kpi.totalPlays")}
              value={overview ? formatCount(overview.total_plays, locale) : "—"}
            />
            <KpiCard
              icon={<Clock size={18} />}
              label={t("statistics.kpi.totalTime")}
              value={overview ? formatListenTime(overview.total_ms) : "—"}
            />
            <KpiCard
              icon={<Music2 size={18} />}
              label={t("statistics.kpi.uniqueTracks")}
              value={
                overview ? formatCount(overview.unique_tracks, locale) : "—"
              }
              hint={
                overview
                  ? t("statistics.kpi.uniqueArtists", {
                      count: overview.unique_artists,
                    })
                  : undefined
              }
            />
            <KpiCard
              icon={<Disc3 size={18} />}
              label={t("statistics.kpi.completionRate")}
              value={
                overview ? formatPercent(overview.completion_rate, locale) : "—"
              }
            />
          </div>

          {/* Activity charts */}
          <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
            <section className="lg:col-span-2 rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5">
              <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400 mb-4">
                {t("statistics.byDay.title")}
              </h2>
              <BarChart
                data={dayData}
                height={180}
                thinLabels
                emptyText={t("statistics.byDay.empty")}
                ariaLabel={t("statistics.byDay.title")}
              />
            </section>
            <section className="rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5">
              <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400 mb-4">
                {t("statistics.byHour.title")}
              </h2>
              <BarChart
                data={hourData}
                height={180}
                thinLabels
                emptyText={t("statistics.byHour.empty")}
                ariaLabel={t("statistics.byHour.title")}
              />
            </section>
          </div>

          {/* Tops */}
          <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
            <TopList
              title={t("statistics.topTracks.title")}
              emptyText={t("statistics.topTracks.empty")}
            >
              {topTracks.map((track, i) => (
                <TopRow
                  key={track.track_id}
                  rank={i + 1}
                  artwork={
                    <Artwork
                      path={track.artwork_path}
                      className="w-10 h-10"
                      iconSize={16}
                      alt={track.title}
                    />
                  }
                  primary={track.title}
                  secondary={track.artist_name ?? "—"}
                  metric={t("statistics.plays", { count: track.plays })}
                  onClick={
                    track.album_id != null
                      ? () => onNavigateToAlbum(track.album_id as number)
                      : undefined
                  }
                />
              ))}
            </TopList>
            <TopList
              title={t("statistics.topArtists.title")}
              emptyText={t("statistics.topArtists.empty")}
            >
              {topArtists.map((artist, i) => (
                <TopRow
                  key={artist.artist_id}
                  rank={i + 1}
                  artwork={
                    artist.picture_url ? (
                      <img
                        src={artist.picture_url}
                        alt={artist.name}
                        loading="lazy"
                        className="w-10 h-10 rounded-full object-cover shrink-0 bg-zinc-100 dark:bg-zinc-800"
                      />
                    ) : (
                      <div
                        className="w-10 h-10 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 flex items-center justify-center text-violet-500/70 dark:text-violet-400/60 text-sm font-bold shrink-0"
                        aria-label={artist.name}
                      >
                        {artist.name.trim().charAt(0).toUpperCase() || "?"}
                      </div>
                    )
                  }
                  primary={artist.name}
                  secondary={formatListenTime(artist.listened_ms)}
                  metric={t("statistics.plays", { count: artist.plays })}
                  onClick={() => onNavigateToArtist(artist.artist_id)}
                />
              ))}
            </TopList>
            <TopList
              title={t("statistics.topAlbums.title")}
              emptyText={t("statistics.topAlbums.empty")}
            >
              {topAlbums.map((album, i) => (
                <TopRow
                  key={album.album_id}
                  rank={i + 1}
                  artwork={
                    <Artwork
                      path={album.artwork_path}
                      className="w-10 h-10"
                      iconSize={16}
                      alt={album.title}
                    />
                  }
                  primary={album.title}
                  secondary={album.artist_name ?? "—"}
                  metric={t("statistics.plays", { count: album.plays })}
                  onClick={() => onNavigateToAlbum(album.album_id)}
                />
              ))}
            </TopList>
          </div>
        </>
      )}
    </div>
  );
}
