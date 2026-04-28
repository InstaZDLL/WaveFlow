import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Play, Shuffle, Music2, Clock, Heart } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { Lightbox } from "../common/Lightbox";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import {
  getArtistDetail,
  enrichArtistDeezer,
  type ArtistDetail,
} from "../../lib/tauri/detail";
import { resolveArtwork } from "../../lib/tauri/artwork";
import {
  formatDuration,
  listTracks,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";

interface ArtistDetailViewProps {
  artistId: number | null;
  onNavigateToAlbum: (albumId: number) => void;
}

export function ArtistDetailView({
  artistId,
  onNavigateToAlbum,
}: ArtistDetailViewProps) {
  const { t } = useTranslation();
  const { playTracks, currentTrack, toggleShuffle } = usePlayer();
  const { createPlaylist } = usePlaylist();

  const [artist, setArtist] = useState<ArtistDetail | null>(null);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] = useState(false);

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
  const [fansCount, setFansCount] = useState<number | null>(null);
  const [bioShort, setBioShort] = useState<string | null>(null);
  const [bioFull, setBioFull] = useState<string | null>(null);
  const [bioExpanded, setBioExpanded] = useState(false);
  const [isLightboxOpen, setIsLightboxOpen] = useState(false);

  // Load artist detail
  useEffect(() => {
    if (artistId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setArtist(null);
      setTracks([]);
      return;
    }
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      setPictureSrc(null);
      setFansCount(null);
      setBioShort(null);
      setBioFull(null);
      setBioExpanded(false);
      try {
        const [detail, allTracks] = await Promise.all([
          getArtistDetail(artistId),
          listTracks(null),
        ]);
        if (cancelled) return;
        setArtist(detail);
        // Seed Deezer cache from the detail response so images render
        // instantly on re-visits (not just after enrichment resolves).
        const seeded = resolveArtwork(
          {
            full: detail.picture_path,
            x1: detail.picture_path_1x,
            x2: detail.picture_path_2x,
            remoteUrl: detail.picture_url,
          },
          "full",
        );
        if (seeded) setPictureSrc(seeded);
        if (detail.fans_count != null) setFansCount(detail.fans_count);
        if (detail.bio_short) setBioShort(detail.bio_short);
        if (detail.bio_full) setBioFull(detail.bio_full);
        // Match any track where this artist appears in the multi-artist
        // string (split on ", ") — covers both primary and feature
        // credits from the same list_tracks payload.
        const artistTracks = allTracks.filter((t) => {
          const names = (t.artist_name ?? "")
            .split(", ")
            .map((s) => s.trim());
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
  }, [artistId]);

  // Load liked IDs
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [artistId]);

  // Deezer enrichment
  useEffect(() => {
    if (artistId == null) return;
    let cancelled = false;
    enrichArtistDeezer(artistId)
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
        if (resolved) setPictureSrc(resolved);
        if (e.fans_count != null) setFansCount(e.fans_count);
        if (e.bio_short) setBioShort(e.bio_short);
        if (e.bio_full) setBioFull(e.bio_full);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [artistId]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  if (artistId == null || (!artist && !isLoading)) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("artistDetail.emptyTitle")}
        description={t("artistDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  if (!artist) return null;

  const handlePlayAll = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
  };

  const handleShufflePlay = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
    await toggleShuffle();
  };

  const fansLabel =
    fansCount != null
      ? fansCount >= 1_000_000
        ? `${(fansCount / 1_000_000).toFixed(1)}M fans`
        : fansCount >= 1_000
          ? `${(fansCount / 1_000).toFixed(0)}K fans`
          : `${fansCount} fans`
      : null;

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-8">
        {/* Artist photo */}
        {pictureSrc ? (
          <img
            src={pictureSrc}
            alt={artist.name}
            onDoubleClick={() => setIsLightboxOpen(true)}
            className="w-48 h-48 rounded-full object-cover shadow-lg shrink-0 cursor-zoom-in"
          />
        ) : (
          <div className="w-48 h-48 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 border border-violet-200/60 dark:border-violet-800/40 flex items-center justify-center shadow-lg shrink-0">
            <span className="text-7xl font-bold text-violet-500/70 dark:text-violet-400/60">
              {artist.name.trim().charAt(0).toUpperCase() || "?"}
            </span>
          </div>
        )}

        <div className="flex-1 min-w-0 pt-2">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("artistDetail.badge")}
          </div>
          <h1 className="text-4xl font-bold mb-3 text-zinc-900 dark:text-white truncate">
            {artist.name}
          </h1>

          {/* Stats */}
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-zinc-500 mb-4">
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

          {/* Actions */}
          <div className="flex items-center space-x-3">
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
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Shuffle size={16} />
              <span>{t("artistDetail.shuffle")}</span>
            </button>
          </div>
        </div>
      </div>

      {/* Bio */}
      {bioShort && (
        <div className="space-y-3">
          <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
            {t("artistDetail.bio.title")}
          </h2>
          <div className="rounded-2xl border border-zinc-200 bg-white p-5 dark:border-zinc-800 dark:bg-zinc-800/40">
            <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-300 whitespace-pre-line">
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
        </div>
      )}

      {/* Discography */}
      {artist.albums.length > 0 && (
        <div className="space-y-4">
          <h2 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-1">
            {t("artistDetail.discography")}
          </h2>
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5">
            {artist.albums.map((album) => (
              <button
                key={album.id}
                type="button"
                onClick={() => onNavigateToAlbum(album.id)}
                className="group flex flex-col space-y-2 text-left"
              >
                <Artwork
                  path={album.artwork_path}
                  path1x={album.artwork_path_1x}
                  path2x={album.artwork_path_2x}
                  size="2x"
                  alt={album.title}
                  className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                  iconSize={44}
                  rounded="2xl"
                />
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
            likedIds={likedIds}
            onToggleLike={handleToggleLike}
            onPlayTrack={(index) =>
              playTracks(tracks, index, { type: "library", id: null })
            }
            onContextMenuRow={trackContextMenu.open}
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
    </div>
  );
}

// ── Track table ─────────────────────────────────────────────────────

function ArtistTrackTable({
  tracks,
  isLoading,
  currentTrackId,
  likedIds,
  onToggleLike,
  onPlayTrack,
  onContextMenuRow,
  t,
}: {
  tracks: Track[];
  isLoading: boolean;
  currentTrackId: number | null;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
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
        <span className="flex justify-end" aria-label={t("library.table.duration")}>
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
            <li
              key={`${track.id}-${index}`}
              onDoubleClick={() => onPlayTrack(index)}
              onContextMenu={(e) => onContextMenuRow(e, track)}
              className={`grid ${gridCols} gap-4 px-5 py-2 items-center select-none transition-colors cursor-pointer ${
                isCurrent
                  ? "bg-emerald-50 dark:bg-emerald-900/20"
                  : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
              }`}
            >
              <span
                className={`text-right text-sm tabular-nums ${
                  isCurrent
                    ? "text-emerald-500 font-semibold"
                    : "text-zinc-400"
                }`}
              >
                {index + 1}
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
                className={`text-sm truncate ${
                  isCurrent
                    ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                    : "text-zinc-800 dark:text-zinc-200"
                }`}
              >
                {track.title}
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
                    likedIds.has(track.id)
                      ? t("liked.unlike")
                      : t("liked.like")
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
