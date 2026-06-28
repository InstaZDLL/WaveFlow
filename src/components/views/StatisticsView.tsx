import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  BarChart2,
  Clock,
  Disc3,
  Download,
  Music2,
  PlayCircle,
} from "lucide-react";
import type { ViewId } from "../../types";
import { Artwork } from "../common/Artwork";
import { EmptyState } from "../common/EmptyState";
import {
  exportStatsJson,
  statsListeningByDay,
  statsListeningByHour,
  statsOverview,
  statsTopAlbums,
  statsTopArtists,
  statsTopGenres,
  statsTopTracks,
  type ListeningByDayRow,
  type StatsOverview,
  type StatsRange,
  type TopAlbumRow,
  type TopArtistRow,
  type TopGenreRow,
  type TopTrackRow,
} from "../../lib/tauri/stats";
import { pickSaveFile } from "../../lib/tauri/dialog";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { useHiddenKpis, type StatsKpiId } from "../../hooks/useHiddenKpis";
import { RangeSelector } from "./statistics/RangeSelector";
import { KpiCard } from "./statistics/KpiCard";
import { BarChart } from "./statistics/BarChart";
import { Heatmap } from "./statistics/Heatmap";
import { TopGenres } from "./statistics/TopGenres";
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
  const [topGenres, setTopGenres] = useState<TopGenreRow[]>([]);
  const [heatmapData, setHeatmapData] = useState<ListeningByDayRow[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [exporting, setExporting] = useState(false);
  const { isHidden, ready: kpiPrefsReady } = useHiddenKpis();

  // Export the on-screen stats (top 100 each, listening by day/hour,
  // overview) as a pretty-printed JSON file. The backend already
  // matches the on-screen shapes byte-for-byte, so no transformation
  // is needed here — just save the string verbatim.
  const handleExport = async () => {
    if (exporting) return;
    setExporting(true);
    try {
      const defaultName = `waveflow-stats-${range}-${new Date()
        .toISOString()
        .slice(0, 10)}.json`;
      const target = await pickSaveFile(
        defaultName,
        ["json"],
        t("statistics.export.dialogTitle"),
      );
      if (!target) return;
      await exportStatsJson(range, target);
    } catch (err) {
      console.error("[StatisticsView] export_stats_json failed", err);
    } finally {
      setExporting(false);
    }
  };

  // Heatmap always covers the past year regardless of the user's
  // selected range — that's the whole point of the contributions-style
  // visual. Fetch once on mount; no need to refetch when `range`
  // changes.
  useEffect(() => {
    let cancelled = false;
    statsListeningByDay("1y")
      .then((rows) => {
        if (!cancelled) setHeatmapData(rows);
      })
      .catch((err) => {
        console.error("[StatisticsView] heatmap load failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

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
      statsTopGenres(range, TOP_LIMIT),
    ])
      .then(([ov, day, hour, tracks, artists, albums, genres]) => {
        if (cancelled) return;
        setOverview(ov);
        setByDay(day);
        setByHour(hour);
        setTopTracks(tracks);
        setTopArtists(artists);
        setTopAlbums(albums);
        setTopGenres(genres);
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
    <div className="space-y-8 animate-fade-in pb-20">
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
        <div className="flex items-center gap-2">
          <RangeSelector value={range} onChange={setRange} />
          <button
            type="button"
            onClick={handleExport}
            disabled={exporting || isEmpty}
            aria-label={t("statistics.export.label")}
            title={t("statistics.export.label")}
            className="inline-flex items-center gap-2 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-xs font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Download size={14} className={exporting ? "animate-pulse" : ""} />
            <span>{t("statistics.export.action")}</span>
          </button>
        </div>
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
          {/* KPIs — each card declares a stable id so the user can hide
              it from Settings → Appearance (`stats.hidden_kpis`). A
              fully-hidden grid is intentional: it renders nothing. */}
          {(() => {
            // Hold the KPI grid back until the hidden-cards preference
            // has loaded, otherwise a hidden card flashes visible for
            // one frame before the read lands.
            if (!kpiPrefsReady) return null;
            const kpis: Array<{ id: StatsKpiId; node: React.ReactNode }> = [
              {
                id: "total_plays",
                node: (
                  <KpiCard
                    icon={<PlayCircle size={18} />}
                    label={t("statistics.kpi.totalPlays")}
                    value={
                      overview ? formatCount(overview.total_plays, locale) : "—"
                    }
                  />
                ),
              },
              {
                id: "total_time",
                node: (
                  <KpiCard
                    icon={<Clock size={18} />}
                    label={t("statistics.kpi.totalTime")}
                    value={overview ? formatListenTime(overview.total_ms) : "—"}
                  />
                ),
              },
              {
                id: "unique_tracks",
                node: (
                  <KpiCard
                    icon={<Music2 size={18} />}
                    label={t("statistics.kpi.uniqueTracks")}
                    value={
                      overview
                        ? formatCount(overview.unique_tracks, locale)
                        : "—"
                    }
                    hint={
                      overview
                        ? t("statistics.kpi.uniqueArtists", {
                            count: overview.unique_artists,
                          })
                        : undefined
                    }
                  />
                ),
              },
              {
                id: "completion_rate",
                node: (
                  <KpiCard
                    icon={<Disc3 size={18} />}
                    label={t("statistics.kpi.completionRate")}
                    value={
                      overview
                        ? formatPercent(overview.completion_rate, locale)
                        : "—"
                    }
                  />
                ),
              },
            ];
            const visible = kpis.filter((k) => !isHidden(k.id));
            if (visible.length === 0) return null;
            return (
              <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                {visible.map((k) => (
                  <div key={k.id}>{k.node}</div>
                ))}
              </div>
            );
          })()}

          {/* Yearly heatmap (GitHub-contributions style) */}
          <section className="rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900/60 p-5">
            <h2 className="text-sm font-semibold uppercase tracking-wide text-zinc-500 dark:text-zinc-400 mb-4">
              {t("statistics.heatmap.title")}
            </h2>
            <Heatmap data={heatmapData} />
          </section>

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

          {/* Per-genre listening breakdown */}
          <TopGenres data={topGenres} />

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
              {topArtists.map((artist, i) => {
                const artistSrc = resolveRemoteImage(
                  artist.picture_path,
                  artist.picture_url,
                );
                return (
                  <TopRow
                    key={artist.artist_id}
                    rank={i + 1}
                    artwork={
                      artistSrc ? (
                        <img
                          src={artistSrc}
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
                );
              })}
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
