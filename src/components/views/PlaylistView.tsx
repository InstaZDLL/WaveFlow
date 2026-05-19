import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
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
  Download,
  ArrowUpDown,
  Check,
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
import { usePageScroll } from "../../hooks/usePageScroll";
import { Artwork } from "../common/Artwork";
import { AlbumLink } from "../common/AlbumLink";
import { ArtistLink } from "../common/ArtistLink";
import { Tooltip } from "../common/Tooltip";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  exportPlaylistM3u,
  getPlaylist,
  reorderPlaylistTrack,
  type Playlist,
} from "../../lib/tauri/playlist";
import { pickSaveFile } from "../../lib/tauri/dialog";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { useSortMemory } from "../../hooks/useSortMemory";

/**
 * Sort modes for the playlist track list. "custom" preserves the
 * user-curated drag-and-drop order stored as `playlist_track.position` —
 * any other mode is a display-only client-side sort that doesn't touch
 * the DB. Switching back to "custom" restores the persisted order
 * verbatim, Spotify-style.
 */
type PlaylistSortMode =
  | "custom"
  | "title"
  | "artist"
  | "album"
  | "added_at"
  | "duration_ms"
  | "filename";

const PLAYLIST_SORT_MODES: ReadonlyArray<PlaylistSortMode> = [
  "custom",
  "title",
  "artist",
  "album",
  "added_at",
  "duration_ms",
  "filename",
];

/** Cross-platform basename — handles both Windows (`\`) and POSIX
 *  (`/`) separators since profiles can ship libraries scanned on
 *  either OS, and an imported `.waveflow` archive may cross
 *  platforms. */
function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  const back = path.lastIndexOf("\\");
  return path.slice(Math.max(slash, back) + 1);
}

function isPlaylistSortMode(value: string): value is PlaylistSortMode {
  return (PLAYLIST_SORT_MODES as readonly string[]).includes(value);
}

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
  const { playTracks, currentTrack, toggleShuffle, isPlaying } = usePlayer();
  const {
    updatePlaylist,
    deletePlaylist,
    getPlaylistTracks,
    playlists,
    removeTrackFromPlaylist,
    createPlaylist,
  } = usePlaylist();
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);

  const [playlist, setPlaylist] = useState<Playlist | null>(null);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  // Per-playlist sort mode, persisted in `profile_setting['sort.playlist:<id>']`
  // via `useSortMemory`. The hook keeps a `direction` field for API
  // symmetry with the library view, but the playlist UI only exposes
  // the orderBy axis — direction is implied by the mode (title/artist/
  // album = asc, added_at/duration_ms = desc, custom = stored order).
  const sortContextKey =
    playlistId != null ? `playlist:${playlistId}` : "playlist:none";
  const playlistSort = useSortMemory(sortContextKey, {
    orderBy: "custom",
    direction: "asc",
  });
  const sortMode: PlaylistSortMode = isPlaylistSortMode(
    playlistSort.sort.orderBy,
  )
    ? playlistSort.sort.orderBy
    : "custom";
  const setSortMode = useCallback(
    (mode: PlaylistSortMode) => {
      playlistSort.setSort({ orderBy: mode, direction: "asc" });
    },
    [playlistSort],
  );
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

  // Client-side sort. "custom" preserves the stored order verbatim;
  // every other mode is a stable JS sort by the relevant field. The
  // comparator picks an axis-appropriate direction (alphabetical for
  // title/artist/album, most-recent-first for added_at, longest-first
  // for duration) so the dropdown stays single-axis — the screen we
  // mirror (Spotify) doesn't expose a direction toggle either.
  const displayTracks = useMemo<Track[]>(() => {
    if (sortMode === "custom") return tracks;
    const collator = new Intl.Collator(undefined, {
      numeric: true,
      sensitivity: "base",
    });
    const sorted = [...tracks];
    switch (sortMode) {
      case "title":
        sorted.sort((a, b) => collator.compare(a.title, b.title));
        break;
      case "artist":
        sorted.sort((a, b) =>
          collator.compare(a.artist_name ?? "", b.artist_name ?? ""),
        );
        break;
      case "album":
        sorted.sort((a, b) =>
          collator.compare(a.album_title ?? "", b.album_title ?? ""),
        );
        break;
      case "added_at":
        sorted.sort((a, b) => (b.added_at ?? 0) - (a.added_at ?? 0));
        break;
      case "duration_ms":
        sorted.sort((a, b) => (b.duration_ms ?? 0) - (a.duration_ms ?? 0));
        break;
      case "filename":
        // Numeric collator gives a natural order on "1 …", "2 …",
        // "10 …" filenames — the most common manual-numbering scheme
        // (matches Explorer / Finder behaviour). Sorted on the basename
        // only so users grouping by parent folder still see filename
        // order, not full-path lexicographic order.
        sorted.sort((a, b) =>
          collator.compare(basename(a.file_path), basename(b.file_path)),
        );
        break;
    }
    return sorted;
  }, [tracks, sortMode]);
  const displayTracksRef = useRef<Track[]>(displayTracks);
  useEffect(() => {
    displayTracksRef.current = displayTracks;
  }, [displayTracks]);

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
      // Range selection follows what the user sees on screen — sorting
      // doesn't change which rows belong to the visual range between
      // anchor and target, so the displayed array is the right input.
      const items = displayTracksRef.current;
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

  const handleExportM3u = useCallback(async () => {
    if (!playlist) return;
    const safeName =
      playlist.name.replace(/[\\/:*?"<>|]/g, "_").trim() || "playlist";
    const dest = await pickSaveFile(
      `${safeName}.m3u8`,
      ["m3u8", "m3u"],
      t("playlistView.export.dialogTitle"),
    );
    if (!dest) return;
    try {
      await exportPlaylistM3u(playlist.id, dest);
    } catch (err) {
      console.error("[PlaylistView] export m3u failed", err);
    }
  }, [playlist, t]);

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
      // Play in the order the user is seeing — not the stored order —
      // so a sort-by-Title view enqueues the alphabetical sequence and
      // "next" stays sensible.
      const current = displayTracksRef.current;
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
    selectedTrackIds: [...selection.selectedIds],
  });
  const onContextMenuRow = trackContextMenu.open;

  // Fetch playlist + its tracks whenever the focused id changes. Also
  // re-runs when the playlist list itself updates (e.g. after rename via
  // `updatePlaylist`) so the header reflects the new metadata without a
  // manual refresh.
  const playlistsSignature = playlists
    .map((p) => `${p.id}:${p.updated_at}`)
    .join(",");
  // Bumped by the `track:updated` listener below — flips the effect
  // deps so a tag edit triggers a fresh fetch even when neither the
  // playlist id nor its `updated_at` changed.
  const [refetchKey, setRefetchKey] = useState(0);
  useTrackUpdated(useCallback(() => setRefetchKey((k) => k + 1), []));
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
  }, [playlistId, playlistsSignature, getPlaylistTracks, refetchKey]);

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
    if (displayTracks.length === 0) return;
    await playTracks(displayTracks, 0, { type: "playlist", id: playlistId });
  };

  const handleShufflePlay = async () => {
    if (displayTracks.length === 0) return;
    await playTracks(displayTracks, 0, { type: "playlist", id: playlistId });
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
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header. Smart playlists (Daily Mix, …) ship a generated cover
          image — render it as a 96×96 tile with a "DAILY MIX" overlay
          label. User-curated playlists fall back to the icon + color
          gradient tile they always had. */}
      {(() => {
        const coverUrl = playlist
          ? resolveRemoteImage(playlist.cover_path, null)
          : null;
        const isSmart = (playlist?.is_smart ?? 0) === 1;
        return (
          <div
            className={`flex items-start justify-between p-6 rounded-2xl ${color.previewBg}`}
          >
            <div className="flex items-center space-x-6">
              <div
                className={`relative w-24 h-24 rounded-2xl overflow-hidden shadow-sm flex items-center justify-center ${
                  coverUrl ? "" : `${color.tileBg} ${color.tileText}`
                }`}
              >
                {coverUrl ? (
                  <>
                    <img
                      src={coverUrl}
                      alt=""
                      className="absolute inset-0 w-full h-full object-cover"
                      loading="lazy"
                    />
                    {isSmart && (
                      <>
                        <div className="absolute inset-x-0 bottom-0 h-1/2 bg-linear-to-t from-black/70 to-transparent" />
                        <div className="absolute bottom-1.5 left-2 right-2 text-[9px] font-bold tracking-widest text-white uppercase truncate">
                          {t("playlistView.smartLabel", "Daily Mix")}
                        </div>
                      </>
                    )}
                  </>
                ) : playlist ? (
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

                <Tooltip label={t("playlistView.actions.exportM3u")}>
                  <button
                    type="button"
                    onClick={handleExportM3u}
                    disabled={tracks.length === 0}
                    aria-label={t("playlistView.actions.exportM3u")}
                    className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <Download size={18} />
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
        );
      })()}

      {/* Sort selector. The dropdown is omitted on empty playlists to
          keep the empty-state visually clean. Only mounted once the
          per-playlist sort memory has finished hydrating so the active
          option doesn't flash from "Custom" to the persisted value on
          first paint. */}
      {tracks.length > 0 && playlistSort.isLoaded && (
        <div className="flex items-center justify-end -mt-4">
          <PlaylistSortMenu current={sortMode} onChange={setSortMode} t={t} />
        </div>
      )}

      {/* Tracks list */}
      {tracks.length > 0 ? (
        <PlaylistTrackTable
          tracks={displayTracks}
          isLoading={isLoading}
          currentTrackId={currentTrack?.id ?? null}
          isPlaying={isPlaying}
          onPlayTrack={handlePlayTrackByIndex}
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onNavigateToAlbum={onNavigateToAlbum}
          onNavigateToArtist={onNavigateToArtist}
          unknownLabel={unknownLabel}
          headerLabels={headerLabels}
          likeLabel={likeLabel}
          unlikeLabel={unlikeLabel}
          onContextMenuRow={onContextMenuRow}
          onReorder={handleReorder}
          isSelected={selection.isSelected}
          onRowSelect={handleRowSelect}
          dragEnabled={sortMode === "custom"}
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
        onCoverChanged={async () => {
          // Cover backend command already wrote the new hash; pull the
          // fresh row so `cover_path` updates without waiting for the
          // next user navigation.
          if (playlistId == null) return;
          try {
            const fresh = await getPlaylist(playlistId);
            setPlaylist(fresh);
          } catch (err) {
            console.error("[PlaylistView] refresh after cover change", err);
          }
        }}
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
  isPlaying: boolean;
  onPlayTrack: (index: number) => void;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  onNavigateToAlbum: (albumId: number) => void;
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
  /**
   * When `false` the rows render without grip handles and `onReorder`
   * is never invoked — the playlist is in a non-custom sort mode and
   * drag-to-reorder would mutate the stored order in ways the user
   * isn't asking for.
   */
  dragEnabled: boolean;
}

const PLAYLIST_ROW_HEIGHT = 56;

function PlaylistTrackTable({
  tracks,
  isLoading,
  currentTrackId,
  isPlaying,
  onPlayTrack,
  likedIds,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  unknownLabel,
  headerLabels,
  likeLabel,
  unlikeLabel,
  onContextMenuRow,
  onReorder,
  isSelected,
  onRowSelect,
  dragEnabled,
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

  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  // Re-anchor the virtualizer whenever the row container moves within the
  // page scroller (e.g. header expands/collapses). `scrollMargin` tells
  // tanstack-virtual how far down the scroller our rows actually start.
  const [scrollMargin, setScrollMargin] = useState(0);
  useLayoutEffect(() => {
    const parent = parentRef.current;
    const scroller = pageScrollRef?.current;
    if (!parent || !scroller) return;
    const recompute = () => {
      const parentRect = parent.getBoundingClientRect();
      const scrollerRect = scroller.getBoundingClientRect();
      setScrollMargin(parentRect.top - scrollerRect.top + scroller.scrollTop);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(parent);
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [pageScrollRef, tracks.length]);

  // Virtualize the row list. SortableContext keeps the *full* id array
  // so dnd-kit knows the abstract ordering, even for items that aren't
  // currently mounted — only the on-screen window pays the useSortable
  // cost. This is what makes grab-on-300+-tracks feel instant.
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => PLAYLIST_ROW_HEIGHT,
    overscan: 8,
    scrollMargin,
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
    // Borderless wrapper so rows span the full content width Spotify-style
    // — the page-level scroller already provides the visual frame, and a
    // contained card here just shrunk every row by ~40 px on each side.
    // The column header keeps its bottom border for the visual separator.
    <div>
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-200 dark:border-zinc-800`}
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
            ref={parentRef}
            className={isLoading ? "opacity-50" : ""}
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
                  top={virtualRow.start - scrollMargin}
                  gridCols={gridCols}
                  isCurrent={track.id === currentTrackId}
                  isPlaying={isPlaying}
                  isLiked={likedIds.has(track.id)}
                  isRowSelected={isSelected(track.id)}
                  likeLabel={likeLabel}
                  unlikeLabel={unlikeLabel}
                  unknownLabel={unknownLabel}
                  onPlayTrack={onPlayTrack}
                  onContextMenuRow={onContextMenuRow}
                  onToggleLike={onToggleLike}
                  onNavigateToAlbum={onNavigateToAlbum}
                  onNavigateToArtist={onNavigateToArtist}
                  onRowSelect={onRowSelect}
                  dragEnabled={dragEnabled}
                />
              );
            })}
          </div>
        </SortableContext>
        {/* Portal the overlay to <body> so it stays positioned relative
            to the viewport even if a future ancestor introduces a
            `transform` (which would make it the containing block for
            `position: fixed` and pin the overlay off-screen). */}
        {createPortal(
          <DragOverlay dropAnimation={null}>
            {activeTrack ? (
              <PlaylistRowPreview
                track={activeTrack}
                unknownLabel={unknownLabel}
              />
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
  isPlaying: boolean;
  isLiked: boolean;
  isRowSelected: boolean;
  likeLabel: string;
  unlikeLabel: string;
  unknownLabel: string;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  onToggleLike: (trackId: number) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
  /** Hide the grip handle when the playlist is in a non-custom sort
   *  mode. dnd-kit's listeners are also detached so a desperate
   *  click-drag on the row body can't trigger a backend reorder.
   */
  dragEnabled: boolean;
}

const SortablePlaylistRow = memo(function SortablePlaylistRow({
  track,
  index,
  top,
  rowHeight,
  gridCols,
  isCurrent,
  isPlaying,
  isLiked,
  isRowSelected,
  likeLabel,
  unlikeLabel,
  unknownLabel,
  onPlayTrack,
  onContextMenuRow,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  onRowSelect,
  dragEnabled,
}: SortablePlaylistRowProps) {
  // Disable dnd-kit's per-item layout animations: they trigger CSS
  // transitions on every neighbour the drag crosses, which is what
  // makes the row feel sluggish on long playlists. The visual jump
  // when items snap into their new slot is barely noticeable, and
  // the drag itself is now silky-smooth.
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useSortable({
      id: String(track.id),
      animateLayoutChanges: () => false,
      disabled: !dragEnabled,
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
    // Row can't be a <button> because it contains action buttons
    // (heart, more-options) and a drag handle; nested buttons are
    // invalid HTML. Keyboard activation still works via tabIndex +
    // onKeyDown.
    <div
      ref={setNodeRef}
      style={style}
      tabIndex={0}
      onClick={(e) => onRowSelect(track, e)}
      onDoubleClick={() => onPlayTrack(index)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onPlayTrack(index);
        }
      }}
      onContextMenu={(e) => onContextMenuRow(e, track)}
      className={`group grid ${gridCols} gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
        isRowSelected
          ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
          : isCurrent
            ? "bg-emerald-50 dark:bg-emerald-900/20"
            : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      {dragEnabled ? (
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
      ) : (
        // Empty slot so the row grid columns don't reshuffle when the
        // sort mode flips. Keeps the title / artist / album lined up
        // with the column header.
        <span aria-hidden="true" />
      )}
      <span
        className={`text-right text-sm tabular-nums flex items-center justify-end ${
          isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
        }`}
      >
        {isCurrent ? <PlayingIndicator isPlaying={isPlaying} /> : index + 1}
      </span>
      <Artwork
        path={track.artwork_path}
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
      <ArtistLink
        name={track.artist_name}
        artistIds={track.artist_ids}
        onNavigate={onNavigateToArtist}
        fallback={unknownLabel}
        className="text-sm text-zinc-500 truncate"
      />
      <AlbumLink
        title={track.album_title}
        albumId={track.album_id}
        onNavigate={onNavigateToAlbum}
        fallback={unknownLabel}
        className="text-sm text-zinc-500 truncate"
      />
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

// =============================================================================
// Playlist sort menu (Spotify-style: list of modes + check, no direction)
// =============================================================================

interface PlaylistSortMenuProps {
  current: PlaylistSortMode;
  onChange: (next: PlaylistSortMode) => void;
  // i18next's `t` is heavily overloaded — accept it whole rather than
  // re-declaring a subset that the type checker would reject.
  t: ReturnType<typeof useTranslation>["t"];
}

function PlaylistSortMenu({ current, onChange, t }: PlaylistSortMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", onClickOutside);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClickOutside);
      document.removeEventListener("keydown", onKey);
    };
  }, [isOpen]);

  // i18n labels with inline fallbacks so the menu works in every locale
  // without forcing a 17-file translation pass for keys most users
  // never see. The fallback strings stay in English so the option set
  // is at least intelligible if a translation drops a key.
  const labels: Record<PlaylistSortMode, string> = {
    custom: t("sort.customOrder", "Custom order"),
    title: t("sort.title"),
    artist: t("sort.artist"),
    album: t("sort.album"),
    added_at: t("sort.recentlyAdded", "Recently added"),
    duration_ms: t("sort.duration"),
    filename: t("sort.filename"),
  };

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((p) => !p)}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        className="flex items-center space-x-2 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
      >
        <ArrowUpDown size={14} />
        <span>{labels[current]}</span>
      </button>
      {isOpen && (
        <ul
          role="listbox"
          className="absolute top-full right-0 mt-2 min-w-56 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated overflow-hidden z-50 animate-fade-in py-1"
        >
          <li
            className="px-4 pt-1 pb-2 text-[10px] font-bold tracking-widest text-zinc-400 uppercase"
            aria-hidden="true"
          >
            {t("sort.menuTitle", "Sort by")}
          </li>
          {PLAYLIST_SORT_MODES.map((mode) => {
            const isSelected = mode === current;
            return (
              <li key={mode} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => {
                    onChange(mode);
                    setIsOpen(false);
                  }}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30"
                  }`}
                >
                  <span>{labels[mode]}</span>
                  {isSelected && <Check size={14} />}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
