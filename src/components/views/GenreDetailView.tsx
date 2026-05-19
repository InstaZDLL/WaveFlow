import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Play, Shuffle, Clock, Music2, Heart, Tags } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { getGenreDetail, type GenreDetail } from "../../lib/tauri/detail";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";

interface GenreDetailViewProps {
  genreId: number | null;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

export function GenreDetailView({
  genreId,
  onNavigateToAlbum,
  onNavigateToArtist,
}: GenreDetailViewProps) {
  const { t } = useTranslation();
  const { playTracks, currentTrack, toggleShuffle, isShuffled, isPlaying } =
    usePlayer();
  const { createPlaylist } = usePlaylist();

  const [genre, setGenre] = useState<GenreDetail | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);

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

  const [editRefetch, setEditRefetch] = useState(0);
  useTrackUpdated(useCallback(() => setEditRefetch((k) => k + 1), []));

  useEffect(() => {
    if (genreId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setGenre(null);
      return;
    }
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        const detail = await getGenreDetail(genreId);
        if (!cancelled) setGenre(detail);
      } catch (err) {
        console.error("[GenreDetailView] load failed", err);
        if (!cancelled) setGenre(null);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [genreId, editRefetch]);

  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [genreId]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  if (genreId == null || (!genre && !isLoading)) {
    return (
      <EmptyState
        icon={<Tags size={40} />}
        title={t("genreDetail.emptyTitle")}
        description={t("genreDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  if (!genre) return null;

  const tracks = genre.tracks;

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

  return (
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-8">
        <div className="w-48 h-48 rounded-2xl bg-amber-100 text-amber-600 dark:bg-amber-950/60 dark:text-amber-400 flex items-center justify-center shadow-lg shrink-0">
          <Tags size={72} />
        </div>

        <div className="flex-1 min-w-0 pt-2">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("genreDetail.badge")}
          </div>
          <h1 className="text-4xl font-bold mb-3 text-zinc-900 dark:text-white truncate">
            {genre.name}
          </h1>

          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-zinc-500 mb-4">
            <span>
              {t("genreDetail.trackCount", { count: genre.track_count })}
            </span>
            <span>·</span>
            <span>{formatDuration(genre.total_duration_ms)}</span>
          </div>

          <div className="flex items-center space-x-3">
            <button
              type="button"
              onClick={handlePlayAll}
              disabled={tracks.length === 0}
              className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Play size={16} className="fill-current" />
              <span>{t("genreDetail.playAll")}</span>
            </button>
            <button
              type="button"
              onClick={handleShufflePlay}
              disabled={tracks.length === 0}
              className="border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800/50 hover:bg-zinc-50 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 px-5 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-50"
            >
              <Shuffle size={16} />
              <span>{t("genreDetail.shuffle")}</span>
            </button>
          </div>
        </div>
      </div>

      {/* Tracks */}
      {tracks.length > 0 ? (
        <GenreTrackTable
          tracks={tracks}
          isLoading={isLoading}
          currentTrackId={currentTrack?.id ?? null}
          isPlaying={isPlaying}
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onPlayTrack={(index) =>
            playTracks(tracks, index, { type: "library", id: null })
          }
          onNavigateToAlbum={onNavigateToAlbum}
          onNavigateToArtist={onNavigateToArtist}
          onContextMenuRow={trackContextMenu.open}
          t={t}
        />
      ) : (
        <EmptyState
          icon={<Music2 size={40} />}
          title={t("genreDetail.emptyTracksTitle")}
          description={t("genreDetail.emptyTracksDescription")}
          className="py-20"
        />
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
            console.error("[GenreDetailView] create playlist failed", err);
          }
        }}
      />

      {trackContextMenu.render()}
    </div>
  );
}

// ── Track table ─────────────────────────────────────────────────────

function GenreTrackTable({
  tracks,
  isLoading,
  currentTrackId,
  isPlaying,
  likedIds,
  onToggleLike,
  onPlayTrack,
  onNavigateToAlbum,
  onNavigateToArtist,
  onContextMenuRow,
  t,
}: {
  tracks: Track[];
  isLoading: boolean;
  currentTrackId: number | null;
  isPlaying: boolean;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onPlayTrack: (index: number) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  t: (key: string, opts?: Record<string, unknown>) => string;
}) {
  const gridCols = "grid-cols-[3rem_2.75rem_1.5fr_1fr_1fr_5rem_2rem]";
  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span className="text-right">{t("library.table.number")}</span>
        <span aria-hidden="true" />
        <span>{t("library.table.title")}</span>
        <span>{t("library.table.artist")}</span>
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
              // eslint-disable-next-line
              tabIndex={0}
              onDoubleClick={() => onPlayTrack(index)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onPlayTrack(index);
                }
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
                <ArtistLink
                  name={track.artist_name}
                  artistIds={track.artist_ids}
                  onNavigate={onNavigateToArtist}
                  fallback={t("library.table.unknown")}
                />
              </span>
              <span className="text-sm text-zinc-500 truncate">
                {track.album_id != null && track.album_title ? (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      onNavigateToAlbum(track.album_id!);
                    }}
                    className="hover:underline truncate text-left"
                  >
                    {track.album_title}
                  </button>
                ) : (
                  (track.album_title ?? t("library.table.unknown"))
                )}
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
