import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Play, Shuffle, Music2, CalendarDays } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { EmptyState } from "../common/EmptyState";
import { DetailViewSkeleton } from "../common/DetailViewSkeleton";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { DetailTrackTable } from "./GenreDetailView";
import { usePlayer } from "../../hooks/usePlayer";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { getYearDetail, type YearDetail } from "../../lib/tauri/detail";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
} from "../../lib/tauri/track";
import { useCreatePlaylistFromModal } from "../../hooks/useCreatePlaylistFromModal";

interface YearDetailViewProps {
  /** `null` is the tracks with no usable year; `undefined`, no year was
   *  asked for. */
  year: number | null | undefined;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

/**
 * Every track of one year, opened from the library's Years tab. The same
 * page as a genre's, without the picture picker: a year's cover is the
 * album that holds most of it.
 */
export function YearDetailView({
  year,
  onNavigateToAlbum,
  onNavigateToArtist,
}: YearDetailViewProps) {
  const { t } = useTranslation();
  const createFromModal = useCreatePlaylistFromModal();
  const { playTracks, currentTrack, toggleShuffle, isShuffled, isPlaying } =
    usePlayer();

  const [detail, setDetail] = useState<YearDetail | null>(null);
  // Starts true so the skeleton paints first, not a "not found" flash.
  const [isLoading, setIsLoading] = useState(true);
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
    if (year === undefined) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDetail(null);
      setIsLoading(false);
      return;
    }
    // Back from one year to another keeps this page mounted: the previous
    // year's tracks must not stay under the new year's title while it
    // loads. Data already for the requested year stays (a refetch).
    // Loading is raised in the same pass, or the frame between would read
    // "no data, not loading" and flash the not-found state.
    setDetail((current) =>
      current != null && current.year !== year ? null : current,
    );
    setIsLoading(true);
    let cancelled = false;
    (async () => {
      try {
        const loaded = await getYearDetail(year);
        if (!cancelled) setDetail(loaded);
      } catch (err) {
        console.error("[YearDetailView] load failed", err);
        if (!cancelled) setDetail(null);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [year, editRefetch]);

  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [year]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  const label =
    year == null ? t("library.yearList.unknown") : String(year);

  if (year === undefined || (!detail && !isLoading)) {
    return (
      <EmptyState
        icon={<CalendarDays size={40} />}
        title={t("yearDetail.emptyTitle")}
        description={t("yearDetail.emptyDescription")}
        className="py-20"
      />
    );
  }

  if (!detail) return <DetailViewSkeleton ariaLabel={t("yearDetail.badge")} />;

  const tracks = detail.tracks;

  const handlePlayAll = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
  };

  const handleShufflePlay = async () => {
    if (tracks.length === 0) return;
    await playTracks(tracks, 0, { type: "library", id: null });
    // Never turn shuffle *off* from the Shuffle button.
    if (!isShuffled) await toggleShuffle();
  };

  return (
    <div className="space-y-8 animate-fade-in pb-20">
      <div className="flex items-center space-x-8">
        <Artwork
          path={detail.artwork_path}
          path1x={detail.artwork_path_1x}
          path2x={detail.artwork_path_2x}
          size="full"
          className="w-48 h-48 shrink-0"
          rounded="2xl"
          iconSize={72}
          placeholderIcon={CalendarDays}
          alt=""
        />

        <div className="flex-1 min-w-0 pt-2">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("yearDetail.badge")}
          </div>
          <h1 className="text-4xl font-bold mb-3 tabular-nums text-zinc-900 dark:text-white truncate">
            {label}
          </h1>

          <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-zinc-500 mb-4">
            <span>
              {t("genreDetail.trackCount", { count: detail.track_count })}
            </span>
            <span>·</span>
            <span>{formatDuration(detail.total_duration_ms)}</span>
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

      {tracks.length > 0 ? (
        <DetailTrackTable
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
          onRowMenuKey={trackContextMenu.openFromKeyboard}
          t={t}
        />
      ) : (
        <EmptyState
          icon={<Music2 size={40} />}
          title={t("yearDetail.emptyTracksTitle")}
          description={t("yearDetail.emptyTracksDescription")}
          className="py-20"
        />
      )}

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
        onCreate={async (data) => {
          try {
            return await createFromModal(data);
          } catch (err) {
            console.error("[YearDetailView] create playlist failed", err);
            throw err;
          }
        }}
      />

      {trackContextMenu.render()}
    </div>
  );
}
