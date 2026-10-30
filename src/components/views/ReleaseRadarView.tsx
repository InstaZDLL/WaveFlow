import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Disc3,
  ExternalLink,
  ListMusic,
  Loader2,
  Play,
  Radar,
  Puzzle,
  RefreshCw,
  Square,
  WifiOff,
  X,
} from "lucide-react";

import { usePageScroll } from "../../hooks/usePageScroll";
import { useReleaseRadarPluginNotice } from "../../hooks/useReleaseRadarPluginNotice";
import { useTasks } from "../../hooks/useTasks";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { openUrl } from "../../lib/tauri/external";
import { getOfflineMode } from "../../lib/tauri/offline";
import { playerPlayTracks } from "../../lib/tauri/player";
import { cancelTask } from "../../lib/tauri/tasks";
import {
  RELEASE_RADAR_CHANGED,
  releaseRadarList,
  releaseRadarRefreshPlaylist,
  releaseRadarScan,
  type RadarRelease,
  type ReleaseRadarScanOutcome,
  type ReleaseRadarSnapshot,
} from "../../lib/tauri/releaseRadar";
import { EmptyState } from "../common/EmptyState";

interface ReleaseRadarViewProps {
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToPlaylist: (playlistId: number) => void;
  /** The extensions page, where the replaced plugin is uninstalled. */
  onNavigateToExtensions: () => void;
}

type Filter = "all" | "playable" | "discovered";

const FILTERS: Filter[] = ["all", "playable", "discovered"];

/** The release types MusicBrainz uses that the view names; any other is
 *  left unnamed rather than shown in English. */
const PRIMARY_TYPES = ["album", "single", "ep", "broadcast", "other"];
const SECONDARY_TYPES = ["live", "compilation", "remix", "soundtrack"];

/**
 * Release Radar: the recent releases of the library's artists, found on
 * MusicBrainz, each either **in the library** (matched to local tracks,
 * playable here) or only **discovered** (a link to MusicBrainz). Nothing
 * is downloaded but the releases' information and covers, and only when
 * the user asks for a search; opening the view sends nothing.
 *
 * Whether a search is running is read from the task registry, not kept
 * here: the scan outlives the view, and a flag held in it would be lost
 * on the first trip to another page.
 */
export function ReleaseRadarView({
  onNavigateToArtist,
  onNavigateToPlaylist,
  onNavigateToExtensions,
}: ReleaseRadarViewProps) {
  const { t, i18n } = useTranslation();
  const [snapshot, setSnapshot] = useState<ReleaseRadarSnapshot | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [offline, setOffline] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const pluginNotice = useReleaseRadarPluginNotice();
  const tasks = useTasks();
  const task = tasks.find((row) => row.kind === "release_radar") ?? null;
  // The task row, or the backend's own flag for the moment before the
  // row exists — a view opened right then would offer a second scan.
  const scanning = task != null || starting || snapshot?.scanning === true;

  const description = t("home.releaseRadar.description");

  // A token per load, so a slow answer never paints over a newer one.
  const loadToken = useRef(0);
  const reload = useCallback(async () => {
    const token = ++loadToken.current;
    try {
      const next = await releaseRadarList();
      if (token === loadToken.current) setSnapshot(next);
    } catch (err) {
      console.error("[ReleaseRadarView] list failed", err);
    }
  }, []);

  // Subscribe first, then snapshot: an artist recorded between the two
  // would otherwise be missed until the next one.
  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    (async () => {
      try {
        const off = await listen(RELEASE_RADAR_CHANGED, () => {
          void reload();
        });
        if (cancelled) {
          off();
          return;
        }
        unlisten = off;
      } catch (err) {
        console.warn("[ReleaseRadarView] subscribe failed", err);
      }
      if (cancelled) return;
      await reload();
      // The playlist follows the library as it is now — an album
      // imported since the last search joins it. Local, no request; it
      // waits out a library scan, so the view is drawn first.
      try {
        await releaseRadarRefreshPlaylist(description);
      } catch (err) {
        console.warn("[ReleaseRadarView] playlist refresh failed", err);
      }
      if (!cancelled) await reload();
    })();
    getOfflineMode()
      .then((value) => {
        if (!cancelled) setOffline(value);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // A language change runs it again, which renames the playlist's
    // description too.
  }, [reload, description]);

  const outcomeNotice = useCallback(
    (outcome: ReleaseRadarScanOutcome): string | null => {
      switch (outcome) {
        case "rate_limited":
          return t("releaseRadar.outcome.rateLimited");
        case "failed":
          return t("releaseRadar.outcome.failed");
        case "cancelled":
          return t("releaseRadar.outcome.cancelled");
        case "offline":
          return t("releaseRadar.offline");
        case "profile_changed":
          return t("releaseRadar.outcome.profileChanged");
        default:
          return null;
      }
    },
    [t],
  );

  const startScan = useCallback(
    async (rescan: boolean) => {
      if (scanning || offline) return;
      setStarting(true);
      setNotice(null);
      try {
        const summary = await releaseRadarScan(rescan, description);
        if (summary.outcome === "offline") setOffline(true);
        setNotice(outcomeNotice(summary.outcome));
      } catch (err) {
        setNotice(t("releaseRadar.outcome.error", { detail: String(err) }));
      } finally {
        setStarting(false);
        void reload();
      }
    },
    [scanning, offline, description, outcomeNotice, reload, t],
  );

  const releases = useMemo(() => snapshot?.releases ?? [], [snapshot]);
  const playable = useMemo(
    () => releases.filter((r) => r.track_ids.length > 0),
    [releases],
  );
  const shown = useMemo(() => {
    if (filter === "playable") return playable;
    if (filter === "discovered")
      return releases.filter((r) => r.track_ids.length === 0);
    return releases;
  }, [filter, releases, playable]);
  const counts: Record<Filter, number> = {
    all: releases.length,
    playable: playable.length,
    discovered: releases.length - playable.length,
  };

  const playRelease = useCallback((release: RadarRelease) => {
    if (release.track_ids.length === 0) return;
    void playerPlayTracks(
      "manual",
      null,
      release.track_ids,
      0,
      release.matched_by === "album",
    ).catch((err) => console.error("[ReleaseRadarView] play failed", err));
  }, []);

  const playlistId = snapshot?.playlist_id ?? null;
  const playAll = useCallback(() => {
    const ids: number[] = [];
    const seen = new Set<number>();
    for (const release of playable) {
      for (const id of release.track_ids) {
        if (!seen.has(id)) {
          seen.add(id);
          ids.push(id);
        }
      }
    }
    if (ids.length === 0) return;
    // Every playable release, uncapped: more than the playlist holds, so
    // not played as the playlist.
    void playerPlayTracks("manual", null, ids, 0).catch((err) =>
      console.error("[ReleaseRadarView] play failed", err),
    );
  }, [playable]);

  const lastChecked = snapshot?.last_checked_at ?? null;
  const lastCheckedLabel =
    lastChecked != null
      ? new Intl.DateTimeFormat(i18n.language, {
          dateStyle: "medium",
          timeStyle: "short",
        }).format(new Date(lastChecked))
      : null;
  const neverScanned = snapshot != null && snapshot.checked === 0;

  return (
    <div className="mx-auto max-w-[1440px] space-y-6 animate-fade-in pb-20">
      {/* Header — the shape of the liked / history pages; `data-page-hero`
          is the skins' hook. */}
      <div
        data-page-hero
        className="flex flex-wrap items-start justify-between gap-4"
      >
        <div className="flex items-center space-x-6 min-w-0">
          <div className="w-24 h-24 shrink-0 rounded-2xl bg-cyan-100 text-cyan-600 flex items-center justify-center shadow-sm dark:bg-cyan-950/60 dark:text-cyan-400">
            <Radar size={48} />
          </div>
          <div className="min-w-0">
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("releaseRadar.badge")}
            </div>
            <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
              Release Radar
            </h1>
            <div className="text-sm text-zinc-500 mt-1">
              {t("releaseRadar.subtitle")}
            </div>
            {snapshot != null && (
              <div className="text-xs text-zinc-400 mt-1">
                {t("releaseRadar.followed", { n: snapshot.followed })}
                {" · "}
                {lastCheckedLabel
                  ? t("releaseRadar.lastChecked", { when: lastCheckedLabel })
                  : t("releaseRadar.neverChecked")}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Actions */}
      <div className="flex flex-wrap items-center gap-2">
        {scanning ? (
          <>
            <span className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-cyan-500/10 text-cyan-700 dark:text-cyan-300">
              <Loader2 size={16} className="animate-spin" />
              {task && task.total > 0
                ? t("releaseRadar.scanning", {
                    checked: task.current,
                    due: task.total,
                  })
                : t("releaseRadar.scanningIndeterminate")}
            </span>
            {task?.cancellable && (
              <button
                type="button"
                onClick={() => void cancelTask(task.id)}
                disabled={task.cancelling}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-zinc-100 text-zinc-700 hover:bg-zinc-200 disabled:opacity-50 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
              >
                <Square size={14} />
                {task.cancelling ? t("tasks.stopping") : t("releaseRadar.stop")}
              </button>
            )}
          </>
        ) : (
          <>
            <button
              type="button"
              onClick={() => void startScan(false)}
              disabled={offline}
              className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-cyan-600 text-white hover:bg-cyan-500 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Radar size={16} />
              {t("releaseRadar.scan")}
            </button>
            {!neverScanned && (
              <button
                type="button"
                onClick={() => void startScan(true)}
                disabled={offline}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-zinc-100 text-zinc-700 hover:bg-zinc-200 disabled:opacity-50 disabled:cursor-not-allowed dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
              >
                <RefreshCw size={14} />
                {t("releaseRadar.rescan")}
              </button>
            )}
          </>
        )}
        {playable.length > 0 && (
          <button
            type="button"
            onClick={playAll}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-400"
          >
            <Play size={14} fill="currentColor" />
            {t("releaseRadar.playAll")}
          </button>
        )}
        {playlistId != null && (
          <button
            type="button"
            onClick={() => onNavigateToPlaylist(playlistId)}
            className="inline-flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-zinc-100 text-zinc-700 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-200 dark:hover:bg-zinc-700"
          >
            <ListMusic size={14} />
            {t("releaseRadar.openPlaylist")}
          </button>
        )}
      </div>

      {pluginNotice.visible && (
        <div
          role="status"
          data-release-radar-plugin-notice
          className="flex items-start gap-3 rounded-xl px-4 py-3 text-sm bg-cyan-50 text-cyan-900 dark:bg-cyan-950/40 dark:text-cyan-200"
        >
          <Puzzle size={16} className="mt-0.5 shrink-0" />
          <div className="min-w-0 flex-1 space-y-2">
            <div>
              <div className="font-semibold">
                {t("releaseRadar.pluginNotice.title")}
              </div>
              <div>{t("releaseRadar.pluginNotice.body")}</div>
            </div>
            <button
              type="button"
              onClick={onNavigateToExtensions}
              className="inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-xs font-medium bg-cyan-600 text-white hover:bg-cyan-500"
            >
              {t("releaseRadar.pluginNotice.openExtensions")}
            </button>
          </div>
          <button
            type="button"
            onClick={pluginNotice.dismiss}
            aria-label={t("common.close")}
            title={t("common.close")}
            className="shrink-0 rounded-full p-1 hover:bg-cyan-100 dark:hover:bg-cyan-900/50"
          >
            <X size={14} />
          </button>
        </div>
      )}
      {offline && (
        <div className="flex items-start gap-2 rounded-xl px-4 py-3 text-sm bg-amber-50 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <WifiOff size={16} className="mt-0.5 shrink-0" />
          <span>{t("releaseRadar.offline")}</span>
        </div>
      )}
      {notice && !offline && (
        <div
          role="status"
          className="rounded-xl px-4 py-3 text-sm bg-zinc-100 text-zinc-700 dark:bg-zinc-800/70 dark:text-zinc-300"
        >
          {notice}
        </div>
      )}

      {releases.length > 0 && (
        <div className="flex flex-wrap gap-2" role="group">
          {FILTERS.map((id) => (
            <button
              key={id}
              type="button"
              aria-pressed={filter === id}
              onClick={() => setFilter(id)}
              className={`px-3 py-1.5 rounded-full text-xs font-medium transition-colors ${
                filter === id
                  ? "bg-cyan-600 text-white"
                  : "bg-zinc-100 text-zinc-600 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
              }`}
            >
              {t(`releaseRadar.filter.${id}`)}
              <span className="ml-1.5 opacity-70 tabular-nums">
                {counts[id]}
              </span>
            </button>
          ))}
        </div>
      )}

      {snapshot == null ? null : releases.length === 0 ? (
        <EmptyState
          icon={<Radar size={40} />}
          title={
            neverScanned
              ? t("releaseRadar.emptyTitle")
              : t("releaseRadar.noneTitle")
          }
          description={
            neverScanned
              ? t("releaseRadar.emptyHint")
              : t("releaseRadar.noneHint")
          }
          accent="blue"
          shape="circle"
          className="py-20"
        />
      ) : shown.length === 0 ? (
        <div className="py-16 text-center text-sm text-zinc-500">
          {t("releaseRadar.filterEmpty")}
        </div>
      ) : (
        <ReleaseGrid
          releases={shown}
          onPlay={playRelease}
          onNavigateToArtist={onNavigateToArtist}
        />
      )}

      <div className="text-[11px] text-zinc-400">
        {t("releaseRadar.credits")}
      </div>
    </div>
  );
}

/**
 * The releases as tiles, row-virtualized against the page scroller like
 * the albums and playlists grids: a hundred artists can bring a few
 * hundred releases, and each tile mounts a cover.
 */
function ReleaseGrid({
  releases,
  onPlay,
  onNavigateToArtist,
}: {
  releases: RadarRelease[];
  onPlay: (release: RadarRelease) => void;
  onNavigateToArtist: (artistId: number) => void;
}) {
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(1);
  const [tileWidth, setTileWidth] = useState(180);
  const [scrollMargin, setScrollMargin] = useState(0);

  // The albums grid's metrics, so the two read as one system.
  const MIN_TILE = 180;
  const GAP = 20;
  // Square cover + title, credit, date and the state line.
  const tileHeight = tileWidth + 104;

  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const recompute = () => {
      const width = el.getBoundingClientRect().width;
      if (width === 0) return;
      const n = Math.max(1, Math.floor((width + GAP) / (MIN_TILE + GAP)));
      setColCount(n);
      setTileWidth((width - (n - 1) * GAP) / n);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Offset the virtual rows by the grid's place in the page scroller.
  useLayoutEffect(() => {
    const parent = parentRef.current;
    const scroller = pageScrollRef?.current;
    if (!parent || !scroller) return;
    const recompute = () => {
      const pr = parent.getBoundingClientRect();
      const sr = scroller.getBoundingClientRect();
      setScrollMargin(pr.top - sr.top + scroller.scrollTop);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(parent);
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [pageScrollRef, releases.length]);

  const rowCount = Math.ceil(releases.length / colCount);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => tileHeight + GAP,
    overscan: 2,
    scrollMargin,
  });

  useEffect(() => {
    virtualizer.measure();
  }, [virtualizer, tileHeight]);

  return (
    <div ref={parentRef}>
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((row) => {
          const start = row.index * colCount;
          const rowItems = releases.slice(start, start + colCount);
          return (
            <div
              key={row.key}
              // Measured, not only estimated: a title of two badges can
              // wrap and make a row taller than the estimate.
              ref={virtualizer.measureElement}
              data-index={row.index}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${row.start - scrollMargin}px)`,
                display: "grid",
                gridTemplateColumns: `repeat(${colCount}, minmax(0, 1fr))`,
                gap: `${GAP}px`,
                paddingBottom: `${GAP}px`,
              }}
            >
              {rowItems.map((release) => (
                <ReleaseCard
                  key={release.id}
                  release={release}
                  onPlay={onPlay}
                  onNavigateToArtist={onNavigateToArtist}
                />
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** A MusicBrainz date at the precision it was given in. */
function formatReleaseDate(date: string, locale: string): string {
  const [y, m, d] = date.split("-").map((part) => Number(part));
  if (!y) return date;
  if (!m) return String(y);
  const value = new Date(Date.UTC(y, m - 1, d || 1));
  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "short",
    ...(d ? { day: "numeric" } : {}),
    timeZone: "UTC",
  }).format(value);
}

/** Whether the release is announced for a day after today. */
function isUpcoming(date: string): boolean {
  const today = new Date().toISOString().slice(0, 10);
  return date.length === 10 && date > today;
}

function ReleaseCard({
  release,
  onPlay,
  onNavigateToArtist,
}: {
  release: RadarRelease;
  onPlay: (release: RadarRelease) => void;
  onNavigateToArtist: (artistId: number) => void;
}) {
  const { t, i18n } = useTranslation();
  // A cached file only: the view never loads a remote image.
  const coverUrl = resolveRemoteImage(release.cover_path, null);
  const isPlayable = release.track_ids.length > 0;
  const primary = release.primary_type?.toLowerCase() ?? null;
  const badges = [
    ...(primary && PRIMARY_TYPES.includes(primary) ? [primary] : []),
    ...release.secondary_types
      .map((s) => s.toLowerCase())
      .filter((s) => SECONDARY_TYPES.includes(s)),
  ];

  return (
    <div
      data-release-card
      data-release-state={isPlayable ? "playable" : "discovered"}
      className="group flex flex-col space-y-2 min-w-0"
    >
      <div className="relative">
        {coverUrl ? (
          <img
            src={coverUrl}
            alt=""
            loading="lazy"
            className={`w-full aspect-square rounded-2xl object-cover shadow-sm ${
              isPlayable ? "" : "opacity-80"
            }`}
          />
        ) : (
          <div className="w-full aspect-square rounded-2xl flex items-center justify-center shadow-sm bg-linear-to-br from-cyan-400 to-teal-700 text-white/80">
            <Disc3 size={44} />
          </div>
        )}
        {isPlayable ? (
          <button
            type="button"
            onClick={() => onPlay(release)}
            aria-label={t("releaseRadar.play", { title: release.title })}
            title={t("releaseRadar.play", { title: release.title })}
            className="absolute bottom-3 right-3 w-11 h-11 rounded-full bg-emerald-500 text-white flex items-center justify-center shadow-lg opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity hover:bg-emerald-400 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-300"
          >
            <Play size={18} fill="currentColor" />
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void openUrl(release.musicbrainz_url)}
            aria-label={t("releaseRadar.openMusicBrainz")}
            title={t("releaseRadar.openMusicBrainz")}
            className="absolute bottom-3 right-3 w-10 h-10 rounded-full bg-white/90 text-zinc-800 flex items-center justify-center shadow-lg opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity hover:bg-white focus:outline-none focus-visible:ring-2 focus-visible:ring-cyan-400"
          >
            <ExternalLink size={16} />
          </button>
        )}
      </div>
      <div className="min-w-0">
        <div
          className="text-sm font-medium text-zinc-900 dark:text-white truncate"
          title={release.title}
        >
          {release.title}
        </div>
        <button
          type="button"
          onClick={() => onNavigateToArtist(release.artist_id)}
          className="block max-w-full text-xs text-zinc-500 dark:text-zinc-400 truncate hover:underline text-left"
          title={release.artist_credit}
        >
          {release.artist_credit || release.artist_name}
        </button>
        <div className="flex flex-wrap items-center gap-1 mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
          <span className="tabular-nums">
            {formatReleaseDate(release.first_release_date, i18n.language)}
          </span>
          {isUpcoming(release.first_release_date) && (
            <span className="px-1.5 py-0.5 rounded bg-violet-100 text-violet-700 dark:bg-violet-950/60 dark:text-violet-300">
              {t("releaseRadar.upcoming")}
            </span>
          )}
          {badges.map((badge) => (
            <span
              key={badge}
              className="px-1.5 py-0.5 rounded bg-zinc-200 text-zinc-600 dark:bg-zinc-700 dark:text-zinc-300"
            >
              {t(`releaseRadar.type.${badge}`)}
            </span>
          ))}
        </div>
        <div className="mt-1 text-[11px] font-medium truncate">
          {isPlayable ? (
            <span className="text-emerald-600 dark:text-emerald-400">
              {t("releaseRadar.state.playable", {
                n: release.track_ids.length,
              })}
            </span>
          ) : (
            <button
              type="button"
              onClick={() => void openUrl(release.musicbrainz_url)}
              className="text-cyan-700 dark:text-cyan-400 hover:underline"
            >
              {t("releaseRadar.state.discovered")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
