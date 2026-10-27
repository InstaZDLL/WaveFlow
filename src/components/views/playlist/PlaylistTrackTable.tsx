import { type PlaylistTrack } from "./playlistTypes";
import { type PlaylistPlaceholder } from "../../../lib/tauri/playlist";
import { formatDuration, type Track } from "../../../lib/tauri/track";
import { useSingleClickPlay } from "../../../hooks/useSingleClickPlay";
import {
  DndContext,
  DragOverlay,
  MeasuringStrategy,
  PointerSensor,
  closestCenter,
  type DragEndEvent,
  type DragStartEvent,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { usePageScroll } from "../../../hooks/usePageScroll";
import { useVirtualizer } from "@tanstack/react-virtual";
import { CircleDashed, Clock, GripVertical, X } from "lucide-react";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import {
  SortableContext,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { SortablePlaylistRow } from "./SortablePlaylistRow";
import { createPortal } from "react-dom";
import { Tooltip } from "../../common/Tooltip";
import { RemoteArtwork } from "../../common/RemoteArtwork";
import { Artwork } from "../../common/Artwork";

interface PlaylistTrackTableProps {
  tracks: PlaylistTrack[];
  /** Entries of an imported playlist the library does not hold yet. */
  placeholders: PlaylistPlaceholder[];
  /** In the playlist's own order each placeholder sits before the track
   *  it waits in front of; under a display sort there is no such place,
   *  so they follow the tracks. */
  interleavePlaceholders: boolean;
  placeholderLabels: { missing: string; remove: string };
  onRemovePlaceholder: (placeholderId: number) => void;
  isLoading: boolean;
  /** The whole table comes from one source — a playlist is one or the
   *  other, never a mix — so the branch is decided once here rather than
   *  per row. */
  remote: boolean;
  currentTrackId: number | null;
  /** Server id of the track playing now, read off the live remote queue.
   *  A remote row's own `id` is a negative sentinel, so matching on it
   *  would highlight nothing, quietly. */
  currentRemoteId: string | null;
  isPlaying: boolean;
  onPlayTrack: (index: number) => void;
  likedIds: Set<number>;
  onToggleLike: (track: PlaylistTrack) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
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
  reorderLabel: string;
  removeLabel: string;
  awaitingLabel: string;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  onReorder: (fromIndex: number, toIndex: number) => void;
  /** Remote only: drop the entry at this index. The local side removes
   *  from the context menu, which a server row has no equivalent of. */
  onRemoveAt?: (index: number) => void;
  /** Remote only: a server write is in flight. */
  busy: boolean;
  isSelected: (id: number) => boolean;
  /** Something is selected: every row shows its checkbox. */
  selectionActive: boolean;
  /** The row checkbox: a Ctrl+click without the modifier. */
  onToggleSelect: (id: number) => void;
  onRowSelect: (track: PlaylistTrack, e: React.MouseEvent) => void;
  /**
   * When `false` the rows render without grip handles and `onReorder`
   * is never invoked — the playlist is in a non-custom sort mode and
   * drag-to-reorder would mutate the stored order in ways the user
   * isn't asking for.
   */
  dragEnabled: boolean;
}

const PLAYLIST_ROW_HEIGHT = 56;

/** A row of the table: a track, or a placeholder waiting for one. */
type PlaylistTableRow =
  | { kind: "track"; track: PlaylistTrack; index: number }
  | { kind: "placeholder"; placeholder: PlaylistPlaceholder };

export function PlaylistTrackTable({
  tracks,
  placeholders,
  interleavePlaceholders,
  placeholderLabels,
  onRemovePlaceholder,
  isLoading,
  remote,
  currentTrackId,
  currentRemoteId,
  isPlaying,
  onPlayTrack,
  likedIds,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  unknownLabel,
  headerLabels,
  likeLabel,
  unlikeLabel,
  reorderLabel,
  removeLabel,
  awaitingLabel,
  onContextMenuRow,
  onRowMenuKey,
  onReorder,
  onRemoveAt,
  busy,
  isSelected,
  selectionActive,
  onToggleSelect,
  onRowSelect,
  dragEnabled,
}: PlaylistTrackTableProps) {
  "use no memo";
  const { value: singleClickPlay } = useSingleClickPlay();
  // A remote table carries a trailing remove column; the local one puts
  // that in the context menu. Same template otherwise, so the two read
  // alike column for column.
  const gridCols = remote
    ? "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem_2rem]"
    : "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem]";
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
  );
  // Per-row stable IDs. Locally `track.id` is the playlist's PRIMARY KEY
  // and can't repeat (a track is in a playlist either zero or one times).
  // A remote row's id is the projection's negative sentinel, handed out
  // once per position at load time — distinct even when the server holds
  // the same track twice, and stable across an optimistic reorder because
  // the rows move rather than being re-projected.
  const ids = useMemo(() => tracks.map((t) => String(t.id)), [tracks]);

  // Tracks and placeholders in display order. A track keeps its index
  // among the tracks — the number it shows and the one played from — and
  // a placeholder takes none: it is not a track the queue could reach.
  const rows = useMemo<PlaylistTableRow[]>(() => {
    const trackRows = tracks.map((track, index): PlaylistTableRow => ({
      kind: "track",
      track,
      index,
    }));
    if (placeholders.length === 0) return trackRows;
    const asRows = (list: PlaylistPlaceholder[]) =>
      list.map((placeholder): PlaylistTableRow => ({
        kind: "placeholder",
        placeholder,
      }));
    if (!interleavePlaceholders) return [...trackRows, ...asRows(placeholders)];
    const before = new Map<number, PlaylistPlaceholder[]>();
    for (const placeholder of placeholders) {
      if (placeholder.before_track_id == null) continue;
      const list = before.get(placeholder.before_track_id) ?? [];
      list.push(placeholder);
      before.set(placeholder.before_track_id, list);
    }
    const out: PlaylistTableRow[] = [];
    const placed = new Set<number>();
    for (const row of trackRows) {
      if (row.kind === "track") {
        for (const placeholder of before.get(row.track.id) ?? []) {
          out.push({ kind: "placeholder", placeholder });
          placed.add(placeholder.id);
        }
      }
      out.push(row);
    }
    // The tail, and any placeholder whose track is not on screen.
    return [...out, ...asRows(placeholders.filter((p) => !placed.has(p.id)))];
  }, [tracks, placeholders, interleavePlaceholders]);

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
  }, [pageScrollRef, rows.length]);

  // Virtualize the row list. SortableContext keeps the *full* id array
  // so dnd-kit knows the abstract ordering, even for items that aren't
  // currently mounted — only the on-screen window pays the useSortable
  // cost. This is what makes grab-on-300+-tracks feel instant.
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: rows.length,
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
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-600 dark:text-zinc-300 uppercase border-b border-zinc-200 dark:border-zinc-800`}
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
        {remote && <span aria-hidden="true" />}
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
              const row = rows[virtualRow.index];
              if (!row) return null;
              if (row.kind === "placeholder") {
                return (
                  <PlaceholderRow
                    key={`placeholder-${row.placeholder.id}`}
                    placeholder={row.placeholder}
                    top={virtualRow.start - scrollMargin}
                    rowHeight={PLAYLIST_ROW_HEIGHT}
                    gridCols={gridCols}
                    labels={placeholderLabels}
                    unknownLabel={unknownLabel}
                    onRemove={onRemovePlaceholder}
                  />
                );
              }
              const { track } = row;
              return (
                <SortablePlaylistRow
                  key={track.id}
                  track={track}
                  singleClickPlay={singleClickPlay}
                  index={row.index}
                  rowHeight={PLAYLIST_ROW_HEIGHT}
                  top={virtualRow.start - scrollMargin}
                  gridCols={gridCols}
                  remote={remote}
                  isCurrent={
                    remote
                      ? currentRemoteId != null &&
                        track.remote_id === currentRemoteId
                      : track.id === currentTrackId
                  }
                  isPlaying={isPlaying}
                  isLiked={
                    remote
                      ? track.remote_starred === true
                      : likedIds.has(track.id)
                  }
                  isRowSelected={!remote && isSelected(track.id)}
                  selectionActive={selectionActive}
                  onToggleSelect={
                    remote ? undefined : () => onToggleSelect(track.id)
                  }
                  likeLabel={likeLabel}
                  unlikeLabel={unlikeLabel}
                  unknownLabel={unknownLabel}
                  reorderLabel={reorderLabel}
                  removeLabel={removeLabel}
                  awaitingLabel={awaitingLabel}
                  busy={busy}
                  onPlayTrack={onPlayTrack}
                  onContextMenuRow={onContextMenuRow}
                  onRowMenuKey={onRowMenuKey}
                  onToggleLike={onToggleLike}
                  onNavigateToAlbum={onNavigateToAlbum}
                  onNavigateToArtist={onNavigateToArtist}
                  onNavigateToRemoteAlbum={onNavigateToRemoteAlbum}
                  onNavigateToRemoteArtist={onNavigateToRemoteArtist}
                  onRemoveAt={dragEnabled ? onRemoveAt : undefined}
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
                remote={remote}
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

/**
 * An entry the library does not hold: shown greyed in its place, with
 * nothing to play and nothing to open — only a way to drop it. It turns
 * into the real row on its own once a scan brings the track in.
 */
function PlaceholderRow({
  placeholder,
  top,
  rowHeight,
  gridCols,
  labels,
  unknownLabel,
  onRemove,
}: {
  placeholder: PlaylistPlaceholder;
  top: number;
  rowHeight: number;
  gridCols: string;
  labels: { missing: string; remove: string };
  unknownLabel: string;
  onRemove: (placeholderId: number) => void;
}) {
  const artist = placeholder.artist ?? unknownLabel;
  return (
    <div
      role="group"
      aria-label={`${placeholder.title} — ${artist} — ${labels.missing}`}
      style={{
        position: "absolute",
        top: `${top}px`,
        left: 0,
        width: "100%",
        height: `${rowHeight}px`,
      }}
      className={`group grid ${gridCols} gap-4 px-5 items-center select-none border-b border-zinc-100 dark:border-zinc-800/60 text-zinc-400 dark:text-zinc-500`}
      data-playlist-placeholder
    >
      <span aria-hidden="true" />
      <span className="text-right text-sm" aria-hidden="true">
        –
      </span>
      <span
        aria-hidden="true"
        className="w-10 h-10 rounded-md border border-dashed border-zinc-300 dark:border-zinc-700 flex items-center justify-center"
      >
        <CircleDashed size={16} />
      </span>
      <div className="min-w-0">
        <p className="truncate text-sm italic">{placeholder.title}</p>
        <p className="truncate text-[11px] uppercase tracking-wider">
          {labels.missing}
        </p>
      </div>
      <span className="truncate text-sm">{artist}</span>
      <span className="truncate text-sm">{placeholder.album ?? ""}</span>
      <span className="text-right text-sm tabular-nums">
        {placeholder.duration_ms != null && placeholder.duration_ms > 0
          ? formatDuration(placeholder.duration_ms)
          : ""}
      </span>
      <Tooltip label={labels.remove}>
        <button
          type="button"
          onClick={() => onRemove(placeholder.id)}
          aria-label={labels.remove}
          className="p-1 rounded-md text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity"
        >
          <X size={14} />
        </button>
      </Tooltip>
    </div>
  );
}

function PlaylistRowPreview({
  track,
  remote,
  unknownLabel,
}: {
  track: PlaylistTrack;
  remote: boolean;
  unknownLabel: string;
}) {
  return (
    <div className="flex items-center space-x-3 p-2 rounded-lg bg-white dark:bg-zinc-800 shadow-lg border border-zinc-200 dark:border-zinc-700 select-none">
      <div className="shrink-0 p-1 -ml-1 text-zinc-400">
        <GripVertical size={14} />
      </div>
      {remote ? (
        <RemoteArtwork
          hash={track.artwork_hash ?? null}
          className="w-10 h-10 rounded-md"
          iconSize={18}
        />
      ) : (
        <Artwork
          path={track.artwork_path}
          className="w-10 h-10"
          iconSize={18}
          alt={track.album_title ?? track.title}
          rounded="md"
        />
      )}
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
