import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Shuffle,
  Music2,
  Clock,
  Heart,
  Pencil,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { Artwork } from "../common/Artwork";
import { ArtistHeroBackdrop } from "../common/ArtistHeroBackdrop";
import { EmptyState } from "../common/EmptyState";
import { DetailViewSkeleton } from "../common/DetailViewSkeleton";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { ArtistImagePickerModal } from "../common/ArtistImagePickerModal";
import { ArtistMetadataEditorModal } from "../common/ArtistMetadataEditorModal";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { Lightbox } from "../common/Lightbox";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useArtistBioCollapsed } from "../../hooks/useArtistBioCollapsed";
import { useArtistHero } from "../../hooks/useArtistHero";
import {
  remoteGetArtist,
  type RemoteArtist,
} from "../../lib/tauri/remoteServer";
import { getSimilarArtistsByName } from "../../lib/tauri/similar";
import { RemoteArtwork } from "../common/RemoteArtwork";
import {
  getArtistDetail,
  enrichArtistDeezer,
  enrichArtistByName,
  type ArtistDetail,
} from "../../lib/tauri/detail";
import { resolveArtwork, resolveRemoteImage } from "../../lib/tauri/artwork";
import { getSimilarArtists, type SimilarArtist } from "../../lib/tauri/similar";
import {
  formatDuration,
  listTracks,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";

/**
 * A server artist in the shape the view already speaks.
 *
 * Everything the server has no answer for is `null`, which is what those
 * fields already mean locally before enrichment has run — and the enrichment
 * that fills them is the same one, keyed by name instead of by row.
 */
function toArtistDetail(artist: RemoteArtist): ArtistDetail {
  return {
    // Never read: every site that acts on a rowid checks the source first.
    id: -1,
    name: artist.name,
    artwork_hash: artist.artwork_hash,
    artwork_path: null,
    artwork_path_1x: null,
    artwork_path_2x: null,
    picture_url: null,
    picture_path: null,
    picture_path_1x: null,
    picture_path_2x: null,
    fans_count: null,
    bio_short: null,
    bio_full: null,
    background_url: null,
    background_path: null,
    // The server's artist payload lists albums and no tracks, so there is no
    // top-track section to fill rather than an empty one to explain.
    track_count: 0,
    album_count: artist.albums.length,
    albums: artist.albums.map((album, index) => ({
      source: "remote" as const,
      remote_id: album.id,
      artwork_hash: album.artwork_hash,
      id: -(index + 1),
      title: album.title,
      year: album.year,
      track_count: 0,
      total_duration_ms: 0,
      artwork_path: null,
      artwork_path_1x: null,
      artwork_path_2x: null,
    })),
  };
}

interface ArtistDetailViewProps {
  artistId: number | null;
  /** Set instead of `artistId` when the artist is the bound server's.
   *  Exactly one of the two is ever set. */
  remoteArtistId?: string | null;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  /// Click target for similar-artist cards that map to a row in the
  /// user's library. Suggestions without a `library_artist_id` aren't
  /// clickable — there's no in-app destination for an external artist.
  onNavigateToArtist: (artistId: number) => void;
}

/**
 * One artist's page, from the device or from the bound server.
 *
 * The server carries an artist's name, their portrait and their albums — and
 * nothing else: no biography, no fan count, no background. That was already
 * true of a library artist, which is why both sides get the photo, the hero
 * background and the biography from the same by-name enrichment. The only real
 * difference is how the enrichment is keyed: a local artist resolves a cached
 * Deezer id through its own row, a server artist has no row and goes by name.
 */
export function ArtistDetailView({
  artistId,
  remoteArtistId = null,
  onNavigateToAlbum,
  onNavigateToRemoteAlbum,
  onNavigateToArtist,
}: ArtistDetailViewProps) {
  const { t } = useTranslation();
  // Which catalogue this artist came from. Everything keyed on a local rowid
  // is gated on it.
  const remote = remoteArtistId != null;
  // The identity of what is being asked for, as opposed to what is loaded —
  // see `AlbumDetailView`: a snapshot read for another artist counts as
  // absent, not as an approximation of this one.
  const artistKey =
    remoteArtistId != null ? `remote:${remoteArtistId}` : `local:${artistId}`;
  const { playTracks, currentTrack, toggleShuffle, isShuffled, isPlaying } =
    usePlayer();
  const { createPlaylist } = usePlaylist();

  const [artist, setArtist] = useState<ArtistDetail | null>(null);
  const [loadedKey, setLoadedKey] = useState<string | null>(null);
  const [tracks, setTracks] = useState<Track[]>([]);
  // Init true so the skeleton paints on first render (paired with the
  // `!artist && !isLoading` early-return below to avoid a one-frame
  // "artist not found" flash before the fetch effect schedules).
  const [isLoading, setIsLoading] = useState(true);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  const [isImagePickerOpen, setIsImagePickerOpen] = useState(false);
  const [isMetadataEditorOpen, setIsMetadataEditorOpen] = useState(false);
  // Bumped after a bio/similar override save so the enrichment +
  // similar effects re-run and pick up the new local values.
  const [overrideRefetch, setOverrideRefetch] = useState(0);

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
    // No `onNavigateToArtist` — we're already on the artist page.
  });

  // Deezer enrichment. `pictureSrc` is already resolved against the
  // local file cache (via `convertFileSrc`) when available so the
  // component never has to know whether the source is a remote CDN
  // URL or an `asset://` path.
  const [pictureSrc, setPictureSrc] = useState<string | null>(null);
  // Wide TheAudioDB fanart backing the hero (issue #482). Separate from
  // `pictureSrc` because the two tiers are treated very differently: a
  // fanart is shown nearly crisp, the square photo heavily blurred.
  const [fanartSrc, setFanartSrc] = useState<string | null>(null);
  // `heroResolved` gates the first paint: the preference defaults to ON
  // and is read asynchronously, so rendering before it lands would flash
  // a hero at users who turned it off.
  const { enabled: heroEnabled, resolved: heroResolved } = useArtistHero();
  const [fansCount, setFansCount] = useState<number | null>(null);
  const [bioShort, setBioShort] = useState<string | null>(null);
  const [bioFull, setBioFull] = useState<string | null>(null);
  const [bioExpanded, setBioExpanded] = useState(false);
  // Global (per-profile) "hide the bio block" preference — issue #422. A
  // long bio otherwise pushes the discography off-screen; the toggle
  // lives on the bio block itself, not in Settings.
  const { collapsed: bioCollapsed, setCollapsed: setBioCollapsed } =
    useArtistBioCollapsed();
  const [isLightboxOpen, setIsLightboxOpen] = useState(false);
  const [similar, setSimilar] = useState<SimilarArtist[]>([]);

  // Bumped on `track:updated` so a tag edit refreshes the visible
  // artist + track list without re-navigating.
  const [editRefetch, setEditRefetch] = useState(0);
  useTrackUpdated(useCallback(() => setEditRefetch((k) => k + 1), []));

  // Load artist detail
  useEffect(() => {
    if (artistId == null && remoteArtistId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setArtist(null);
      setTracks([]);
      return;
    }
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      setPictureSrc(null);
      setFanartSrc(null);
      setFansCount(null);
      setBioShort(null);
      setBioFull(null);
      setBioExpanded(false);
      try {
        // A server artist has no track list to intersect: its payload lists
        // albums only, so the top-track section is absent rather than empty.
        const [detail, allTracks] = remoteArtistId != null
          ? [toArtistDetail(await remoteGetArtist(remoteArtistId)), []]
          : await Promise.all([
              getArtistDetail(artistId as number),
              listTracks(null),
            ]);
        if (cancelled) return;
        setArtist(detail);
        setLoadedKey(artistKey);
        // Local sidecar `artist.jpg` always wins over the Deezer cache —
        // keeps the offline-first promise from issue #31.
        const seeded = resolveArtwork(
          {
            full: detail.artwork_path ?? detail.picture_path,
            x1: detail.artwork_path_1x ?? detail.picture_path_1x,
            x2: detail.artwork_path_2x ?? detail.picture_path_2x,
            remoteUrl: detail.picture_url,
          },
          "full",
        );
        if (seeded) setPictureSrc(seeded);
        // Served straight from the metadata cache, so the hero paints on
        // the first frame when the artist was enriched before.
        setFanartSrc(
          resolveRemoteImage(detail.background_path, detail.background_url),
        );
        if (detail.fans_count != null) setFansCount(detail.fans_count);
        if (detail.bio_short) setBioShort(detail.bio_short);
        if (detail.bio_full) setBioFull(detail.bio_full);
        // Match any track where this artist appears in the multi-artist
        // string (split on ", ") — covers both primary and feature
        // credits from the same list_tracks payload.
        const artistTracks = allTracks.filter((t) => {
          const names = (t.artist_name ?? "").split(", ").map((s) => s.trim());
          return names.includes(detail.name);
        });
        setTracks(artistTracks);
      } catch (err) {
        console.error("[ArtistDetailView] load failed", err);
        if (!cancelled) {
          setArtist(null);
          setTracks([]);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [artistId, remoteArtistId, artistKey, editRefetch]);

  // Load liked IDs
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [artistId]);

  // Similar artists (Last.fm primary, Deezer fallback). Loaded after
  // the main detail so it never blocks the initial paint.
  useEffect(() => {
    // Keyed by row locally, by name for a server artist — which is what the
    // curated-override path needs the local row for, and why the two are not
    // one call.
    const pending =
      remoteArtistId != null
        ? artist
          ? getSimilarArtistsByName(artist.name)
          : null
        : artistId != null
          ? getSimilarArtists(artistId)
          : null;
    if (!pending) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSimilar([]);
      return;
    }
    let cancelled = false;
    pending
      .then((list) => {
        if (!cancelled) setSimilar(list);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [artistId, remoteArtistId, artist, overrideRefetch]);

  // Deezer enrichment. Gated on `artist` being loaded so the
  // "local-wins-over-Deezer" guard below can read `artwork_path` from a
  // fresh detail instead of a stale closure value.
  const hasLocalArtistImage = !!artist?.artwork_path;
  useEffect(() => {
    if (artist == null) return;
    // Same enrichment, two keys. A local artist resolves a cached Deezer id
    // through its own row, which is also what carries a curated override; a
    // server artist has no row, so it goes by name.
    const pending =
      remoteArtistId != null
        ? enrichArtistByName(artist.name)
        : artistId != null
          ? enrichArtistDeezer(artistId)
          : null;
    if (!pending) return;
    let cancelled = false;
    pending
      .then((e) => {
        if (cancelled) return;
        const resolved = resolveArtwork(
          {
            full: e.picture_path,
            x1: e.picture_path_1x,
            x2: e.picture_path_2x,
            remoteUrl: e.picture_url,
          },
          "full",
        );
        if (resolved && !hasLocalArtistImage) setPictureSrc(resolved);
        const fanart = resolveRemoteImage(e.background_path, e.background_url);
        // Only ever *set* — a refresh that comes back empty (offline,
        // TheAudioDB down) must not blank a hero the cache already gave us.
        if (fanart) setFanartSrc(fanart);
        if (e.fans_count != null) setFansCount(e.fans_count);
        if (e.bio_short) setBioShort(e.bio_short);
        if (e.bio_full) setBioFull(e.bio_full);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [artistId, remoteArtistId, hasLocalArtistImage, artist, overrideRefetch]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  // Either identifier will do — testing only the local one is what sent every
  // server album to the empty state in `AlbumDetailView`.
  if ((artistId == null && remoteArtistId == null) || (!artist && !isLoading)) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("artistDetail.emptyTitle")}
        description={t("artistDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  // Loading, or holding another artist's snapshot — the same thing as far as
  // everything below is concerned.
  if (!artist || loadedKey !== artistKey) {
    return (
      <DetailViewSkeleton ariaLabel={t("artistDetail.badge")} shape="circle" />
    );
  }

  const handlePlayAll = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
  };

  const handleShufflePlay = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
    // Gate the toggle so we never *disable* shuffle when the user
    // explicitly clicks the Shuffle button.
    if (!isShuffled) await toggleShuffle();
  };

  const fansLabel =
    fansCount != null
      ? fansCount >= 1_000_000
        ? `${(fansCount / 1_000_000).toFixed(1)}M fans`
        : fansCount >= 1_000
          ? `${(fansCount / 1_000).toFixed(0)}K fans`
          : `${fansCount} fans`
      : null;

  // Hero backdrop precedence (issue #482): real wide fanart → blurred
  // square photo → nothing at all (today's flat header).
  const heroSrc =
    heroResolved && heroEnabled ? (fanartSrc ?? pictureSrc) : null;
  // Header copy is forced white over the hero's dark scrim, in every
  // theme — matching Spotify, whose artist header is always dark-on-image.
  const secondaryButtonClass = heroSrc
    ? "border border-white/25 bg-white/10 hover:bg-white/20 text-white backdrop-blur-sm"
    : "border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300";

  return (
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header — wrapped in a full-bleed hero when the artist has an
          image and the preference is on (issue #482). The negative
          margins break out of `<main>`'s `p-8` so the backdrop reaches
          the column edges; the padding puts the inset back for the
          content itself. */}
      <div
        className={
          heroSrc ? "relative -mx-8 -mt-8 px-8 pt-12 pb-10 overflow-hidden" : ""
        }
      >
        {heroSrc && (
          <ArtistHeroBackdrop src={heroSrc} isFanart={fanartSrc != null} />
        )}
        {/* `relative` keeps the header above the absolutely-positioned
            backdrop without inventing a z-index. */}
        <div className="relative flex items-center space-x-8">
          {/* Artist photo + edit overlay (visible on hover/focus) */}
          <div className="relative shrink-0 group">
            {!pictureSrc && remote && artist.artwork_hash ? (
              /* The server's own portrait. Shown while the by-name enrichment
                 is in flight and kept if it never finds anything — no
                 lightbox, which opens the original file, and there is none. */
              <RemoteArtwork
                hash={artist.artwork_hash}
                className="w-48 h-48 rounded-full shadow-lg"
                iconSize={64}
              />
            ) : pictureSrc ? (
              /* Keyboard-accessible lightbox trigger; omitted when no artist photo */
              <button
                type="button"
                onClick={() => setIsLightboxOpen(true)}
                aria-label={t("common.viewArtwork")}
                className="cursor-zoom-in focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded-full"
              >
                <img
                  src={pictureSrc}
                  alt={artist.name}
                  className="w-48 h-48 rounded-full object-cover shadow-lg"
                />
              </button>
            ) : (
              <div className="w-48 h-48 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 border border-violet-200/60 dark:border-violet-800/40 flex items-center justify-center shadow-lg">
                <span className="text-7xl font-bold text-violet-500/70 dark:text-violet-400/60">
                  {artist.name.trim().charAt(0).toUpperCase() || "?"}
                </span>
              </div>
            )}
            {/* The wrapper is pointer-events-none so the lightbox button
              keeps receiving clicks everywhere; the inner pencil button
              re-enables pointer events for its small hit area. */}
            {/* Both edits write into the local `artist` row — the picked
                image and the curated metadata alike. A server artist has no
                such row. */}
            {!remote && (
            <div className="absolute right-2 bottom-2 pointer-events-none">
              <button
                type="button"
                onClick={() => setIsImagePickerOpen(true)}
                aria-label={t("artistImagePicker.editAria")}
                title={t("artistImagePicker.title")}
                className="pointer-events-auto w-10 h-10 rounded-full bg-zinc-900/80 hover:bg-zinc-900 text-white shadow-lg flex items-center justify-center opacity-0 group-hover:opacity-100 focus-visible:opacity-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-400 transition-opacity"
              >
                <Pencil size={16} />
              </button>
            </div>
            )}
          </div>

          <div className="flex-1 min-w-0 pt-2">
            <div
              className={`text-[10px] font-bold tracking-widest uppercase mb-1 ${
                heroSrc ? "text-white/70" : "text-zinc-400"
              }`}
            >
              {t("artistDetail.badge")}
            </div>
            <h1
              className={`text-4xl font-bold mb-3 truncate ${
                heroSrc
                  ? "text-white [text-shadow:0_2px_12px_rgb(0_0_0/0.55)]"
                  : "text-zinc-900 dark:text-white"
              }`}
            >
              {artist.name}
            </h1>

            {/* Stats */}
            <div
              className={`flex flex-wrap items-center gap-x-2 gap-y-1 text-sm mb-4 ${
                heroSrc ? "text-white/85" : "text-zinc-500"
              }`}
            >
              <span>
                {t("artistDetail.trackCount", { count: artist.track_count })}
              </span>
              <span>·</span>
              <span>
                {t("artistDetail.albumCount", { count: artist.album_count })}
              </span>
              {fansLabel && (
                <>
                  <span>·</span>
                  <span>{fansLabel}</span>
                </>
              )}
            </div>

            {/* Actions. Both play the artist's *tracks*, and the server's
                artist payload lists albums and no tracks — so for a server
                artist there is nothing at this level to play, and a pair of
                permanently disabled buttons would say the page is broken
                rather than that the discography below is the way in. */}
            <div className="flex items-center space-x-3">
              {!remote && (
              <>
              <button
                type="button"
                onClick={handlePlayAll}
                disabled={tracks.length === 0}
                className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
              >
                <Play size={16} className="fill-current" />
                <span>{t("artistDetail.playAll")}</span>
              </button>
              <button
                type="button"
                onClick={handleShufflePlay}
                disabled={tracks.length === 0}
                className={`${secondaryButtonClass} px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50`}
              >
                <Shuffle size={16} />
                <span>{t("artistDetail.shuffle")}</span>
              </button>
              </>
              )}
              {!remote && (
              <button
                type="button"
                onClick={() => setIsMetadataEditorOpen(true)}
                className={`${secondaryButtonClass} px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm`}
              >
                <Pencil size={16} />
                <span>{t("artistDetail.editMetadata")}</span>
              </button>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Bio */}
      {bioShort && (
        <div className="space-y-3">
          <div className="flex items-center justify-between px-1">
            <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
              {t("artistDetail.bio.title")}
            </h2>
            <button
              type="button"
              onClick={() => void setBioCollapsed(!bioCollapsed)}
              aria-expanded={!bioCollapsed}
              aria-label={t(
                bioCollapsed
                  ? "artistDetail.bio.show"
                  : "artistDetail.bio.hide",
              )}
              className="flex items-center gap-1 text-[10px] font-bold uppercase tracking-wider text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
            >
              <span>
                {t(
                  bioCollapsed
                    ? "artistDetail.bio.show"
                    : "artistDetail.bio.hide",
                )}
              </span>
              {bioCollapsed ? (
                <ChevronDown size={14} />
              ) : (
                <ChevronUp size={14} />
              )}
            </button>
          </div>
          {!bioCollapsed && (
            <div className="rounded-2xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-800/40">
              <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-300 whitespace-pre-line break-words hyphens-auto">
                {bioExpanded ? (bioFull ?? bioShort) : bioShort}
              </p>
              {bioFull && bioFull.length > bioShort.length && (
                <button
                  type="button"
                  onClick={() => setBioExpanded((p) => !p)}
                  className="mt-3 text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:underline"
                >
                  {bioExpanded
                    ? t("nowPlaying.readLess")
                    : t("nowPlaying.readMore")}
                </button>
              )}
            </div>
          )}
        </div>
      )}

      {/* Similar artists (Last.fm primary, Deezer fallback) */}
      {similar.length > 0 && (
        <div className="space-y-4">
          <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
            {t("artistDetail.similar.title")}
          </h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-4">
            {similar.map((s) => {
              const inLibrary = s.library_artist_id != null;
              const imgSrc = resolveRemoteImage(s.picture_path, s.picture_url);
              const Wrapper = inLibrary ? "button" : "div";
              return (
                <Wrapper
                  key={`${s.source}-${s.name}`}
                  type={inLibrary ? "button" : undefined}
                  onClick={
                    inLibrary
                      ? () => onNavigateToArtist(s.library_artist_id!)
                      : undefined
                  }
                  className={`group flex flex-col items-center text-center space-y-2 ${
                    inLibrary ? "cursor-pointer" : "cursor-default opacity-70"
                  }`}
                  title={
                    inLibrary
                      ? t("artistDetail.similar.inLibrary")
                      : t("artistDetail.similar.notInLibrary")
                  }
                >
                  {imgSrc ? (
                    <img
                      src={imgSrc}
                      alt={s.name}
                      loading="lazy"
                      className="w-24 h-24 rounded-full object-cover shadow-sm group-hover:shadow-md transition-shadow"
                    />
                  ) : (
                    <div className="w-24 h-24 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 flex items-center justify-center shadow-sm">
                      <span className="text-3xl font-bold text-violet-500/70 dark:text-violet-400/60">
                        {s.name.trim().charAt(0).toUpperCase() || "?"}
                      </span>
                    </div>
                  )}
                  <div className="px-1 w-full">
                    <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
                      {s.name}
                    </div>
                    {inLibrary && (
                      <div className="text-[10px] font-medium text-emerald-600 dark:text-emerald-400 mt-0.5">
                        {t("artistDetail.similar.inLibrary")}
                      </div>
                    )}
                  </div>
                </Wrapper>
              );
            })}
          </div>
        </div>
      )}

      {/* Discography */}
      {artist.albums.length > 0 && (
        <div className="space-y-4">
          <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
            {t("artistDetail.discography")}
          </h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-5">
            {artist.albums.map((album) => (
              <button
                key={album.remote_id ?? album.id}
                type="button"
                onClick={() =>
                  album.remote_id
                    ? onNavigateToRemoteAlbum?.(album.remote_id)
                    : onNavigateToAlbum(album.id)
                }
                className="group flex flex-col space-y-2 text-left"
              >
                {album.source === "remote" ? (
                  <RemoteArtwork
                    hash={album.artwork_hash ?? null}
                    className="w-full aspect-square rounded-2xl shadow-sm group-hover:shadow-md transition-shadow"
                    iconSize={44}
                  />
                ) : (
                <Artwork
                  path={album.artwork_path}
                  path1x={album.artwork_path_1x}
                  path2x={album.artwork_path_2x}
                  // Discography tile renders ~150-200 px wide; see
                  // HomeView carousel comment — same reason for full.
                  size="full"
                  alt={album.title}
                  className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                  iconSize={44}
                  rounded="2xl"
                />
                )}
                <div className="px-1">
                  <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
                    {album.title}
                  </div>
                  <div className="text-xs text-zinc-500">
                    {album.year ?? ""}
                    {album.year && album.track_count > 0 ? " · " : ""}
                    {t("artistDetail.albumTrackCount", {
                      count: album.track_count,
                    })}
                  </div>
                </div>
              </button>
            ))}
          </div>
        </div>
      )}

      {/* All tracks */}
      {tracks.length > 0 && (
        <div className="space-y-4">
          <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
            {t("artistDetail.allTracks")}
          </h2>
          <ArtistTrackTable
            tracks={tracks}
            isLoading={isLoading}
            currentTrackId={currentTrack?.id ?? null}
            isPlaying={isPlaying}
            likedIds={likedIds}
            onToggleLike={handleToggleLike}
            onPlayTrack={(index) =>
              playTracks(tracks, index, { type: "library", id: null })
            }
            onContextMenuRow={trackContextMenu.open}
            onRowMenuKey={trackContextMenu.openFromKeyboard}
            t={t}
          />
        </div>
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
            console.error("[ArtistDetailView] create playlist failed", err);
          }
        }}
      />

      {trackContextMenu.render()}

      <Lightbox
        src={pictureSrc}
        alt={artist.name}
        isOpen={isLightboxOpen}
        onClose={() => setIsLightboxOpen(false)}
      />

      {/* Mounted only for a local artist: their triggers are hidden for a
          server one, and mounting them anyway would hand each the negative
          sentinel. */}
      {!remote && (
      <>
      <ArtistImagePickerModal
        artistId={artist.id}
        artistName={artist.name}
        hasArtwork={!!artist.artwork_path}
        isOpen={isImagePickerOpen}
        onClose={() => setIsImagePickerOpen(false)}
        onSuccess={() => setEditRefetch((k) => k + 1)}
      />

      <ArtistMetadataEditorModal
        artistId={artist.id}
        artistName={artist.name}
        isOpen={isMetadataEditorOpen}
        onClose={() => setIsMetadataEditorOpen(false)}
        onSuccess={() => {
          // Reset bio so the enrichment effect repopulates from the new
          // source — clearing an override must drop the stale text, and
          // the effect only ever *sets* bio (never nulls it).
          setBioShort(null);
          setBioFull(null);
          setBioExpanded(false);
          setOverrideRefetch((k) => k + 1);
        }}
        onSplit={(primaryArtistId) => {
          // The phantom artist we're viewing was just dissolved (issue
          // #396); jump to the new primary so we don't render a dead id.
          setIsMetadataEditorOpen(false);
          if (primaryArtistId != null) onNavigateToArtist(primaryArtistId);
        }}
      />
      </>
      )}
    </div>
  );
}

// ── Track table ─────────────────────────────────────────────────────

function ArtistTrackTable({
  tracks,
  isLoading,
  currentTrackId,
  isPlaying,
  likedIds,
  onToggleLike,
  onPlayTrack,
  onContextMenuRow,
  onRowMenuKey,
  t,
}: {
  tracks: Track[];
  isLoading: boolean;
  currentTrackId: number | null;
  isPlaying: boolean;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  t: (key: string, opts?: Record<string, unknown>) => string;
}) {
  const gridCols = "grid-cols-[3rem_2.75rem_1fr_1fr_5rem_2rem]";
  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span className="text-right">{t("library.table.number")}</span>
        <span aria-hidden="true" />
        <span>{t("library.table.title")}</span>
        <span>{t("library.table.album")}</span>
        <span
          className="flex justify-end"
          aria-label={t("library.table.duration")}
        >
          <Clock size={14} />
        </span>
        <span aria-hidden="true" />
      </div>

      <ul
        className={`divide-y divide-zinc-100 dark:divide-zinc-800/60 ${
          isLoading ? "opacity-50" : ""
        }`}
      >
        {tracks.map((track, index) => {
          const isCurrent = track.id === currentTrackId;
          return (
            // Row can't be a <button> because it contains action
            // buttons; nested buttons are invalid HTML. Keyboard
            // activation still works via tabIndex + onKeyDown.
            <li
              key={`${track.id}-${index}`}
              tabIndex={0}
              role="button"
              onDoubleClick={() => onPlayTrack(index)}
              onKeyDown={(e) => {
                // Only play when the row itself is focused — see LibraryView.
                if (e.target !== e.currentTarget) return;
                if (onRowMenuKey(e, tracks[index])) return;
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onPlayTrack(index);
                }
              }}
              onKeyUp={(e) => {
                if (e.target !== e.currentTarget) return;
                if (e.key === " ") e.preventDefault();
              }}
              onContextMenu={(e) => onContextMenuRow(e, track)}
              className={`grid ${gridCols} gap-4 px-5 py-2 items-center select-none transition-colors cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
                isCurrent
                  ? "bg-emerald-50 dark:bg-emerald-900/20"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
              }`}
            >
              <span
                className={`text-right text-sm tabular-nums flex items-center justify-end ${
                  isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
                }`}
              >
                {isCurrent ? (
                  <PlayingIndicator isPlaying={isPlaying} />
                ) : (
                  index + 1
                )}
              </span>
              <Artwork
                path={track.artwork_path}
                path1x={track.artwork_path_1x}
                path2x={track.artwork_path_2x}
                size="1x"
                className="w-10 h-10"
                iconSize={18}
                alt={track.album_title ?? track.title}
                rounded="md"
              />
              <span
                className={`text-sm truncate flex items-center gap-2 ${
                  isCurrent
                    ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                    : "text-zinc-800 dark:text-zinc-200"
                }`}
              >
                <span className="truncate">{track.title}</span>
                <HiResBadge
                  bitDepth={track.bit_depth}
                  sampleRate={track.sample_rate}
                  codec={track.codec}
                  variant="inline"
                />
              </span>
              <span className="text-sm text-zinc-500 truncate">
                {track.album_title ?? t("library.table.unknown")}
              </span>
              <span className="text-sm tabular-nums text-zinc-400 text-right">
                {formatDuration(track.duration_ms)}
              </span>
              <div className="flex justify-center">
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    onToggleLike(track.id);
                  }}
                  aria-label={
                    likedIds.has(track.id) ? t("liked.unlike") : t("liked.like")
                  }
                  className={`p-1 rounded-full transition-colors ${
                    likedIds.has(track.id)
                      ? "text-pink-500"
                      : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
                  }`}
                >
                  <Heart
                    size={14}
                    className={likedIds.has(track.id) ? "fill-current" : ""}
                  />
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
