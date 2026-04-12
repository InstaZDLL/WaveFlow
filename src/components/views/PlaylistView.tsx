import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Play,
  Shuffle,
  Edit2,
  Trash2,
  Clock,
  Music2,
  Heart,
} from "lucide-react";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { Tooltip } from "../common/Tooltip";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import { getPlaylist, type Playlist } from "../../lib/tauri/playlist";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";

interface PlaylistViewProps {
  playlistId: number | null;
  /** Called when the active playlist gets deleted so AppLayout can swap. */
  onAfterDelete: () => void;
  onNavigateToArtist: (artistId: number) => void;
}

export function PlaylistView({ playlistId, onAfterDelete, onNavigateToArtist }: PlaylistViewProps) {
  const { t } = useTranslation();
  const { playTracks, currentTrack, toggleShuffle } = usePlayer();
  const { updatePlaylist, deletePlaylist, getPlaylistTracks, playlists } =
    usePlaylist();

  const [playlist, setPlaylist] = useState<Playlist | null>(null);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isDeleting, setIsDeleting] = useState(false);
  const confirmTimeoutRef = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (confirmTimeoutRef.current != null) {
        window.clearTimeout(confirmTimeoutRef.current);
      }
    };
  }, []);

  // Load liked IDs so hearts render correctly.
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [playlistId]);

  const handleToggleLike = async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  };

  // Fetch playlist + its tracks whenever the focused id changes. Also
  // re-runs when the playlist list itself updates (e.g. after rename via
  // `updatePlaylist`) so the header reflects the new metadata without a
  // manual refresh.
  const playlistsSignature = playlists
    .map((p) => `${p.id}:${p.updated_at}`)
    .join(",");
  useEffect(() => {
    let cancelled = false;
    if (playlistId == null) {
      setPlaylist(null);
      setTracks([]);
      return;
    }
    (async () => {
      setIsLoading(true);
      try {
        const [pl, items] = await Promise.all([
          getPlaylist(playlistId),
          getPlaylistTracks(playlistId),
        ]);
        if (cancelled) return;
        setPlaylist(pl);
        setTracks(items);
      } catch (err) {
        if (!cancelled) {
          console.error("[PlaylistView] failed to load playlist", err);
          setPlaylist(null);
          setTracks([]);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [playlistId, playlistsSignature, getPlaylistTracks]);

  if (playlistId == null) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("playlistView.noneTitle")}
        description={t("playlistView.noneDescription")}
        className="py-20"
      />
    );
  }

  if (playlist == null && !isLoading) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("playlistView.notFoundTitle")}
        description={t("playlistView.notFoundDescription")}
        className="py-20"
      />
    );
  }

  const color = playlist
    ? resolvePlaylistColor(playlist.color_id)
    : resolvePlaylistColor("violet");
  const totalDurationMs = playlist?.total_duration_ms ?? 0;

  const handlePlayAll = async () => {
    if (tracks.length === 0) return;
    await playTracks(
      tracks,
      0,
      { type: "playlist", id: playlistId }
    );
  };

  const handleShufflePlay = async () => {
    if (tracks.length === 0) return;
    await playTracks(
      tracks,
      0,
      { type: "playlist", id: playlistId }
    );
    // Toggle shuffle on if it isn't already; the backend handles the
    // case where it's already shuffled gracefully.
    await toggleShuffle();
  };

  const handleEditSubmit = async (data: {
    name: string;
    description: string;
    colorId: string;
    iconId: string;
  }) => {
    if (playlistId == null) return;
    try {
      await updatePlaylist(playlistId, {
        name: data.name,
        description: data.description || null,
        color_id: data.colorId,
        icon_id: data.iconId,
      });
    } catch (err) {
      console.error("[PlaylistView] update failed", err);
    }
  };

  /**
   * Two-step delete: first click flips into "confirm?" with a 3 s
   * auto-revert. Second click within the window actually deletes.
   * Mirrors the LibraryView pattern.
   */
  const handleDeleteClick = async () => {
    if (playlistId == null || isDeleting) return;
    if (!confirmDelete) {
      setConfirmDelete(true);
      if (confirmTimeoutRef.current != null) {
        window.clearTimeout(confirmTimeoutRef.current);
      }
      confirmTimeoutRef.current = window.setTimeout(() => {
        setConfirmDelete(false);
        confirmTimeoutRef.current = null;
      }, 3000);
      return;
    }
    if (confirmTimeoutRef.current != null) {
      window.clearTimeout(confirmTimeoutRef.current);
      confirmTimeoutRef.current = null;
    }
    setIsDeleting(true);
    try {
      // Redirect away BEFORE the delete so we don't briefly render a
      // not-found state.
      onAfterDelete();
      await deletePlaylist(playlistId);
    } catch (err) {
      console.error("[PlaylistView] delete failed", err);
    } finally {
      setIsDeleting(false);
      setConfirmDelete(false);
    }
  };

  const totalDurationLabel =
    totalDurationMs > 0 ? formatDuration(totalDurationMs) : "—";

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div
        className={`flex items-start justify-between p-6 rounded-2xl ${color.previewBg}`}
      >
        <div className="flex items-center space-x-6">
          <div
            className={`w-24 h-24 rounded-2xl flex items-center justify-center shadow-sm ${color.tileBg} ${color.tileText}`}
          >
            {playlist ? (
              <PlaylistIcon iconId={playlist.icon_id} size={48} />
            ) : (
              <Music2 size={48} />
            )}
          </div>
          <div>
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("playlistView.badge")}
            </div>
            <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
              {playlist?.name ?? ""}
            </h1>
            {playlist?.description && (
              <p className="text-sm text-zinc-500 mb-2">
                {playlist.description}
              </p>
            )}
            <div className="flex items-center text-sm text-zinc-500 space-x-2">
              <Music2 size={16} />
              <span>
                {t("playlistView.trackCount", {
                  count: playlist?.track_count ?? 0,
                })}
              </span>
              <span>·</span>
              <span>{totalDurationLabel}</span>
            </div>
          </div>
        </div>

        <div className="flex items-center space-x-3">
          <button
            type="button"
            onClick={handlePlayAll}
            disabled={tracks.length === 0}
            className={`text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm ${color.button} disabled:opacity-50 disabled:cursor-not-allowed`}
          >
            <Play size={16} className="fill-current" />
            <span>{t("playlistView.actions.play")}</span>
          </button>

          <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
            <Tooltip label={t("playlistView.actions.shuffle")}>
              <button
                type="button"
                onClick={handleShufflePlay}
                disabled={tracks.length === 0}
                aria-label={t("playlistView.actions.shuffle")}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Shuffle size={18} />
              </button>
            </Tooltip>

            <Tooltip label={t("playlistView.actions.edit")}>
              <button
                type="button"
                onClick={() => setIsEditOpen(true)}
                aria-label={t("playlistView.actions.edit")}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white"
              >
                <Edit2 size={18} />
              </button>
            </Tooltip>

            <Tooltip
              label={
                confirmDelete
                  ? t("playlistView.actions.deleteConfirm")
                  : t("playlistView.actions.delete")
              }
            >
              <button
                type="button"
                onClick={handleDeleteClick}
                disabled={isDeleting}
                aria-label={t("playlistView.actions.delete")}
                className={`p-2 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
                  confirmDelete
                    ? "bg-red-500 text-white hover:bg-red-600"
                    : "text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-500/10"
                }`}
              >
                <Trash2 size={18} />
              </button>
            </Tooltip>
          </div>
        </div>
      </div>

      {/* Tracks list */}
      {tracks.length > 0 ? (
        <PlaylistTrackTable
          tracks={tracks}
          isLoading={isLoading}
          currentTrackId={currentTrack?.id ?? null}
          onPlayTrack={(index) =>
            playTracks(tracks, index, {
              type: "playlist",
              id: playlistId,
            })
          }
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onNavigateToArtist={onNavigateToArtist}
          unknownLabel={t("library.table.unknown")}
          headerLabels={{
            number: t("library.table.number"),
            title: t("library.table.title"),
            artist: t("library.table.artist"),
            album: t("library.table.album"),
            duration: t("library.table.duration"),
          }}
          likeLabel={t("liked.like")}
          unlikeLabel={t("liked.unlike")}
        />
      ) : (
        <EmptyState
          icon={<Music2 size={40} />}
          title={t("playlistView.emptyTitle")}
          description={t("playlistView.emptyDescription")}
          className="py-20"
        />
      )}

      <CreatePlaylistModal
        isOpen={isEditOpen}
        onClose={() => setIsEditOpen(false)}
        existing={playlist}
        onCreate={handleEditSubmit}
      />
    </div>
  );
}

interface PlaylistTrackTableProps {
  tracks: Track[];
  isLoading: boolean;
  currentTrackId: number | null;
  onPlayTrack: (index: number) => void;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  unknownLabel: string;
  headerLabels: {
    number: string;
    title: string;
    artist: string;
    album: string;
    duration: string;
  };
  likeLabel: string;
  unlikeLabel: string;
}

function PlaylistTrackTable({
  tracks,
  isLoading,
  currentTrackId,
  onPlayTrack,
  likedIds,
  onToggleLike,
  onNavigateToArtist,
  unknownLabel,
  headerLabels,
  likeLabel,
  unlikeLabel,
}: PlaylistTrackTableProps) {
  const gridCols = "grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem_2rem]";
  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span className="text-right">{headerLabels.number}</span>
        <span aria-hidden="true" />
        <span>{headerLabels.title}</span>
        <span>{headerLabels.artist}</span>
        <span>{headerLabels.album}</span>
        <span className="flex justify-end" aria-label={headerLabels.duration}>
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
              <ArtistLink
                name={track.artist_name}
                artistIds={track.artist_ids}
                onNavigate={onNavigateToArtist}
                fallback={unknownLabel}
                className="text-sm text-zinc-500 truncate"
              />
              <span className="text-sm text-zinc-500 truncate">
                {track.album_title ?? unknownLabel}
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
                  aria-label={likedIds.has(track.id) ? unlikeLabel : likeLabel}
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
