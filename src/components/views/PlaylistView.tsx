import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  Play,
  Shuffle,
  Edit2,
  Trash2,
  Clock,
  Music2,
  Heart,
  GripVertical,
} from "lucide-react";
import {
  DndContext,
  DragOverlay,
  MeasuringStrategy,
  PointerSensor,
  useSensor,
  useSensors,
  closestCenter,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { Tooltip } from "../common/Tooltip";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  getPlaylist,
  reorderPlaylistTrack,
  type Playlist,
} from "../../lib/tauri/playlist";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";

interface PlaylistViewProps {
  playlistId: number | null;
  /** Called when the active playlist gets deleted so AppLayout can swap. */
  onAfterDelete: () => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

export function PlaylistView({
  playlistId,
  onAfterDelete,
  onNavigateToAlbum,
  onNavigateToArtist,
}: PlaylistViewProps) {
  const { t } = useTranslation();
  const { playTracks, currentTrack, toggleShuffle } = usePlayer();
  const { updatePlaylist, deletePlaylist, getPlaylistTracks, playlists, removeTrackFromPlaylist, createPlaylist } =
    usePlaylist();
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] = useState(false);

  const [playlist, setPlaylist] = useState<Playlist | null>(null);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isDeleting, setIsDeleting] = useState(false);
  const selection = useMultiSelect<Track>();
  const confirmTimeoutRef = useRef<number | null>(null);
  // Latest tracks snapshot kept in a ref so row callbacks (play, reorder)
  // can stay reference-stable across optimistic reorders. Without this
  // they'd close over `tracks` and bust the memo() on every row each time
  // the array changes mid-drag.
  const tracksRef = useRef<Track[]>(tracks);
  useEffect(() => {
    tracksRef.current = tracks;
  }, [tracks]);

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

  // Clear selection when switching playlists.
  const clearSelection = selection.clear;
  useEffect(() => {
    clearSelection();
  }, [playlistId, clearSelection]);

  const handleRowSelect = useCallback(
    (track: Track, e: React.MouseEvent) => {
      const items = tracksRef.current;
      if (e.shiftKey) {
        selection.selectRange(track.id, items);
      } else if (e.ctrlKey || e.metaKey) {
        selection.toggleOne(track.id);
      } else {
        selection.setSingle(track.id);
      }
    },
    [selection],
  );

  const handleToggleLike = useCallback(async (trackId: number) => {
    const nowLiked = await toggleLikeTrack(trackId);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(trackId);
      else next.delete(trackId);
      return next;
    });
  }, []);

  const handleRemoveFromPlaylist = useCallback(
    async (pid: number, trackId: number) => {
      try {
        await removeTrackFromPlaylist(pid, trackId);
        setTracks((prev) => prev.filter((t) => t.id !== trackId));
      } catch (err) {
        console.error("[PlaylistView] remove track from playlist failed", err);
      }
    },
    [removeTrackFromPlaylist],
  );

  const handleReorder = useCallback(
    (fromIdx: number, toIdx: number) => {
      if (playlistId == null || fromIdx === toIdx) return;
      const current = tracksRef.current;
      const moved = current[fromIdx];
      if (!moved) return;
      // Optimistic local reorder so the row settles in place before
      // the round-trip; if the backend rejects it, the playlist
      // refetch on update_at change snaps things back.
      setTracks((prev) => arrayMove(prev, fromIdx, toIdx));
      reorderPlaylistTrack(playlistId, moved.id, toIdx).catch((err) => {
        console.error("[PlaylistView] reorder failed", err);
        setTracks((prev) => arrayMove(prev, toIdx, fromIdx));
      });
    },
    [playlistId],
  );

  const handlePlayTrackByIndex = useCallback(
    (index: number) => {
      if (playlistId == null) return;
      const current = tracksRef.current;
      if (index < 0 || index >= current.length) return;
      void playTracks(current, index, { type: "playlist", id: playlistId });
    },
    [playTracks, playlistId],
  );

  const handleLikedChanged = useCallback(
    (trackId: number, nowLiked: boolean) =>
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      }),
    [],
  );

  const handleOpenCreatePlaylistModal = useCallback(
    () => setIsCreatePlaylistModalOpen(true),
    [],
  );

  const trackContextMenu = useTrackContextMenu({
    likedIds,
    onLikedChanged: handleLikedChanged,
    onCreatePlaylist: handleOpenCreatePlaylistModal,
    onNavigateToAlbum,
    onNavigateToArtist,
    currentPlaylistId: playlistId,
    onRemoveFromPlaylist: handleRemoveFromPlaylist,
  });
  const onContextMenuRow = trackContextMenu.open;

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
      // eslint-disable-next-line react-hooks/set-state-in-effect
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

  // Pre-translated table labels — pulled out before any early return so
  // the hook order stays stable across render branches.
  const unknownLabel = t("library.table.unknown");
  const likeLabel = t("liked.like");
  const unlikeLabel = t("liked.unlike");
  const headerLabels = useMemo(
    () => ({
      number: t("library.table.number"),
      title: t("library.table.title"),
      artist: t("library.table.artist"),
      album: t("library.table.album"),
      duration: t("library.table.duration"),
    }),
    [t],
  );

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
          onPlayTrack={handlePlayTrackByIndex}
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onNavigateToArtist={onNavigateToArtist}
          unknownLabel={unknownLabel}
          headerLabels={headerLabels}
          likeLabel={likeLabel}
          unlikeLabel={unlikeLabel}
          onContextMenuRow={onContextMenuRow}
          onReorder={handleReorder}
          isSelected={selection.isSelected}
          onRowSelect={handleRowSelect}
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
            console.error("[PlaylistView] create playlist failed", err);
          }
        }}
      />

      {trackContextMenu.render()}

      {playlistId != null && (
        <SelectionActionBar
          trackIds={[...selection.selectedIds]}
          context={{ type: "playlist", playlistId }}
          onClear={selection.clear}
          onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
          onAfterRemoveFromPlaylist={(removedIds) => {
            const removed = new Set(removedIds);
            setTracks((prev) => prev.filter((t) => !removed.has(t.id)));
          }}
        />
      )}
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
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  onReorder: (fromIndex: number, toIndex: number) => void;
  isSelected: (id: number) => boolean;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
}

const PLAYLIST_ROW_HEIGHT = 56;

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
  onContextMenuRow,
  onReorder,
  isSelected,
  onRowSelect,
}: PlaylistTrackTableProps) {
  "use no memo";
  const gridCols = "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem]";
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
  );
  // Per-row stable IDs: `track.id` is the playlist's PRIMARY KEY and
  // can't repeat (a track is in a playlist either zero or one times),
  // so it doubles as the dnd-kit handle.
  const ids = useMemo(() => tracks.map((t) => String(t.id)), [tracks]);

  const [activeId, setActiveId] = useState<string | null>(null);

  const scrollRef = useRef<HTMLDivElement>(null);
  // Virtualize the row list. SortableContext keeps the *full* id array
  // so dnd-kit knows the abstract ordering, even for items that aren't
  // currently mounted — only the on-screen window pays the useSortable
  // cost. This is what makes grab-on-300+-tracks feel instant.
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => PLAYLIST_ROW_HEIGHT,
    overscan: 8,
  });

  const handleDragStart = useCallback((e: DragStartEvent) => {
    setActiveId(String(e.active.id));
  }, []);

  const handleDragEnd = useCallback(
    (e: DragEndEvent) => {
      setActiveId(null);
      const { active, over } = e;
      if (!over || active.id === over.id) return;
      const fromId = String(active.id);
      const toId = String(over.id);
      const from = tracks.findIndex((t) => String(t.id) === fromId);
      const to = tracks.findIndex((t) => String(t.id) === toId);
      if (from === -1 || to === -1) return;
      onReorder(from, to);
    },
    [tracks, onReorder],
  );

  const handleDragCancel = useCallback(() => setActiveId(null), []);

  const activeTrack = activeId
    ? tracks.find((t) => String(t.id) === activeId)
    : null;
  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span aria-hidden="true" />
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
      <DndContext
        sensors={sensors}
        collisionDetection={closestCenter}
        modifiers={[restrictToVerticalAxis]}
        // Always-measure works best with virtualization: rows entering the
        // window during a drag scroll get measured on the fly instead of
        // a single synchronous burst on the first dragmove.
        measuring={{ droppable: { strategy: MeasuringStrategy.Always } }}
        onDragStart={handleDragStart}
        onDragEnd={handleDragEnd}
        onDragCancel={handleDragCancel}
      >
        <SortableContext items={ids} strategy={verticalListSortingStrategy}>
          <div
            ref={scrollRef}
            className={`max-h-[65vh] overflow-y-auto ${
              isLoading ? "opacity-50" : ""
            }`}
          >
            <div
              style={{
                height: `${rowVirtualizer.getTotalSize()}px`,
                position: "relative",
                width: "100%",
              }}
            >
              {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                const track = tracks[virtualRow.index];
                if (!track) return null;
                return (
                  <SortablePlaylistRow
                    key={track.id}
                    track={track}
                    index={virtualRow.index}
                    rowHeight={PLAYLIST_ROW_HEIGHT}
                    top={virtualRow.start}
                    gridCols={gridCols}
                    isCurrent={track.id === currentTrackId}
                    isLiked={likedIds.has(track.id)}
                    isRowSelected={isSelected(track.id)}
                    likeLabel={likeLabel}
                    unlikeLabel={unlikeLabel}
                    unknownLabel={unknownLabel}
                    onPlayTrack={onPlayTrack}
                    onContextMenuRow={onContextMenuRow}
                    onToggleLike={onToggleLike}
                    onNavigateToArtist={onNavigateToArtist}
                    onRowSelect={onRowSelect}
                  />
                );
              })}
            </div>
          </div>
        </SortableContext>
        {/* Portal the overlay to <body> so it stays positioned relative
            to the viewport even if a future ancestor introduces a
            `transform` (which would make it the containing block for
            `position: fixed` and pin the overlay off-screen). */}
        {createPortal(
          <DragOverlay dropAnimation={null}>
            {activeTrack ? (
              <PlaylistRowPreview track={activeTrack} unknownLabel={unknownLabel} />
            ) : null}
          </DragOverlay>,
          document.body,
        )}
      </DndContext>
    </div>
  );
}

function PlaylistRowPreview({
  track,
  unknownLabel,
}: {
  track: Track;
  unknownLabel: string;
}) {
  return (
    <div className="flex items-center space-x-3 p-2 rounded-lg bg-white dark:bg-zinc-800 shadow-lg border border-zinc-200 dark:border-zinc-700 select-none">
      <div className="shrink-0 p-1 -ml-1 text-zinc-400">
        <GripVertical size={14} />
      </div>
      <Artwork
        path={track.artwork_path}
        className="w-10 h-10"
        iconSize={18}
        alt={track.album_title ?? track.title}
        rounded="md"
      />
      <div className="flex-1 min-w-0">
        <div className="text-sm truncate text-zinc-800 dark:text-zinc-200">
          {track.title}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {track.artist_name ?? unknownLabel}
        </div>
      </div>
    </div>
  );
}

interface SortablePlaylistRowProps {
  track: Track;
  index: number;
  /** Pixel offset from the virtualizer for this row's slot. */
  top: number;
  rowHeight: number;
  gridCols: string;
  isCurrent: boolean;
  isLiked: boolean;
  isRowSelected: boolean;
  likeLabel: string;
  unlikeLabel: string;
  unknownLabel: string;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  onToggleLike: (trackId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
}

const SortablePlaylistRow = memo(function SortablePlaylistRow({
  track,
  index,
  top,
  rowHeight,
  gridCols,
  isCurrent,
  isLiked,
  isRowSelected,
  likeLabel,
  unlikeLabel,
  unknownLabel,
  onPlayTrack,
  onContextMenuRow,
  onToggleLike,
  onNavigateToArtist,
  onRowSelect,
}: SortablePlaylistRowProps) {
  // Disable dnd-kit's per-item layout animations: they trigger CSS
  // transitions on every neighbour the drag crosses, which is what
  // makes the row feel sluggish on long playlists. The visual jump
  // when items snap into their new slot is barely noticeable, and
  // the drag itself is now silky-smooth.
  const { attributes, listeners, setNodeRef, transform, isDragging } = useSortable({
    id: String(track.id),
    animateLayoutChanges: () => false,
  });
  // Place the row's slot via CSS `top` (not via a translateY
  // transform): dnd-kit anchors the drag overlay and resolves drop
  // targets from `offsetTop`, which doesn't see CSS transforms. With
  // `transform: translateY(start)` every row reports `offsetTop = 0`
  // and dnd-kit thinks they're all stacked at the parent's top edge,
  // making the overlay snap to viewport top and collisions resolve to
  // whichever row is first in the DOM. Using `top` keeps offsetTop
  // honest. useSortable's own transform (intra-drag displacement) is
  // kept as the only `transform` on the element so it composes
  // cleanly with `top` instead of fighting it.
  const sortableTransform = CSS.Transform.toString(transform);
  const style: React.CSSProperties = {
    position: "absolute",
    top: `${top}px`,
    left: 0,
    width: "100%",
    height: `${rowHeight}px`,
    transform: sortableTransform || undefined,
    // While this row is the drag source, `<DragOverlay>` shows the
    // visible copy that follows the cursor. We hide the in-place
    // copy but keep it mounted to preserve its slot for neighbour
    // layout calculations.
    opacity: isDragging ? 0 : 1,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      onClick={(e) => onRowSelect(track, e)}
      onDoubleClick={() => onPlayTrack(index)}
      onContextMenu={(e) => onContextMenuRow(e, track)}
      className={`group grid ${gridCols} gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 ${
        isRowSelected
          ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
          : isCurrent
          ? "bg-emerald-50 dark:bg-emerald-900/20"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label="Drag to reorder"
        className="shrink-0 p-1 -ml-1 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 group-hover:opacity-100 transition-opacity"
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
      >
        <GripVertical size={14} />
      </button>
      <span
        className={`text-right text-sm tabular-nums ${
          isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
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
          aria-label={isLiked ? unlikeLabel : likeLabel}
          className={`p-1 rounded-full transition-colors ${
            isLiked
              ? "text-pink-500"
              : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
          }`}
        >
          <Heart size={14} className={isLiked ? "fill-current" : ""} />
        </button>
      </div>
    </div>
  );
});
