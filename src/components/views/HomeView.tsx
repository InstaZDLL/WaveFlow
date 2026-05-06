import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Folder, Library, Heart, Clock, ListMusic, Music2 } from "lucide-react";
import type { ViewId } from "../../types";
import { ActionLink } from "../common/ActionLink";
import { StatCard } from "../common/StatCard";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { useProfile } from "../../hooks/useProfile";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { pickFolder } from "../../lib/tauri/dialog";
import {
  getProfileStats,
  listAlbums,
  listRecentPlays,
  type AlbumRow,
  type RecentPlay,
} from "../../lib/tauri/browse";
import type { Track } from "../../lib/tauri/track";

interface HomeViewProps {
  onNavigate: (view: ViewId) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

const RECENT_LIMIT = 12;
const ALBUMS_LIMIT = 12;

const WAVEFORM_BAR_COUNT = 80;
const WAVEFORM_HEIGHTS = Array.from({ length: WAVEFORM_BAR_COUNT }, (_, i) => {
  const x = i / (WAVEFORM_BAR_COUNT - 1);
  const wave =
    Math.sin(x * Math.PI * 5) * 0.55 +
    Math.sin(x * Math.PI * 11 + 0.6) * 0.3 +
    Math.sin(x * Math.PI * 19 + 1.2) * 0.18;
  const envelope = Math.sin(x * Math.PI);
  return Math.max(0.1, Math.abs(wave) * envelope * 0.95 + envelope * 0.15);
});

function getGreetingKey(): "morning" | "evening" | "night" {
  const hour = new Date().getHours();
  if (hour >= 5 && hour < 18) return "morning";
  if (hour >= 18 && hour < 23) return "evening";
  return "night";
}

// `RecentPlay` is a flat row from the SQL query and lacks the codec /
// quality columns the player uses for diagnostics. Filling them with
// nulls is fine — the player already treats those as optional.
function recentPlayToTrack(rp: RecentPlay): Track {
  return {
    id: rp.track_id,
    library_id: 0,
    title: rp.title,
    album_id: rp.album_id,
    album_title: rp.album_title,
    artist_id: rp.artist_id,
    artist_name: rp.artist_name,
    artist_ids: rp.artist_ids,
    duration_ms: rp.duration_ms,
    track_number: null,
    disc_number: null,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: null,
    musical_key: null,
    file_path: rp.file_path,
    file_size: 0,
    added_at: 0,
    artwork_path: rp.artwork_path,
    artwork_path_1x: rp.artwork_path_1x,
    artwork_path_2x: rp.artwork_path_2x,
    rating: null,
  };
}

export function HomeView({
  onNavigate,
  onNavigateToAlbum,
  onNavigateToArtist,
}: HomeViewProps) {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
  } = useLibrary();
  const { playTracks, playbackState, currentTrack } = usePlayer();
  const { playlists } = usePlaylist();

  const [likedCount, setLikedCount] = useState(0);
  const [recentCount, setRecentCount] = useState(0);
  const [recentPlays, setRecentPlays] = useState<RecentPlay[]>([]);
  const [recentAlbums, setRecentAlbums] = useState<AlbumRow[]>([]);
  const [isImporting, setIsImporting] = useState(false);

  const greetingName = activeProfile?.name ?? "";
  const totalTracks = useMemo(
    () => libraries.reduce((sum, lib) => sum + lib.track_count, 0),
    [libraries],
  );
  const hasLibrary = libraries.length > 0;

  // Profile-wide counters refresh on profile change AND on every track-end
  // so the "Joués récemment" card reflects the freshly-inserted play_event.
  useEffect(() => {
    let cancelled = false;
    getProfileStats()
      .then((stats) => {
        if (cancelled) return;
        setLikedCount(stats.liked_count);
        setRecentCount(stats.recent_plays_count);
      })
      .catch((err) => console.error("[HomeView] profile stats failed", err));
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id, playbackState]);

  // Recent plays carousel — same trigger as the dedicated RecentView so
  // the home tile updates in lockstep with the full history page.
  useEffect(() => {
    let cancelled = false;
    listRecentPlays(null, RECENT_LIMIT)
      .then((rows) => {
        if (!cancelled) setRecentPlays(rows);
      })
      .catch((err) => console.error("[HomeView] list recent plays", err));
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id, playbackState]);

  // "Récemment ajoutés" — re-fetch when any library gets rescanned (the
  // signature flips on `updated_at` change, just like LibraryView).
  const librariesSignature = libraries
    .map((l) => `${l.id}:${l.updated_at}`)
    .join(",");
  useEffect(() => {
    // The section is gated on `hasLibrary` in render, so we just skip
    // the fetch when there's nothing to load — no need to clear state
    // explicitly (and clearing in-effect trips eslint's set-state rule).
    if (!hasLibrary) return;
    let cancelled = false;
    listAlbums(null, { orderBy: "added_at", direction: "desc" })
      .then((rows) => {
        if (!cancelled) setRecentAlbums(rows.slice(0, ALBUMS_LIMIT));
      })
      .catch((err) => console.error("[HomeView] list albums", err));
    return () => {
      cancelled = true;
    };
  }, [hasLibrary, librariesSignature]);

  const handleImport = async () => {
    if (isImporting) return;
    try {
      const path = await pickFolder(t("library.actions.importFolder"));
      if (!path) return;
      setIsImporting(true);
      let libId = selectedLibraryId;
      if (libId == null) {
        if (libraries.length > 0) {
          libId = libraries[0].id;
          selectLibrary(libId);
        } else {
          const lib = await createLibrary({ name: "Ma musique" });
          libId = lib.id;
          selectLibrary(libId);
        }
      }
      await importFolder(libId, path);
    } catch (err) {
      console.error("[HomeView] import failed", err);
    } finally {
      setIsImporting(false);
    }
  };

  const handlePlayRecent = async (index: number) => {
    if (recentPlays.length === 0) return;
    const tracks = recentPlays.map(recentPlayToTrack);
    await playTracks(tracks, index, { type: "library", id: null });
  };

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Welcome Banner */}
      <div className="relative overflow-hidden rounded-3xl p-10 bg-linear-to-br from-emerald-50 to-white shadow-sm border border-emerald-100/50 dark:from-emerald-900/40 dark:to-zinc-800/40 dark:border-zinc-800 dark:shadow-none">
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -top-24 -left-16 w-80 h-80 rounded-full bg-emerald-300/30 dark:bg-emerald-400/25 blur-3xl"
        />
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -bottom-32 right-0 w-md h-112 rounded-full bg-emerald-400/20 dark:bg-emerald-500/20 blur-3xl"
        />

        <div className="relative">
          <div className="inline-flex items-center space-x-2 bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-500/40 dark:border-emerald-400/40 px-3 py-1 rounded-full text-xs font-semibold mb-6 backdrop-blur-sm">
            <div className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse" />
            <span>{t("home.banner.badge")}</span>
          </div>

          <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
            {t(`home.greeting.${getGreetingKey()}`)}
            {greetingName && `, ${greetingName}`}
          </h1>
          <p className="text-zinc-500 dark:text-zinc-400 mb-8">
            {t("home.banner.subtitle")}
          </p>

          <div className="flex flex-wrap gap-6">
            <ActionLink
              icon={<Folder size={16} />}
              label={
                isImporting
                  ? t("library.actions.importing")
                  : t("home.banner.importFolder")
              }
              highlight
              onClick={handleImport}
            />
            {hasLibrary && (
              <ActionLink
                icon={<Library size={16} />}
                label={t("home.banner.browseLibrary")}
                onClick={() => onNavigate("library")}
              />
            )}
          </div>
        </div>
      </div>

      {/* Stats Cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <StatCard
          icon={<Library />}
          accent="emerald"
          count={totalTracks.toString()}
          label={t("home.stats.library")}
          onClick={() => onNavigate("library")}
        />
        <StatCard
          icon={<Heart className="fill-current" />}
          accent="pink"
          count={likedCount.toString()}
          label={t("home.stats.liked")}
          onClick={() => onNavigate("liked")}
        />
        <StatCard
          icon={<Clock />}
          accent="blue"
          count={recentCount.toString()}
          label={t("home.stats.recent")}
          onClick={() => onNavigate("recent")}
        />
        <StatCard
          icon={<ListMusic />}
          accent="purple"
          count={playlists.length.toString()}
          label={t("home.stats.playlists")}
        />
      </div>

      {/* Récemment joués */}
      <section>
        <div className="flex items-end justify-between mb-6">
          <h2 className="text-2xl font-bold inline-block border-b-4 border-emerald-500 pb-1 text-zinc-900 dark:text-white">
            {t("home.recentlyPlayed.title")}
          </h2>
          {recentPlays.length > 0 && (
            <button
              type="button"
              onClick={() => onNavigate("recent")}
              className="text-sm font-medium text-emerald-600 hover:text-emerald-700 dark:text-emerald-400 dark:hover:text-emerald-300 transition-colors"
            >
              {t("home.seeAll")}
            </button>
          )}
        </div>
        {recentPlays.length === 0 ? (
          <div className="relative overflow-hidden min-h-80 rounded-3xl border flex items-center justify-center p-8 border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/40 dark:shadow-none">
            <EmptyState
              icon={<Clock size={32} />}
              title={t("home.recentlyPlayed.emptyTitle")}
              description={t("home.recentlyPlayed.emptyDescription")}
              size="sm"
            >
              <svg
                viewBox="0 0 400 40"
                preserveAspectRatio="none"
                aria-hidden="true"
                className="mt-8 w-96 h-10 text-emerald-400 dark:text-emerald-400/60"
              >
                {WAVEFORM_HEIGHTS.map((h, i) => {
                  const barWidth = 2.5;
                  const gap = 2.5;
                  const totalWidth =
                    WAVEFORM_BAR_COUNT * (barWidth + gap) - gap;
                  const startX = (400 - totalWidth) / 2;
                  const x = startX + i * (barWidth + gap);
                  const barH = h * 36;
                  const y = (40 - barH) / 2;
                  return (
                    <rect
                      key={i}
                      x={x}
                      y={y}
                      width={barWidth}
                      height={barH}
                      rx={1}
                      fill="currentColor"
                    />
                  );
                })}
              </svg>
            </EmptyState>
          </div>
        ) : (
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6 gap-4">
            {recentPlays.map((play, idx) => {
              const isCurrent = play.track_id === currentTrack?.id;
              return (
                <button
                  key={`${play.track_id}-${play.played_at}`}
                  type="button"
                  onDoubleClick={() => handlePlayRecent(idx)}
                  onClick={() => handlePlayRecent(idx)}
                  className="group flex flex-col items-stretch text-left rounded-2xl p-3 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
                >
                  <Artwork
                    path={play.artwork_path}
                    path1x={play.artwork_path_1x}
                    path2x={play.artwork_path_2x}
                    size="2x"
                    alt={play.album_title ?? play.title}
                    className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                    iconSize={36}
                    rounded="2xl"
                  />
                  <div className="mt-3 min-w-0">
                    <div
                      className={`text-sm font-semibold truncate ${
                        isCurrent
                          ? "text-emerald-600 dark:text-emerald-400"
                          : "text-zinc-800 dark:text-zinc-200"
                      }`}
                    >
                      {play.title}
                    </div>
                    <ArtistLink
                      name={play.artist_name}
                      artistIds={play.artist_ids}
                      onNavigate={onNavigateToArtist}
                      fallback="—"
                      className="text-xs text-zinc-500 truncate block"
                    />
                  </div>
                </button>
              );
            })}
          </div>
        )}
      </section>

      {/* Récemment ajoutés — only render when we have albums to show. */}
      {hasLibrary && recentAlbums.length > 0 && (
        <section>
          <div className="flex items-end justify-between mb-6">
            <h2 className="text-2xl font-bold inline-block border-b-4 border-emerald-500 pb-1 text-zinc-900 dark:text-white">
              {t("home.recentlyAdded.title")}
            </h2>
            <button
              type="button"
              onClick={() => onNavigate("library")}
              className="text-sm font-medium text-emerald-600 hover:text-emerald-700 dark:text-emerald-400 dark:hover:text-emerald-300 transition-colors"
            >
              {t("home.seeAll")}
            </button>
          </div>
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-6 gap-4">
            {recentAlbums.map((album) => (
              <button
                key={album.id}
                type="button"
                onClick={() => onNavigateToAlbum(album.id)}
                className="group flex flex-col items-stretch text-left rounded-2xl p-3 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
              >
                <Artwork
                  path={album.artwork_path}
                  path1x={album.artwork_path_1x}
                  path2x={album.artwork_path_2x}
                  size="2x"
                  alt={album.title}
                  className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                  iconSize={36}
                  rounded="2xl"
                />
                <div className="mt-3 min-w-0">
                  <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
                    {album.title}
                  </div>
                  <div className="text-xs text-zinc-500 truncate flex items-center gap-1">
                    <Music2 size={11} className="shrink-0" />
                    <span className="truncate">
                      {album.artist_name ?? t("library.table.unknown")}
                    </span>
                  </div>
                </div>
              </button>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
