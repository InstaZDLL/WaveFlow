import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
} from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Sparkles, X } from "lucide-react";
import type { ViewId } from "../../../../types";
import { useProfile } from "../../../../hooks/useProfile";
import { useLibrary } from "../../../../hooks/useLibrary";
import { usePlayer } from "../../../../hooks/usePlayer";
import { usePlaylist } from "../../../../hooks/usePlaylist";
import { useWrappedBannerVisibility } from "../../../../hooks/useWrappedBannerVisibility";
import { useSkin } from "../../../../hooks/useSkin";
import { regenerateAllSmartPlaylists } from "../../../../lib/tauri/smart_playlists";
import { smartPlaylistKind } from "../../../../lib/tauri/playlist";
import { availableWrappedYears } from "../../../../lib/tauri/wrapped";
import {
  getProfileStats,
  listAlbums,
  listRecentPlays,
  type AlbumRow,
  type RecentPlay,
} from "../../../../lib/tauri/browse";
import {
  statsOverview,
  statsTopAlbums,
  type TopAlbumRow,
} from "../../../../lib/tauri/stats";
import {
  playerGetQueue,
  playerJumpToIndex,
  type QueueTrackPayload,
} from "../../../../lib/tauri/player";
import { isRadioTrack } from "../../../../lib/playerSources";
import { recentPlayToTrack } from "../recentPlayToTrack";
import { MoodRadioGrid } from "../MoodRadioGrid";
import { EditorialMasthead } from "../EditorialMasthead";
import { ListeningCanvas, type StripAlbum } from "./ListeningCanvas";
import { SuggestionOfTheDay } from "./SuggestionOfTheDay";
import { ForYouMosaic } from "./ForYouMosaic";
import { RecentlyAdded } from "./RecentlyAdded";
import { ListeningHistory } from "./ListeningHistory";
import { coverUrl } from "./format";
import { useCoverGlow } from "./useCoverGlow";

interface FluidHomeProps {
  onNavigate: (view: ViewId) => void;
  onNavigateToPlaylists: () => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToPlaylist: (playlistId: number) => void;
  onNavigateToWrapped: (year: number | null) => void;
}

const RECENT_LIMIT = 16;
const ALBUMS_LIMIT = 9;

const rise = (i: number): CSSProperties => ({ ["--i" as string]: i });

/**
 * The Home page under the Fluid skin: what you were listening to, what
 * we suggest today, what mood you are in, what we picked for you, what
 * is new, what you played. Six sections, five layout families — a split
 * canvas, two bands, a mosaic, a shelf, a list. Same data sources as the classic Home, plus
 * the queue (for "up next"), the all-time top album (for the featured
 * tile) and the listening total (for the counters line).
 */
export function FluidHome({
  onNavigate,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToPlaylist,
  onNavigateToWrapped,
}: FluidHomeProps) {
  const { t } = useTranslation();
  const { skin } = useSkin();
  const { activeProfile } = useProfile();
  const { libraries } = useLibrary();
  const {
    currentTrack,
    isPlaying,
    playbackState,
    durationMs,
    togglePlayback,
    playTracks,
  } = usePlayer();
  const { playlists, refresh: refreshPlaylists } = usePlaylist();

  const [recentPlays, setRecentPlays] = useState<RecentPlay[]>([]);
  const [recentPlaysLoading, setRecentPlaysLoading] = useState(true);
  const [recentAlbums, setRecentAlbums] = useState<AlbumRow[]>([]);
  const [recentAlbumsLoading, setRecentAlbumsLoading] = useState(true);
  const [featured, setFeatured] = useState<TopAlbumRow | null>(null);
  const [likedCount, setLikedCount] = useState(0);
  const [listenedMs, setListenedMs] = useState(0);
  const [queue, setQueue] = useState<QueueTrackPayload[]>([]);
  // Absolute index of `queue[0]` in the player's queue, for the jump.
  const [queueBase, setQueueBase] = useState(0);
  const [isRegenerating, setIsRegenerating] = useState(false);
  const [wrappedYears, setWrappedYears] = useState<number[]>([]);
  const wrappedBanner = useWrappedBannerVisibility();

  const profileId = activeProfile?.id;
  const hasLibrary = libraries.length > 0;

  // Recent plays + counters: same triggers as the classic Home, so the
  // page updates in lockstep with the history view.
  useEffect(() => {
    let cancelled = false;
    listRecentPlays(null, RECENT_LIMIT)
      .then((rows) => {
        if (!cancelled) setRecentPlays(rows);
      })
      .catch((err) => console.error("[FluidHome] list recent plays", err))
      .finally(() => {
        if (!cancelled) setRecentPlaysLoading(false);
      });
    getProfileStats()
      .then((s) => {
        if (!cancelled) setLikedCount(s.liked_count);
      })
      .catch((err) => console.error("[FluidHome] profile stats", err));
    statsOverview("all")
      .then((o) => {
        if (!cancelled) setListenedMs(o.total_ms);
      })
      .catch((err) => console.error("[FluidHome] stats overview", err));
    statsTopAlbums("all", 1)
      .then((rows) => {
        if (!cancelled) setFeatured(rows[0] ?? null);
      })
      .catch((err) => console.error("[FluidHome] top albums", err));
    availableWrappedYears()
      .then((years) => {
        if (!cancelled) setWrappedYears(years);
      })
      .catch((err) => console.error("[FluidHome] wrapped years", err));
    return () => {
      cancelled = true;
    };
  }, [profileId, playbackState]);

  // Recently added — refetched when any library is rescanned.
  const librariesSignature = libraries
    .map((l) => `${l.id}:${l.updated_at}`)
    .join(",");
  useEffect(() => {
    if (!hasLibrary) return;
    let cancelled = false;
    listAlbums(null, { orderBy: "added_at", direction: "desc" })
      .then((rows) => {
        if (!cancelled) setRecentAlbums(rows.slice(0, ALBUMS_LIMIT));
      })
      .catch((err) => console.error("[FluidHome] list albums", err))
      .finally(() => {
        if (!cancelled) setRecentAlbumsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [hasLibrary, librariesSignature]);

  // Up next: the queue after the cursor, kept current through the
  // backend's queue events.
  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      playerGetQueue()
        .then((snap) => {
          if (cancelled) return;
          const base = snap.current_index >= 0 ? snap.current_index + 1 : 0;
          setQueueBase(base);
          setQueue(snap.items.slice(base, base + 3));
        })
        .catch(() => {
          // No queue yet: nothing to show.
        });
    };
    refresh();
    const unlistens: UnlistenFn[] = [];
    Promise.all([
      listen("player:queue-changed", refresh),
      listen("player:track-changed", refresh),
    ])
      .then((fns) => {
        if (cancelled) fns.forEach((fn) => fn());
        else unlistens.push(...fns);
      })
      .catch((err) => console.error("[FluidHome] listen failed", err));
    return () => {
      cancelled = true;
      unlistens.forEach((fn) => fn());
    };
  }, [profileId]);

  const smartPlaylists = useMemo(
    () =>
      playlists.filter(
        (p) => p.is_smart === 1 && smartPlaylistKind(p)?.kind !== "custom",
      ),
    [playlists],
  );

  const handleRegenerateMixes = useCallback(async () => {
    if (isRegenerating) return;
    setIsRegenerating(true);
    try {
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
      console.error("[FluidHome] regenerate smart playlists failed", err);
    } finally {
      setIsRegenerating(false);
    }
  }, [isRegenerating, refreshPlaylists, t]);

  // The canvas is about the playing track, or the last played one when
  // nothing is loaded (a fresh launch): "Lire" then starts it.
  const lastPlayed = recentPlays[0] ?? null;
  const canvasTrack =
    currentTrack && !isRadioTrack(currentTrack)
      ? currentTrack
      : lastPlayed
        ? recentPlayToTrack(lastPlayed)
        : (currentTrack ?? null);
  const isCurrent = currentTrack != null && canvasTrack === currentTrack;
  useCoverGlow(canvasTrack ? coverUrl(canvasTrack) : null);

  const playRecent = useCallback(
    async (index: number) => {
      if (recentPlays.length === 0) return;
      const tracks = recentPlays.map(recentPlayToTrack);
      await playTracks(tracks, index, { type: "library", id: null });
    },
    [recentPlays, playTracks],
  );

  const handleResume = useCallback(() => {
    if (isCurrent) void togglePlayback();
    else if (lastPlayed) void playRecent(0);
  }, [isCurrent, lastPlayed, togglePlayback, playRecent]);

  // The last five distinct records, each pointing at the play to start.
  const strip = useMemo<StripAlbum[]>(() => {
    const seen = new Set<number>();
    const out: StripAlbum[] = [];
    recentPlays.forEach((p, i) => {
      if (p.album_id == null || seen.has(p.album_id)) return;
      seen.add(p.album_id);
      out.push({
        albumId: p.album_id,
        title: p.album_title ?? p.title,
        artist: p.artist_name,
        artwork_path: p.artwork_path,
        artwork_path_1x: p.artwork_path_1x,
        artwork_path_2x: p.artwork_path_2x,
        playIndex: i,
      });
    });
    return out.slice(0, 5);
  }, [recentPlays]);

  const totalTracks = libraries.reduce((s, l) => s + l.track_count, 0);
  const totalAlbums = libraries.reduce((s, l) => s + l.album_count, 0);
  const statsLine = [
    t("sidebar.library.tracksCount", { count: totalTracks }),
    t("sidebar.library.popover.albums", { count: totalAlbums }),
    t("home.fluid.liked", { count: likedCount }),
    t("home.fluid.hours", { count: Math.round(listenedMs / 3_600_000) }),
  ];

  // A live station has no album and no end: the canvas keeps the last
  // played record, and the kicker says a radio is on.
  const radioLabel =
    currentTrack && isRadioTrack(currentTrack) ? t("home.fluid.radio") : null;

  // The broadsheet masthead over the canvas: a piece of the preset, not
  // of a skin id — the date moves up into its strip and the canvas
  // cover gets its "Fig." caption.
  const masthead = skin.fluid?.greeting === "masthead";

  return (
    <div className="mx-auto max-w-[1680px] space-y-14 pb-10 animate-fade-in">
      <div className="fe-rise" style={rise(0)}>
        {masthead && <EditorialMasthead />}
        <ListeningCanvas
          profileName={activeProfile?.name ?? ""}
          statsLine={statsLine}
          masthead={masthead}
          track={canvasTrack}
          isCurrent={isCurrent}
          isPlaying={isPlaying}
          durationMs={durationMs}
          radioLabel={radioLabel}
          onResume={handleResume}
          onOpenAlbum={onNavigateToAlbum}
          onNavigateToArtist={onNavigateToArtist}
          upNext={queue}
          onPlayQueueItem={(i) =>
            playerJumpToIndex(queueBase + i).catch((err) =>
              console.error("[FluidHome] jump failed", err),
            )
          }
          strip={strip}
          onPlayStrip={(i) => void playRecent(i)}
        />
      </div>

      {/* Suggestion of the day — one track and why. Renders nothing
          while it loads and when the library has nothing to suggest, so
          it carries its own stagger wrapper rather than leaving an
          empty one in the stack. */}
      <SuggestionOfTheDay
        onNavigateToArtist={onNavigateToArtist}
        style={rise(1)}
      />

      {wrappedYears.length > 0 && wrappedBanner.shouldShow(wrappedYears[0]) && (
        <div className="fe-rise" style={rise(2)}>
          <div className="relative flex items-center gap-5 rounded-[1.125rem] bg-[var(--fe-surface)] px-6 py-5">
            <Sparkles size={22} className="fe-accent shrink-0" />
            <button
              type="button"
              onClick={() => onNavigateToWrapped(wrappedYears[0])}
              className="min-w-0 flex-1 text-left"
            >
              <span className="fe-label block">
                {t("home.wrapped.eyebrow")}
              </span>
              <span className="fe-title fe-ink mt-1 block text-[1.25rem]">
                {t("home.wrapped.title", { year: wrappedYears[0] })}
              </span>
              <span className="fe-ink-3 mt-0.5 block text-[0.8125rem]">
                {t("home.wrapped.subtitle")}
              </span>
            </button>
            <button
              type="button"
              onClick={() => wrappedBanner.dismissYear(wrappedYears[0])}
              aria-label={t("home.wrapped.dismiss")}
              className="fe-icon-btn shrink-0"
            >
              <X size={16} />
            </button>
          </div>
        </div>
      )}

      <div className="fe-rise" style={rise(3)}>
        <MoodRadioGrid />
      </div>

      <div className="fe-rise" style={rise(4)}>
        <ForYouMosaic
          featured={featured}
          smartPlaylists={smartPlaylists}
          isRegenerating={isRegenerating}
          onRegenerate={handleRegenerateMixes}
          onNavigateToAlbum={onNavigateToAlbum}
          onNavigateToPlaylist={onNavigateToPlaylist}
        />
      </div>

      {hasLibrary && (
        <div className="fe-rise" style={rise(5)}>
          <RecentlyAdded
            albums={recentAlbums}
            loading={recentAlbumsLoading}
            onNavigateToAlbum={onNavigateToAlbum}
            onSeeAll={() => onNavigate("library")}
          />
        </div>
      )}

      <div className="fe-rise" style={rise(6)}>
        <ListeningHistory
          plays={recentPlays}
          loading={recentPlaysLoading}
          currentTrackId={currentTrack?.id ?? null}
          onPlay={(i) => void playRecent(i)}
          onNavigateToArtist={onNavigateToArtist}
          onSeeAll={() => onNavigate("recent")}
        />
      </div>
    </div>
  );
}
