import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  Folder,
  Library,
  Heart,
  Clock,
  ListMusic,
  Sparkles,
  RefreshCw,
  X,
  Loader2,
} from "lucide-react";
import { useWrappedBannerVisibility } from "../../hooks/useWrappedBannerVisibility";
import type { ViewId } from "../../types";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { useProfile } from "../../hooks/useProfile";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { pickFolder } from "../../lib/tauri/dialog";
import { regenerateAllSmartPlaylists } from "../../lib/tauri/smart_playlists";
import { smartFamilyStyle, smartPlaylistKind } from "../../lib/tauri/playlist";
import { availableWrappedYears } from "../../lib/tauri/wrapped";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { useSkin } from "../../hooks/useSkin";
import { useProfileStats } from "../../hooks/useProfileStats";
import {
  listAlbums,
  listRecentPlays,
  type AlbumRow,
  type RecentPlay,
} from "../../lib/tauri/browse";
import { MoodRadioGrid } from "./home/MoodRadioGrid";
import { HomeSuggestionCard } from "./home/HomeSuggestionCard";
import { FluidHome } from "./home/fluid/FluidHome";
import { FirstStepsCard } from "../discovery/FirstStepsCard";
import { recentPlayToTrack } from "./home/recentPlayToTrack";
import { useAlbumArtistContextMenu } from "../../hooks/useAlbumArtistContextMenu";

interface HomeViewProps {
  onNavigate: (view: ViewId) => void;
  /** The library, already on its Playlists tab. `onNavigate` only picks
   *  a view, and this card has to pick a tab within one. */
  onNavigateToPlaylists: () => void;
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

function getGreetingKey(): "morning" | "afternoon" | "evening" | "night" {
  const hour = new Date().getHours();
  if (hour >= 5 && hour < 12) return "morning";
  if (hour >= 12 && hour < 18) return "afternoon";
  if (hour >= 18 && hour < 23) return "evening";
  return "night";
}

/**
 * The Home page. The Fluid skin composes it differently (a listening
 * canvas, a mood spectrum, an editorial mosaic) and loads its own data,
 * so the branch happens here, above every hook of the classic view.
 */
export function HomeView(props: HomeViewProps) {
  const { skin } = useSkin();
  if (skin.layout === "fluid") return <FluidHome {...props} />;
  return <ClassicHomeView {...props} />;
}

function ClassicHomeView({
  onNavigate,
  onNavigateToPlaylists,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToPlaylist,
  onNavigateToWrapped,
}: HomeViewProps) {
  const { t, i18n } = useTranslation();
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
  // The user's own rule-based playlists are smart too, but they are not
  // "made for you": they stay in the sidebar, as the user's.
  const smartPlaylists = useMemo(
    () =>
      playlists.filter(
        (p) => p.is_smart === 1 && smartPlaylistKind(p)?.kind !== "custom",
      ),
    [playlists],
  );

  // The other half, and what the Playlists card counts. It links to the
  // library's Playlists tab, which shows the user's own only — the smart
  // ones live in "Made for you" above — so counting all of them made the
  // card promise a number the page it opens does not show.
  const userPlaylists = useMemo(
    () => playlists.filter((p) => p.is_smart === 0),
    [playlists],
  );

  const handleRegenerateMixes = useCallback(async () => {
    if (isRegenerating) return;
    setIsRegenerating(true);
    try {
      // Single round-trip regenerates Daily Mix slots + On Repeat so
      // the whole "Made for you" carousel refreshes together — the
      // frontend doesn't need to know about the family split.
      // The mixes are named in the interface's language; `{name}` and
      // `{decade}` go through as text for the backend to fill in.
      await regenerateAllSmartPlaylists({
        genre_name: t("home.genreMix.name", { name: "{name}" }),
        genre_description: t("home.genreMix.description", { name: "{name}" }),
        decade_name: t("home.decadeMix.name", { decade: "{decade}" }),
        decade_description: t("home.decadeMix.description", {
          decade: "{decade}",
        }),
        artist_name: t("home.artistMix.name", { name: "{name}" }),
        artist_description: t("home.artistMix.description", { name: "{name}" }),
        release_radar_description: t("home.releaseRadar.description"),
      });
      await refreshPlaylists();
    } catch (err) {
      // Non-fatal — empty libraries / not enough listening data return
      // empty payloads rather than throw, so a hard error here means a
      // SQL or filesystem issue worth surfacing in the console.
      console.error("[HomeView] regenerate smart playlists failed", err);
    } finally {
      setIsRegenerating(false);
    }
  }, [isRegenerating, refreshPlaylists, t]);

  const { liked_count: likedCount, recent_plays_count: recentCount } =
    useProfileStats();
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
          const lib = await createLibrary({
            name: t("onboarding.defaultLibraryName"),
          });
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

  // Right-click on a recently added album: pin it to the sidebar.
  const albumMenu = useAlbumArtistContextMenu();

  const greeting = `${t(`home.greeting.${getGreetingKey()}`)}${
    greetingName ? `, ${greetingName}` : ""
  }`;

  // Studio's Home: a short header and the library's figures in one row,
  // then the music — the suggestion, the mixes, the moods, the history.
  // Flat surfaces, hairlines, the covers as the only colour; nothing
  // here animates on its own or blurs (`docs/design/studio-refined.md`).
  return (
    <div className="space-y-10 animate-fade-in pb-16">
      <div className="space-y-5">
        <header className="flex flex-wrap items-end justify-between gap-x-8 gap-y-4">
          <div className="min-w-0">
            <p className="flex items-center gap-2 text-xs font-medium text-zinc-500 dark:text-zinc-400">
              <span
                aria-hidden="true"
                className="h-1.5 w-1.5 rounded-full bg-emerald-500"
              />
              {t("home.banner.badge")}
            </p>
            <h1 className="mt-1.5 text-[1.75rem] leading-tight font-semibold tracking-[-0.02em] text-zinc-900 dark:text-white">
              {greeting}
            </h1>
            <p className="mt-1 max-w-2xl text-sm text-zinc-500 dark:text-zinc-400">
              {t("home.banner.subtitle")}
            </p>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-2">
            {hasLibrary && (
              <button
                type="button"
                onClick={() => onNavigate("library")}
                className={STUDIO_BUTTON_SECONDARY}
              >
                <Library size={15} aria-hidden="true" />
                {t("home.banner.browseLibrary")}
              </button>
            )}
            <button
              type="button"
              onClick={handleImport}
              disabled={isImporting}
              className={STUDIO_BUTTON_PRIMARY}
            >
              {isImporting ? (
                <Loader2
                  size={15}
                  className="animate-spin"
                  aria-hidden="true"
                />
              ) : (
                <Folder size={15} aria-hidden="true" />
              )}
              {isImporting
                ? t("library.actions.importing")
                : t("home.banner.importFolder")}
            </button>
          </div>
        </header>

        {/* The library's figures, one row of four, parted by hairlines:
            the container's tone shows through a 1 px gap. */}
        <div className="grid grid-cols-2 gap-px overflow-hidden rounded-xl border border-zinc-200 bg-zinc-200 lg:grid-cols-4 dark:border-zinc-800 dark:bg-zinc-800">
          <StatItem
            icon={<Library size={16} />}
            count={totalTracks.toLocaleString(i18n.language)}
            label={t("home.stats.library")}
            onClick={() => onNavigate("library")}
          />
          <StatItem
            icon={<Heart size={16} />}
            count={likedCount.toLocaleString(i18n.language)}
            label={t("home.stats.liked")}
            onClick={() => onNavigate("liked")}
          />
          <StatItem
            icon={<Clock size={16} />}
            count={recentCount.toLocaleString(i18n.language)}
            label={t("home.stats.recent")}
            onClick={() => onNavigate("recent")}
          />
          <StatItem
            icon={<ListMusic size={16} />}
            count={userPlaylists.length.toLocaleString(i18n.language)}
            label={t("home.stats.playlists")}
            onClick={onNavigateToPlaylists}
          />
        </div>
      </div>

      {/* Suggestion of the day — one track and why: a forgotten
          favourite, one in line with the recent listening, or one never
          played. Hidden when the library has nothing to suggest. */}
      <HomeSuggestionCard />

      {/* Daily Mix — generated from listening history. Hidden when the
          user has too few play_events for a meaningful split (the
          backend returns an empty list and skips re-generation in that
          case); the regen button stays available so the section
          re-appears as soon as enough listening data piles up. */}
      <section>
        <SectionHeading
          title={t("home.dailyMix.title", "Pour vous")}
          action={
            <button
              type="button"
              onClick={handleRegenerateMixes}
              disabled={isRegenerating}
              className={STUDIO_TEXT_ACTION}
            >
              {isRegenerating ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <RefreshCw size={14} />
              )}
              {isRegenerating
                ? t("home.dailyMix.regenerating", "Génération…")
                : t("home.dailyMix.regenerate", "Régénérer")}
            </button>
          }
        />
        {smartPlaylists.length === 0 && playlistsLoading ? (
          <HomeGridSkeleton label={t("home.dailyMix.title", "Pour vous")} />
        ) : smartPlaylists.length === 0 ? (
          <div className="flex min-h-32 items-center justify-center rounded-xl border border-zinc-200 bg-white p-6 dark:border-zinc-800 dark:bg-zinc-900">
            <EmptyState
              icon={<Sparkles size={28} />}
              title={t("home.dailyMix.emptyTitle")}
              description={t("home.dailyMix.emptyDescription")}
              size="sm"
            />
          </div>
        ) : (
          <div className={STUDIO_GRID}>
            {smartPlaylists.map((pl) => {
              const cover = resolveRemoteImage(pl.cover_path, null);
              // The family names the mix under its title; the cover is
              // its only colour.
              const style = smartFamilyStyle(smartPlaylistKind(pl));
              return (
                <button
                  key={`smart-${pl.id}`}
                  type="button"
                  onClick={() => onNavigateToPlaylist(pl.id)}
                  className={STUDIO_TILE}
                >
                  <span className="relative block aspect-square w-full overflow-hidden rounded-lg bg-zinc-100 dark:bg-zinc-800">
                    {cover ? (
                      <img
                        src={cover}
                        alt=""
                        className="absolute inset-0 h-full w-full object-cover"
                        loading="lazy"
                      />
                    ) : (
                      <span className="absolute inset-0 flex items-center justify-center text-zinc-400 dark:text-zinc-500">
                        <Sparkles size={28} aria-hidden="true" />
                      </span>
                    )}
                  </span>
                  <span className="mt-2.5 block min-w-0">
                    <span className="block truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
                      {pl.name}
                    </span>
                    <span className="block truncate text-xs text-zinc-500 dark:text-zinc-400">
                      {t(style.labelKey)}
                      <span aria-hidden="true"> · </span>
                      <span className="tabular-nums">
                        {t("home.dailyMix.trackCount", {
                          count: pl.track_count,
                        })}
                      </span>
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </section>

      {/* First steps — the features worth finding, ticked as they are
          used. Renders nothing once closed or complete. */}
      <FirstStepsCard />

      {/* Wrapped year-in-review banner — gated by
          `useWrappedBannerVisibility`: hidden by default and auto-
          surfaces during the Wrapped season (Dec 1 → Jan 31). Users
          can force it on, force it off, or dismiss it for the current
          recap year from the close button below. */}
      {wrappedYears.length > 0 && wrappedBanner.shouldShow(wrappedYears[0]) && (
        <div className="relative rounded-xl border border-zinc-200 bg-white transition-colors hover:border-zinc-300 dark:border-zinc-800 dark:bg-zinc-900 dark:hover:border-zinc-700">
          <button
            type="button"
            onClick={() => onNavigateToWrapped(wrappedYears[0])}
            className="flex w-full items-center gap-4 rounded-xl p-4 pe-14 text-start focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
              <Sparkles size={20} aria-hidden="true" />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block text-[11px] font-semibold uppercase tracking-[0.08em] text-emerald-600 dark:text-emerald-400">
                {t("home.wrapped.eyebrow")}
              </span>
              <span className="block truncate text-base font-semibold text-zinc-900 dark:text-white">
                {t("home.wrapped.title", { year: wrappedYears[0] })}
              </span>
              <span className="block truncate text-sm text-zinc-500 dark:text-zinc-400">
                {t("home.wrapped.subtitle")}
              </span>
            </span>
            {/* The visibility sits on its own box: the button style carries
                `inline-flex`, which would compete with `hidden` on one element. */}
            <span className="hidden md:inline-flex">
              <span className={STUDIO_BUTTON_SECONDARY}>
                {t("home.wrapped.cta")}
              </span>
            </span>
          </button>
          {/* Dismiss button — persists the year so the banner stays
              hidden until the next recap year is available. Sits beside
              the navigate trigger, not inside it. */}
          <button
            type="button"
            onClick={() => wrappedBanner.dismissYear(wrappedYears[0])}
            aria-label={t("home.wrapped.dismiss")}
            className="absolute end-3 top-3 rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-100 hover:text-zinc-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:hover:bg-white/5 dark:hover:text-zinc-200"
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      )}

      {/* Mood radios — BPM/loudness-filtered queues. Hidden entirely
          when the library has zero analysed tracks (component decides
          internally so HomeView stays declarative). */}
      <MoodRadioGrid />

      {/* Récemment joués */}
      <section>
        <SectionHeading
          title={t("home.recentlyPlayed.title")}
          action={
            recentPlays.length > 0 ? (
              <button
                type="button"
                onClick={() => onNavigate("recent")}
                className={STUDIO_TEXT_ACTION}
              >
                {t("home.seeAll")}
              </button>
            ) : null
          }
        />
        {recentPlays.length === 0 && recentPlaysLoading ? (
          <HomeGridSkeleton label={t("home.recentlyPlayed.title")} />
        ) : recentPlays.length === 0 ? (
          <div className="flex min-h-48 items-center justify-center rounded-xl border border-zinc-200 bg-white p-6 dark:border-zinc-800 dark:bg-zinc-900">
            <EmptyState
              icon={<Clock size={28} />}
              title={t("home.recentlyPlayed.emptyTitle")}
              description={t("home.recentlyPlayed.emptyDescription")}
              size="sm"
            >
              <svg
                viewBox="0 0 400 40"
                preserveAspectRatio="none"
                aria-hidden="true"
                className="mt-6 h-8 w-80 text-zinc-300 dark:text-zinc-700"
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
          <div className={STUDIO_GRID}>
            {recentPlays.map((play, idx) => {
              const isCurrent = play.track_id === currentTrack?.id;
              return (
                <div
                  key={`${play.track_id}-${play.played_at}`}
                  role="button"
                  tabIndex={0}
                  aria-current={isCurrent ? "true" : undefined}
                  onClick={() => handlePlayRecent(idx)}
                  onKeyDown={(e) => {
                    // The artist link inside is a button of its own: its
                    // Enter opens the artist, not the track.
                    if (e.target !== e.currentTarget) return;
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      handlePlayRecent(idx);
                    }
                  }}
                  className={`${STUDIO_TILE} cursor-pointer`}
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
                    className={`w-full aspect-square ${
                      isCurrent
                        ? "ring-2 ring-emerald-500 ring-offset-2 ring-offset-white dark:ring-offset-zinc-900"
                        : ""
                    }`}
                    iconSize={32}
                    rounded="lg"
                  />
                  <div className="mt-2.5 min-w-0">
                    <div
                      className={`truncate text-sm font-medium ${
                        isCurrent
                          ? "text-emerald-600 dark:text-emerald-400"
                          : "text-zinc-900 dark:text-zinc-100"
                      }`}
                    >
                      {play.title}
                    </div>
                    <ArtistLink
                      name={play.artist_name}
                      artistIds={play.artist_ids}
                      onNavigate={onNavigateToArtist}
                      fallback="—"
                      className="block truncate text-xs text-zinc-500 dark:text-zinc-400"
                    />
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </section>

      {/* Récemment ajoutés — render while loading (skeleton) or once
          we have data; only suppress when the library is empty AND
          nothing is loading, which is the "no library yet" state where
          the header's import action handles the empty case on its own. */}
      {hasLibrary && (recentAlbumsLoading || recentAlbums.length > 0) && (
        <section>
          <SectionHeading
            title={t("home.recentlyAdded.title")}
            action={
              <button
                type="button"
                onClick={() => onNavigate("library")}
                className={STUDIO_TEXT_ACTION}
              >
                {t("home.seeAll")}
              </button>
            }
          />
          {recentAlbums.length === 0 && recentAlbumsLoading ? (
            <HomeGridSkeleton label={t("home.recentlyAdded.title")} />
          ) : (
            <div className={STUDIO_GRID}>
              {recentAlbums.map((album) => (
                <button
                  key={album.id}
                  type="button"
                  onClick={() => onNavigateToAlbum(album.id)}
                  {...albumMenu.bind({ kind: "album", id: album.id })}
                  className={STUDIO_TILE}
                >
                  <Artwork
                    path={album.artwork_path}
                    path1x={album.artwork_path_1x}
                    path2x={album.artwork_path_2x}
                    // See "Récemment joués" comment above — same reason.
                    size="full"
                    alt={album.title}
                    className="w-full aspect-square"
                    iconSize={32}
                    rounded="lg"
                  />
                  <div className="mt-2.5 min-w-0">
                    <div className="truncate text-sm font-medium text-zinc-900 dark:text-zinc-100">
                      {album.title}
                    </div>
                    <div className="truncate text-xs text-zinc-500 dark:text-zinc-400">
                      {album.artist_name ?? t("library.table.unknown")}
                    </div>
                  </div>
                </button>
              ))}
            </div>
          )}
        </section>
      )}
      {albumMenu.render()}
    </div>
  );
}

/* Studio's vocabulary for the Home, in one place: one grid for every
   row of covers (so a mix, a play and an album are the same size), one
   tile, three kinds of action. */
const STUDIO_GRID =
  "grid grid-cols-[repeat(auto-fill,minmax(9.5rem,1fr))] gap-x-5 gap-y-6";
const STUDIO_TILE =
  "group -m-2 flex min-w-0 flex-col items-stretch rounded-xl p-2 text-start transition-colors hover:bg-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:hover:bg-white/5";
const STUDIO_BUTTON_PRIMARY =
  "inline-flex h-8 items-center gap-2 rounded-lg bg-emerald-600 px-3 text-sm font-medium text-white transition-colors hover:bg-emerald-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 focus-visible:ring-offset-white disabled:opacity-60 dark:bg-emerald-500 dark:text-zinc-950 dark:hover:bg-emerald-400 dark:focus-visible:ring-offset-zinc-900";
const STUDIO_BUTTON_SECONDARY =
  "inline-flex h-8 items-center gap-2 rounded-lg border border-zinc-200 bg-white px-3 text-sm font-medium text-zinc-700 transition-colors hover:border-zinc-300 hover:bg-zinc-50 hover:text-zinc-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-200 dark:hover:border-zinc-600 dark:hover:bg-zinc-800 dark:hover:text-white";
const STUDIO_TEXT_ACTION =
  "inline-flex items-center gap-1.5 rounded-md px-1.5 py-1 text-sm font-medium text-zinc-500 transition-colors hover:text-zinc-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-not-allowed disabled:opacity-50 dark:text-zinc-400 dark:hover:text-white";

/** A section's title, with its one action (see all, regenerate) on the
 *  right, on the title's baseline. */
function SectionHeading({
  title,
  action,
}: {
  title: string;
  action?: ReactNode;
}) {
  return (
    <div className="mb-4 flex items-center justify-between gap-4">
      <h2 className="text-lg font-semibold tracking-[-0.01em] text-zinc-900 dark:text-white">
        {title}
      </h2>
      {action}
    </div>
  );
}

/** One figure of the library's row: icon, count, label — the whole cell
 *  a button to the page it counts. */
function StatItem({
  icon,
  count,
  label,
  onClick,
}: {
  icon: ReactNode;
  count: string;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex min-w-0 items-center gap-3 bg-white px-4 py-3 text-start transition-colors hover:bg-zinc-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 dark:bg-zinc-900 dark:hover:bg-zinc-800/70"
    >
      <span
        aria-hidden="true"
        className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-zinc-100 text-zinc-500 dark:bg-white/5 dark:text-zinc-400"
      >
        {icon}
      </span>
      <span className="min-w-0">
        <span className="block text-base font-semibold leading-tight tabular-nums text-zinc-900 dark:text-white">
          {count}
        </span>
        <span className="block truncate text-xs text-zinc-500 dark:text-zinc-400">
          {label}
        </span>
      </span>
    </button>
  );
}

interface HomeSkeletonProps {
  label: string;
}

function HomeGridSkeleton({ label }: HomeSkeletonProps) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-800";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={label}
      className={`${STUDIO_GRID} animate-pulse motion-reduce:animate-none`}
    >
      {Array.from({ length: 6 }).map((_, i) => (
        <div key={i} className="space-y-2.5">
          <div className={`aspect-square w-full rounded-lg ${tile}`} />
          <div className={`h-3 w-3/4 rounded ${tile}`} />
          <div className={`h-3 w-1/2 rounded ${tile}`} />
        </div>
      ))}
    </div>
  );
}
