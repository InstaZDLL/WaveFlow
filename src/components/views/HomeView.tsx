import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
} from "react";
import { useTranslation } from "react-i18next";
import {
  Folder,
  Library,
  Heart,
  Clock,
  ListMusic,
  Music2,
  Sparkles,
  RefreshCw,
  X,
} from "lucide-react";
import { useWrappedBannerVisibility } from "../../hooks/useWrappedBannerVisibility";
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
import { regenerateAllSmartPlaylists } from "../../lib/tauri/smart_playlists";
import { smartPlaylistKind } from "../../lib/tauri/playlist";
import { availableWrappedYears } from "../../lib/tauri/wrapped";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { useSkin } from "../../hooks/useSkin";
import {
  getProfileStats,
  listAlbums,
  listRecentPlays,
  type AlbumRow,
  type RecentPlay,
} from "../../lib/tauri/browse";
import type { Track } from "../../lib/tauri/track";
import { MoodRadioGrid } from "./home/MoodRadioGrid";
import { EditorialMasthead } from "./home/EditorialMasthead";

interface HomeViewProps {
  onNavigate: (view: ViewId) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToPlaylist: (playlistId: number) => void;
  onNavigateToWrapped: (year: number | null) => void;
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
  onNavigateToPlaylist,
  onNavigateToWrapped,
}: HomeViewProps) {
  const { t } = useTranslation();
  const { skin } = useSkin();
  const { activeProfile } = useProfile();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
  } = useLibrary();
  const { playTracks, playbackState, currentTrack } = usePlayer();
  const {
    playlists,
    isLoading: playlistsLoading,
    refresh: refreshPlaylists,
  } = usePlaylist();
  const [isRegenerating, setIsRegenerating] = useState(false);

  // Smart playlists are stored in the same `playlist` table as user
  // playlists; the only thing distinguishing them in the UI is the
  // `is_smart` flag. Filter here so the "Made for you" carousel only
  // shows the auto-generated ones — user playlists keep their dedicated
  // sidebar list.
  const smartPlaylists = useMemo(
    () => playlists.filter((p) => p.is_smart === 1),
    [playlists],
  );

  const handleRegenerateMixes = useCallback(async () => {
    if (isRegenerating) return;
    setIsRegenerating(true);
    try {
      // Single round-trip regenerates Daily Mix slots + On Repeat so
      // the whole "Made for you" carousel refreshes together — the
      // frontend doesn't need to know about the family split.
      await regenerateAllSmartPlaylists();
      await refreshPlaylists();
    } catch (err) {
      // Non-fatal — empty libraries / not enough listening data return
      // empty payloads rather than throw, so a hard error here means a
      // SQL or filesystem issue worth surfacing in the console.
      console.error("[HomeView] regenerate smart playlists failed", err);
    } finally {
      setIsRegenerating(false);
    }
  }, [isRegenerating, refreshPlaylists]);

  const [likedCount, setLikedCount] = useState(0);
  const [recentCount, setRecentCount] = useState(0);
  const [recentPlays, setRecentPlays] = useState<RecentPlay[]>([]);
  const [recentAlbums, setRecentAlbums] = useState<AlbumRow[]>([]);
  const [isImporting, setIsImporting] = useState(false);
  const [wrappedYears, setWrappedYears] = useState<number[]>([]);
  const wrappedBanner = useWrappedBannerVisibility();
  // Per-section loading flags — start true so each carousel paints a
  // skeleton on first render rather than flashing its (large) empty
  // state for the duration of the first SQL fetch.
  const [recentPlaysLoading, setRecentPlaysLoading] = useState(true);
  const [recentAlbumsLoading, setRecentAlbumsLoading] = useState(true);

  const greetingName = activeProfile?.name ?? "";
  const totalTracks = useMemo(
    () => libraries.reduce((sum, lib) => sum + lib.track_count, 0),
    [libraries],
  );
  const hasLibrary = libraries.length > 0;
  const isLoungeSkin = skin.id === "lounge";
  const isEditorialSkin = skin.id === "editorial";

  // Wrapped years — refresh whenever the profile changes; the list is
  // cheap (one DISTINCT over play_event) so we don't bother caching
  // across the session.
  useEffect(() => {
    let cancelled = false;
    availableWrappedYears()
      .then((years) => {
        if (!cancelled) setWrappedYears(years);
      })
      .catch((err) => console.error("[HomeView] wrapped years failed", err));
    return () => {
      cancelled = true;
    };
  }, [activeProfile?.id, playbackState]);

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
      .catch((err) => console.error("[HomeView] list recent plays", err))
      .finally(() => {
        if (!cancelled) setRecentPlaysLoading(false);
      });
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
      .catch((err) => console.error("[HomeView] list albums", err))
      .finally(() => {
        if (!cancelled) setRecentAlbumsLoading(false);
      });
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

  // Extracted from nested ternaries on the JSX — each branch is
  // a per-skin layout token (Lounge widens columns, Editorial
  // swaps in the front-page grid + hero modifier, baseline
  // stacks vertically). Keeping them named here makes the
  // matrix obvious instead of buried in three 4-level deep
  // ternaries inside the JSX className= props.
  let containerClasses: string;
  if (isLoungeSkin) {
    containerClasses = "lounge-home space-y-10 animate-fade-in pb-24";
  } else if (isEditorialSkin) {
    containerClasses = "editorial-home space-y-12 animate-fade-in pb-28";
  } else {
    containerClasses = "space-y-8 animate-fade-in pb-20";
  }

  let gridClasses: string;
  if (isLoungeSkin) {
    gridClasses =
      "grid grid-cols-1 xl:grid-cols-[minmax(0,1fr)_22rem] gap-5 items-stretch";
  } else if (isEditorialSkin) {
    gridClasses =
      "editorial-front-page grid grid-cols-1 xl:grid-cols-[minmax(0,1fr)_20rem] gap-8 items-stretch";
  } else {
    gridClasses = "space-y-8";
  }

  let bannerSkinClasses: string;
  if (isLoungeSkin) {
    bannerSkinClasses = "min-h-80 p-10 xl:p-12 flex items-end";
  } else if (isEditorialSkin) {
    bannerSkinClasses =
      "editorial-hero min-h-[23rem] p-8 sm:p-10 xl:p-12 flex items-end";
  } else {
    bannerSkinClasses = "p-10";
  }
  const bannerClasses = `relative overflow-hidden rounded-3xl bg-linear-to-br from-emerald-50 to-white shadow-sm border border-emerald-100/50 dark:from-emerald-900/40 dark:to-zinc-800/40 dark:border-zinc-800 dark:shadow-none ${bannerSkinClasses}`;
  // Safari (issue #414) fails to clip a `filter: blur()` child to its
  // `overflow-hidden` + `rounded-3xl` parent's corners — the blur's
  // expanded paint bounds leak past the rounded edge as square glow
  // patches. `overflow: hidden` alone doesn't trigger Safari's correct
  // clip path there; a mask does. Applied to every banner below that
  // has an absolutely-positioned `blur-3xl` decorative orb.
  const roundedClipMaskStyle: CSSProperties = {
    WebkitMaskImage: "-webkit-radial-gradient(white, black)",
    maskImage: "radial-gradient(white, black)",
  };

  return (
    <div className={containerClasses}>
      {isEditorialSkin && <EditorialMasthead />}
      <div className={gridClasses}>
        {/* Welcome Banner */}
        <div className={bannerClasses} style={roundedClipMaskStyle}>
          <div
            aria-hidden="true"
            className="pointer-events-none absolute -top-24 -left-16 w-80 h-80 rounded-full bg-emerald-300/30 dark:bg-emerald-400/25 blur-3xl"
          />
          <div
            aria-hidden="true"
            className="pointer-events-none absolute -bottom-32 right-0 w-md h-112 rounded-full bg-emerald-400/20 dark:bg-emerald-500/20 blur-3xl"
          />

          <div className="relative max-w-3xl">
            <div className="inline-flex items-center space-x-2 bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-500/40 dark:border-emerald-400/40 px-3 py-1 rounded-full text-xs font-semibold mb-6 backdrop-blur-sm">
              <div className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse" />
              <span>{t("home.banner.badge")}</span>
            </div>

            <h1
              className={`font-bold mb-2 text-zinc-900 dark:text-white ${
                isLoungeSkin
                  ? "text-5xl leading-tight"
                  : isEditorialSkin
                    ? "text-5xl md:text-6xl leading-[0.95]"
                    : "text-4xl"
              }`}
            >
              {t(`home.greeting.${getGreetingKey()}`)}
              {greetingName && `, ${greetingName}`}
            </h1>
            {isEditorialSkin && (
              <figure className="editorial-lead-art" aria-hidden="true">
                <div className="editorial-lead-art__frame">
                  {currentTrack ? (
                    <Artwork
                      path={currentTrack.artwork_path}
                      path1x={currentTrack.artwork_path_1x}
                      path2x={currentTrack.artwork_path_2x}
                      size="full"
                      alt=""
                      className="w-full h-full"
                      iconSize={42}
                      rounded="md"
                    />
                  ) : (
                    <div className="editorial-lead-art-fallback">
                      <Music2 size={60} />
                    </div>
                  )}
                </div>
                <figcaption className="editorial-lead-art__caption">
                  {currentTrack?.album_title && currentTrack?.artist_name
                    ? t("editorial.lead.caption", {
                        defaultValue:
                          "Fig 1. — Tiré de « {{album}} » par {{artist}}.",
                        album: currentTrack.album_title,
                        artist: currentTrack.artist_name,
                      })
                    : currentTrack?.artist_name
                      ? t("editorial.lead.captionArtistOnly", {
                          defaultValue: "Fig 1. — Une création de {{artist}}.",
                          artist: currentTrack.artist_name,
                        })
                      : t("editorial.lead.captionFallback", {
                          defaultValue: "Fig 1. — Sélection du jour.",
                        })}
                </figcaption>
              </figure>
            )}
            <p className="text-zinc-500 dark:text-zinc-400 mb-8 max-w-2xl">
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
        <div
          className={
            isLoungeSkin
              ? "grid grid-cols-2 xl:grid-cols-1 gap-4"
              : isEditorialSkin
                ? "editorial-stats grid grid-cols-2 xl:grid-cols-1 gap-0"
                : "grid grid-cols-1 md:grid-cols-4 gap-4"
          }
        >
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
      </div>

      {/* Wrapped year-in-review banner — gated by
          `useWrappedBannerVisibility`: hidden by default and auto-
          surfaces during the Wrapped season (Dec 1 → Jan 31). Users
          can force it on, force it off, or dismiss it for the current
          recap year from the close button below. */}
      {wrappedYears.length > 0 && wrappedBanner.shouldShow(wrappedYears[0]) && (
        <div
          className="relative overflow-hidden w-full text-left rounded-3xl p-8 group transition-transform hover:scale-[1.01]"
          style={{
            background:
              "linear-gradient(135deg,#1d0e3a 0%,#3a1052 50%,#7c2d12 100%)",
            ...roundedClipMaskStyle,
          }}
        >
          <div
            aria-hidden="true"
            className="pointer-events-none absolute -top-24 -right-16 w-80 h-80 rounded-full bg-fuchsia-400/30 blur-3xl"
          />
          <div
            aria-hidden="true"
            className="pointer-events-none absolute -bottom-32 -left-16 w-96 h-96 rounded-full bg-orange-400/25 blur-3xl"
          />
          {/* Dismiss button — persists the year so the banner stays
              hidden until the next recap year is available. Sits in
              its own stacking context above the navigate trigger so
              clicks don't fall through. */}
          <button
            type="button"
            onClick={() => wrappedBanner.dismissYear(wrappedYears[0])}
            aria-label={t("home.wrapped.dismiss")}
            className="absolute top-3 right-3 z-10 p-1.5 rounded-full text-white/70 hover:text-white hover:bg-white/15 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
          >
            <X size={18} aria-hidden="true" />
          </button>
          <button
            type="button"
            onClick={() => onNavigateToWrapped(wrappedYears[0])}
            className="relative w-full text-left flex items-center gap-6 text-white"
          >
            <div className="w-16 h-16 rounded-2xl bg-white/15 backdrop-blur-sm flex items-center justify-center">
              <Sparkles size={32} />
            </div>
            <div className="flex-1 min-w-0 pr-6">
              <div className="uppercase tracking-[0.4em] text-xs text-white/70 mb-1">
                {t("home.wrapped.eyebrow")}
              </div>
              <div className="text-3xl font-extrabold leading-tight">
                {t("home.wrapped.title", { year: wrappedYears[0] })}
              </div>
              <div className="text-sm text-white/70 mt-1">
                {t("home.wrapped.subtitle")}
              </div>
            </div>
            <div className="hidden md:inline-block px-4 py-2 rounded-full bg-white text-zinc-900 font-semibold text-sm group-hover:scale-105 transition-transform">
              {t("home.wrapped.cta")}
            </div>
          </button>
        </div>
      )}

      {/* Mood radios — BPM/loudness-filtered queues. Hidden entirely
          when the library has zero analysed tracks (component decides
          internally so HomeView stays declarative). */}
      <MoodRadioGrid />

      {/* Daily Mix — generated from listening history. Hidden when the
          user has too few play_events for a meaningful split (the
          backend returns an empty list and skips re-generation in that
          case); the regen button stays available so the section
          re-appears as soon as enough listening data piles up. */}
      <div
        className={
          isLoungeSkin
            ? "grid grid-cols-1 xl:grid-cols-2 gap-8 items-start"
            : isEditorialSkin
              ? "editorial-feature-grid grid grid-cols-1 xl:grid-cols-[minmax(0,0.95fr)_minmax(0,1.05fr)] gap-10 items-start"
              : "space-y-8"
        }
      >
        <section>
          <div className="flex items-end justify-between mb-6">
            <h2 className="text-2xl font-bold inline-block border-b-4 border-violet-500 pb-1 text-zinc-900 dark:text-white">
              {t("home.dailyMix.title", "Pour vous")}
            </h2>
            <button
              type="button"
              onClick={handleRegenerateMixes}
              disabled={isRegenerating}
              className="inline-flex items-center gap-2 text-sm font-medium text-violet-600 hover:text-violet-700 dark:text-violet-400 dark:hover:text-violet-300 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <RefreshCw
                size={14}
                className={isRegenerating ? "animate-spin" : ""}
              />
              {isRegenerating
                ? t("home.dailyMix.regenerating", "Génération…")
                : t("home.dailyMix.regenerate", "Régénérer")}
            </button>
          </div>
          {smartPlaylists.length === 0 && playlistsLoading ? (
            <HomeBannerSkeleton label={t("home.dailyMix.title", "Pour vous")} />
          ) : smartPlaylists.length === 0 ? (
            <div className="relative overflow-hidden min-h-32 rounded-3xl border flex items-center justify-center p-8 border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/40 dark:shadow-none">
              <EmptyState
                icon={<Sparkles size={32} />}
                title={t("home.dailyMix.emptyTitle", "Pas encore de Daily Mix")}
                description={t(
                  "home.dailyMix.emptyDescription",
                  "Écoute quelques morceaux puis clique sur Régénérer pour créer tes mixes personnalisés.",
                )}
                size="sm"
              />
            </div>
          ) : (
            <div className="flex flex-wrap gap-4">
              {smartPlaylists.map((pl) => {
                const cover = resolveRemoteImage(pl.cover_path, null);
                const kind = smartPlaylistKind(pl);
                // Per-family styling so On Repeat reads as a distinct
                // surface from Daily Mix — different eyebrow, gradient,
                // and focus ring tint without needing a separate carousel.
                const isOnRepeat = kind?.kind === "on_repeat";
                const eyebrow = isOnRepeat
                  ? t("home.onRepeat.label", "On Repeat")
                  : t("home.dailyMix.label", "Daily Mix");
                const fallbackGradient = isOnRepeat
                  ? "bg-linear-to-br from-emerald-400 to-emerald-700"
                  : "bg-linear-to-br from-violet-400 to-violet-700";
                const ringTint = isOnRepeat
                  ? "focus-visible:ring-emerald-500"
                  : "focus-visible:ring-violet-500";
                return (
                  <button
                    key={`smart-${pl.id}`}
                    type="button"
                    onClick={() => onNavigateToPlaylist(pl.id)}
                    className={`group relative overflow-hidden rounded-2xl bg-zinc-100 dark:bg-zinc-800 aspect-16/7 basis-70 grow max-w-100 text-left focus:outline-none focus-visible:ring-2 ${ringTint} transition-transform hover:-translate-y-0.5`}
                  >
                    {cover ? (
                      <img
                        src={cover}
                        alt=""
                        className="absolute inset-0 w-full h-full object-cover"
                        loading="lazy"
                      />
                    ) : (
                      <div className={`absolute inset-0 ${fallbackGradient}`} />
                    )}
                    <div className="absolute inset-0 bg-linear-to-t from-black/70 via-black/30 to-transparent" />
                    <div className="absolute inset-x-0 bottom-0 p-4 text-white">
                      <div className="text-xs uppercase tracking-widest opacity-80">
                        {eyebrow}
                      </div>
                      <div className="text-xl font-bold leading-tight mt-1 truncate">
                        {pl.name}
                      </div>
                      <div className="text-xs opacity-75 mt-0.5">
                        {t("home.dailyMix.trackCount", {
                          defaultValue: "{{count}} morceaux",
                          count: pl.track_count,
                        })}
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          )}
        </section>

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
          {recentPlays.length === 0 && recentPlaysLoading ? (
            <HomeCarouselSkeleton label={t("home.recentlyPlayed.title")} />
          ) : recentPlays.length === 0 ? (
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
            <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-4">
              {recentPlays.map((play, idx) => {
                const isCurrent = play.track_id === currentTrack?.id;
                return (
                  <div
                    key={`${play.track_id}-${play.played_at}`}
                    role="button"
                    tabIndex={0}
                    onClick={() => handlePlayRecent(idx)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        handlePlayRecent(idx);
                      }
                    }}
                    className="group flex flex-col items-stretch text-left rounded-2xl p-3 transition-colors hover:bg-zinc-50 dark:hover:bg-zinc-800/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 cursor-pointer"
                  >
                    <Artwork
                      path={play.artwork_path}
                      path1x={play.artwork_path_1x}
                      path2x={play.artwork_path_2x}
                      // Carousel tile renders at ~200 px wide; the 128 px
                      // 2x thumbnail upscales to a visibly soft image on a
                      // HiDPI display (effective 400 device px). Serve the
                      // original artwork — typically 600-1500 px square,
                      // small enough to decode instantly and crisp at any
                      // card size.
                      size="full"
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
                  </div>
                );
              })}
            </div>
          )}
        </section>
      </div>

      {/* Récemment ajoutés — render while loading (skeleton) or once
          we have data; only suppress when the library is empty AND
          nothing is loading, which is the "no library yet" state where
          the welcome banner handles the empty case on its own. */}
      {hasLibrary && (recentAlbumsLoading || recentAlbums.length > 0) && (
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
          {recentAlbums.length === 0 && recentAlbumsLoading ? (
            <HomeCarouselSkeleton label={t("home.recentlyAdded.title")} />
          ) : (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-4">
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
                    // See "Récemment joués" comment above — same reason.
                    size="full"
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
          )}
        </section>
      )}
    </div>
  );
}

interface HomeSkeletonProps {
  label: string;
}

function HomeBannerSkeleton({ label }: HomeSkeletonProps) {
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={label}
      className="flex flex-wrap gap-4 animate-pulse"
    >
      {Array.from({ length: 3 }).map((_, i) => (
        <div
          key={i}
          className="aspect-16/7 basis-70 grow max-w-100 rounded-2xl bg-zinc-200/70 dark:bg-zinc-700/40"
        />
      ))}
    </div>
  );
}

function HomeCarouselSkeleton({ label }: HomeSkeletonProps) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={label}
      className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-4 animate-pulse"
    >
      {Array.from({ length: 6 }).map((_, i) => (
        <div key={i} className="p-3 space-y-3">
          <div className={`w-full aspect-square rounded-2xl ${tile}`} />
          <div className={`h-3 w-3/4 rounded ${tile}`} />
          <div className={`h-3 w-1/2 rounded ${tile}`} />
        </div>
      ))}
    </div>
  );
}
