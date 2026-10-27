import { type PlaylistTrack } from "./playlistTypes";
import { formatDuration, type Track } from "../../../lib/tauri/track";
import { memo } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { isPlayClick } from "../../../hooks/useSingleClickPlay";
import { GripVertical, Heart, X } from "lucide-react";
import { RowSelectCheckbox } from "../../common/RowSelectCheckbox";
import { PlayingIndicator } from "../../common/PlayingIndicator";
import { RemoteArtwork } from "../../common/RemoteArtwork";
import { Artwork } from "../../common/Artwork";
import { HiResBadge } from "../../common/HiResBadge";
import { ArtistLink } from "../../common/ArtistLink";
import { AlbumLink } from "../../common/AlbumLink";

interface SortablePlaylistRowProps {
  track: PlaylistTrack;
  /** Settings → single-click play, read once by the table. */
  singleClickPlay: boolean;
  index: number;
  /** Pixel offset from the virtualizer for this row's slot. */
  top: number;
  rowHeight: number;
  gridCols: string;
  /** From the server rather than the device. Gates the artwork source,
   *  the artist / album links, what the heart writes, and the local-only
   *  selection and context menu. */
  remote: boolean;
  isCurrent: boolean;
  isPlaying: boolean;
  isLiked: boolean;
  isRowSelected: boolean;
  /** Something is selected: the checkbox shows on every row. */
  selectionActive: boolean;
  /** Absent on a server row, which cannot be selected. */
  onToggleSelect?: () => void;
  likeLabel: string;
  unlikeLabel: string;
  unknownLabel: string;
  reorderLabel: string;
  removeLabel: string;
  awaitingLabel: string;
  /** Remote only: a server write is in flight. */
  busy: boolean;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  onToggleLike: (track: PlaylistTrack) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
  /** Remote only, and only in the curated order: drop this entry. */
  onRemoveAt?: (index: number) => void;
  onRowSelect: (track: PlaylistTrack, e: React.MouseEvent) => void;
  /** Hide the grip handle when the playlist is in a non-custom sort
   *  mode. dnd-kit's listeners are also detached so a desperate
   *  click-drag on the row body can't trigger a backend reorder.
   */
  dragEnabled: boolean;
}

export const SortablePlaylistRow = memo(function SortablePlaylistRow({
  track,
  singleClickPlay,
  index,
  top,
  rowHeight,
  gridCols,
  remote,
  isCurrent,
  isPlaying,
  isLiked,
  isRowSelected,
  selectionActive,
  onToggleSelect,
  likeLabel,
  unlikeLabel,
  unknownLabel,
  reorderLabel,
  removeLabel,
  awaitingLabel,
  busy,
  onPlayTrack,
  onContextMenuRow,
  onRowMenuKey,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  onRemoveAt,
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
      role="button"
      onClick={(e) => {
        if (isPlayClick(e, singleClickPlay)) {
          onPlayTrack(index);
          return;
        }
        // The second click of a double-click: the first one already
        // played the row, and selecting it now would paint the selection
        // over the track it started (#897).
        if (singleClickPlay && e.detail > 1) return;
        onRowSelect(track, e);
      }}
      onDoubleClick={() => {
        if (!singleClickPlay) onPlayTrack(index);
      }}
      onKeyDown={(e) => {
        // Only play when the row itself is focused — see `library/TrackTable`.
        if (e.target !== e.currentTarget) return;
        // The context menu acts on a rowid throughout; a server row has
        // none, so it is not offered rather than opened onto dead items.
        if (!remote && onRowMenuKey(e, track)) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onPlayTrack(index);
        }
      }}
      onKeyUp={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === " ") e.preventDefault();
      }}
      onContextMenu={(e) => {
        if (remote) return;
        onContextMenuRow(e, track);
      }}
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
          aria-label={reorderLabel}
          className="shrink-0 p-1 -ml-1 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 reveal:opacity-100 transition-opacity"
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
        {onToggleSelect ? (
          <RowSelectCheckbox
            checked={isRowSelected}
            selectionActive={selectionActive}
            onToggle={onToggleSelect}
            title={track.title}
          >
            {isCurrent ? <PlayingIndicator isPlaying={isPlaying} /> : index + 1}
          </RowSelectCheckbox>
        ) : isCurrent ? (
          <PlayingIndicator isPlaying={isPlaying} />
        ) : (
          index + 1
        )}
      </span>
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
      <span
        className={`text-sm truncate flex items-center gap-2 ${
          isCurrent
            ? "text-emerald-600 dark:text-emerald-400 font-semibold"
            : "text-zinc-800 dark:text-zinc-200"
        }`}
      >
        <span className="truncate">
          {track.title || (remote ? awaitingLabel : "")}
        </span>
        <HiResBadge
          bitDepth={track.bit_depth}
          sampleRate={track.sample_rate}
          codec={track.codec}
          variant="inline"
        />
      </span>
      {/* The server's ids are strings, so they cannot ride in the numeric
          `artist_id` / `album_id` the shared links navigate by. */}
      {remote ? (
        <RemoteEntityLink
          name={track.artist_name}
          id={track.remote_artist_id}
          onNavigate={onNavigateToRemoteArtist}
        />
      ) : (
        <ArtistLink
          name={track.artist_name}
          artistIds={track.artist_ids}
          onNavigate={onNavigateToArtist}
          fallback={unknownLabel}
          className="text-sm text-zinc-500 truncate"
        />
      )}
      {remote ? (
        <RemoteEntityLink
          name={track.album_title}
          id={track.remote_album_id}
          onNavigate={onNavigateToRemoteAlbum}
        />
      ) : (
        <AlbumLink
          title={track.album_title}
          albumId={track.album_id}
          onNavigate={onNavigateToAlbum}
          fallback={unknownLabel}
          className="text-sm text-zinc-500 truncate"
        />
      )}
      <span className="text-sm tabular-nums text-zinc-400 text-right">
        {formatDuration(track.duration_ms)}
      </span>
      <div className="flex justify-center">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            void onToggleLike(track);
          }}
          aria-label={isLiked ? unlikeLabel : likeLabel}
          aria-pressed={isLiked}
          className={`p-1 rounded-full transition-colors ${
            isLiked
              ? "text-pink-500"
              : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
          }`}
        >
          <Heart size={14} className={isLiked ? "fill-current" : ""} />
        </button>
      </div>
      {/* Remote only. Removing acts on the entry's position, which is why
          the column is empty under a display sort — the same reason the
          grip is. An empty slot keeps the columns lined up with the
          header. */}
      {remote &&
        (onRemoveAt ? (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onRemoveAt(index);
            }}
            disabled={busy}
            className="p-1 rounded text-zinc-300 dark:text-zinc-600 opacity-0 reveal:opacity-100 hover:text-red-500 dark:hover:text-red-400 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
            aria-label={removeLabel}
            title={removeLabel}
          >
            <X size={15} />
          </button>
        ) : (
          <span aria-hidden="true" />
        ))}
    </div>
  );
});

/**
 * An artist or album cell for a server track: a link when the server gave
 * us an id to navigate by, plain text when it did not.
 */
function RemoteEntityLink({
  name,
  id,
  onNavigate,
}: {
  name: string | null;
  id: string | null | undefined;
  onNavigate?: (id: string) => void;
}) {
  if (name && id && onNavigate) {
    return (
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onNavigate(id);
          }}
          className="truncate max-w-full text-left hover:text-emerald-600 dark:hover:text-emerald-400 hover:underline"
          title={name}
        >
          {name}
        </button>
      </div>
    );
  }
  return (
    <div className="min-w-0 text-sm text-zinc-500 truncate">{name ?? "—"}</div>
  );
}
