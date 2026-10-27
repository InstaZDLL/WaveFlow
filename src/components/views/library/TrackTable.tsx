import { type LibraryTrackRow } from "../../../lib/tauri/browse";
import { type TracksView, type Translator } from "./libraryTypes";
import {
  listPlaylistsContainingTrack,
  type Playlist,
} from "../../../lib/tauri/playlist";
import {
  formatDuration,
  setTrackRating,
  type Track,
} from "../../../lib/tauri/track";
import {
  MAX_COLUMN_WIDTH,
  cellText,
  fitLayoutToWidth,
  specFor,
  tagKeyOf,
  trackMinWidthFor,
  trackSizeFor,
  type ColumnId,
  type ColumnLayout,
} from "../../../lib/trackColumns";
import { useRemoteTrackContextMenu } from "../../../hooks/useRemoteTrackContextMenu";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { usePageScroll } from "../../../hooks/usePageScroll";
import { useVirtualizer } from "@tanstack/react-virtual";
import { formatBytes } from "../../../lib/format";
import { formatMusicalKey } from "../../../lib/musicalKey";
import { fitWidth, styleOf } from "../../../lib/measureText";
import { TrackTableHeader } from "./TrackTableHeader";
import { toLocalTrack } from "./toLocalTrack";
import { isPlayClick } from "../../../hooks/useSingleClickPlay";
import { RowSelectCheckbox } from "../../common/RowSelectCheckbox";
import { PlayingIndicator } from "../../common/PlayingIndicator";
import { Artwork } from "../../common/Artwork";
import { RemoteArtwork } from "../../common/RemoteArtwork";
import { HiResBadge } from "../../common/HiResBadge";
import { ArtistLink } from "../../common/ArtistLink";
import { AlbumLink } from "../../common/AlbumLink";
import { StarRating } from "../../common/StarRating";
import { Check, Download, FolderDown, Heart, Pencil, Plus } from "lucide-react";
import { AddToPlaylistPopover } from "./AddToPlaylistPopover";

interface TrackTableProps {
  tracks: LibraryTrackRow[];
  isLoading: boolean;
  view: TracksView;
  t: Translator;
  onPlayTrack: (index: number) => void;
  currentTrackId: number | null;
  isPlaying: boolean;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  playlists: Playlist[];
  onAddToPlaylist: (
    playlistId: number,
    trackId: number,
  ) => Promise<void> | void;
  onRemoveFromPlaylist: (
    playlistId: number,
    trackId: number,
  ) => Promise<void> | void;
  onCreatePlaylist: (trackId: number) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  isSelected: (id: number) => boolean;
  /** Something is selected: every row shows its checkbox. */
  selectionActive: boolean;
  /** The row checkbox: a Ctrl+click without the modifier. */
  onToggleSelect: (id: number) => void;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
  /** Whether a plain click plays instead of selecting. The table needs it
   *  because selection speaks in local rowids and a server row has none: it
   *  would otherwise be the only row a click does nothing to. */
  singleClickPlay: boolean;
  /** A server track opens the remote detail views; the two catalogues are
   *  never merged, so they are never the same page. */
  onNavigateToRemoteAlbum: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist: (remoteArtistId: string) => void;
  /** Keep a server track's bytes in the managed folder — offline playback,
   *  still a remote track. */
  onDownloadRemote: (remoteTrackId: string) => void;
  /** Copy a server track into a scanned folder, where it becomes a local
   *  track of the user's own library. */
  onImportRemote: (row: LibraryTrackRow) => void;
  onEditRemoteTags: (remoteTrackId: string) => void;
  /** Server tracks whose download is in flight, so the button can say so. */
  downloadingRemote: Set<string>;
  /** Server tracks already kept offline. */
  downloadedRemote: Set<string>;
  /** For the date and byte-size columns (#588). */
  locale: string;
  /** Which columns to show, in what order, and how wide (#588). */
  layout: ColumnLayout;
  /** The active sort, so the header can mark the column and its
   *  direction. `null` in the lists that have no sort of their own. */
  sort: { orderBy: string; direction: "asc" | "desc" } | null;
  onSort: (orderBy: string) => void;
  onPreviewColumn: (id: ColumnId, width: number) => void;
  onCommitColumn: (id: ColumnId) => void;
  /** Values of the chosen custom-tag columns, keyed by track id then
   *  tag key. Empty unless a `tag:` column is shown. */
  tagValues: Map<string, Record<string, string>>;
}

export function TrackTable({
  tracks,
  isLoading,
  view,
  t,
  onPlayTrack,
  currentTrackId,
  isPlaying,
  likedIds,
  onToggleLike,
  playlists,
  onAddToPlaylist,
  onRemoveFromPlaylist,
  onCreatePlaylist,
  onNavigateToAlbum,
  onNavigateToArtist,
  onContextMenuRow,
  onRowMenuKey,
  isSelected,
  selectionActive,
  onToggleSelect,
  onRowSelect,
  singleClickPlay,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  onDownloadRemote,
  onImportRemote,
  onEditRemoteTags,
  downloadingRemote,
  downloadedRemote,
  locale,
  layout: savedLayout,
  sort,
  onSort,
  onPreviewColumn,
  onCommitColumn,
  tagValues,
}: TrackTableProps) {
  "use no memo";
  const unknown = t("library.table.unknown");
  // Right-click on a server row. The local menu speaks `Track` — a rowid, a
  // file, a rating — which a server row has none of, so those rows were handed
  // `null` and the gesture did nothing at all. Offers the same three actions
  // as the hover icons, so both ways in agree.
  const remoteContextMenu = useRemoteTrackContextMenu({
    onDownload: onDownloadRemote,
    onImport: (target) => {
      // A server row's `id` is the server's identifier — the same string the
      // menu carries.
      const row = tracks.find((candidate) => candidate.id === target.remoteId);
      if (row) onImportRemote(row);
    },
    onEditTags: (target) => onEditRemoteTags(target.remoteId),
    downloading: downloadingRemote,
    downloaded: downloadedRemote,
  });
  const [openMenuTrackId, setOpenMenuTrackId] = useState<number | null>(null);
  // Per-track playlist membership snapshot, fetched the first time the
  // user opens the `+` popover for a given track. Entry stays cached for
  // the lifetime of the table so reopening the menu is instant. Optimistic
  // updates flip the set on toggle.
  const [trackMembership, setTrackMembership] = useState<
    Map<number, Set<number>>
  >(new Map());
  const [ratingOverrides, setRatingOverrides] = useState<
    Map<number, number | null>
  >(new Map());
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
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

  // Virtual scroll — only the visible rows are in the DOM.
  const ROW_HEIGHT = view === "list" ? 56 : 44;
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => ROW_HEIGHT,
    overscan: 15,
    scrollMargin,
  });

  useEffect(() => {
    if (openMenuTrackId == null) return;
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("[data-add-to-playlist-popover]")) return;
      if (target.closest("[data-add-to-playlist-trigger]")) return;
      setOpenMenuTrackId(null);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuTrackId(null);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [openMenuTrackId]);

  // The last track holds the row's actions. A server row puts three
  // buttons there (keep offline, import, correct tags) at 28px each, so
  // the 2.5rem it used to be overflowed into the heart beside it — it
  // already did with two, and a third made it plain.
  //
  // Built from the layout since #588, so a `grid-cols-[...]` utility
  // can no longer be the source of truth. The chrome keeps its fixed
  // tracks at both ends -- the index, the thumbnail the list view
  // shows, the heart, the actions -- because none of it carries a value
  // and none of it sorts.
  const leadingSpacers = view === "list" ? 2 : 1;
  // In rem, once, so the grid, its floor and the fitting below all read
  // the same sizes.
  const leadingRem = view === "list" ? [3, 2.75] : [3];
  const trailingRem = [2, 5.5];
  const leadingTracks = leadingRem.map((n) => `${n}rem`);
  const trailingTracks = trailingRem.map((n) => `${n}rem`);

  // Fit the columns to the room the table has (#669). The frame's parent
  // is measured, not the frame, because the frame's own `min-width` below
  // is derived from the columns this picks. Everything under here reads
  // `layout` — the fitted one — while the column picker and the saved
  // widths keep the full layout, so a column hidden for lack of room
  // returns unchanged when the right panel closes or the window grows.
  const frameRef = useRef<HTMLDivElement>(null);
  const [availableWidth, setAvailableWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    const container = frameRef.current?.parentElement;
    if (!container) return;
    const measure = () => setAvailableWidth(container.clientWidth);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(container);
    return () => ro.disconnect();
  }, []);
  const layout = useMemo(() => {
    if (availableWidth === null) return savedLayout;
    const rem =
      parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    const chromeRem = [...leadingRem, ...trailingRem].reduce(
      (a, b) => a + b,
      0,
    );
    return fitLayoutToWidth(
      savedLayout,
      availableWidth,
      (chromeRem + /* px-5 */ 2.5) * rem + /* border */ 2,
      leadingRem.length + trailingRem.length,
      /* gap-4 */ rem,
    );
    // `view` decides the rem arrays, which are rebuilt every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [availableWidth, savedLayout, view]);

  const gridCols = [
    ...leadingTracks,
    ...layout.order.map((id) => trackSizeFor(id, layout)),
    ...trailingTracks,
  ].join(" ");
  // The grid's own floor: every track at its minimum, the `gap-4`
  // between them, the row's `px-5` and the frame's 1px border. Put on the
  // frame, it makes a column list wider than the view (a narrow window,
  // the right panel open) widen the whole table and scroll the page
  // sideways. Without it the grid spilled out of the frame, and the
  // background, border and header stopped halfway along the rows.
  const trackCount =
    leadingTracks.length + layout.order.length + trailingTracks.length;
  const tableMinWidth = `calc(${[
    ...leadingTracks,
    ...layout.order.map((id) => `${trackMinWidthFor(id, layout)}px`),
    ...trailingTracks,
  ].join(" + ")} + ${trackCount - 1}rem + 2.5rem + 2px)`;

  // Formatters for the text a column shows, and for measuring it.
  const bodyRef = useRef<HTMLDivElement>(null);
  const textFor = useCallback(
    (id: ColumnId, row: LibraryTrackRow) =>
      cellText(id, row, {
        duration: formatDuration,
        bytes: (n) => formatBytes(n, locale),
        // Milliseconds, not seconds: `track.added_at` is written by
        // `now_millis()` on the Rust side, and `TrackPropertiesModal`
        // has always fed it to `Date` unscaled. Multiplying here put
        // the "Added" column tens of thousands of years out.
        date: (epochMs) =>
          new Date(epochMs).toLocaleDateString(locale, {
            year: "numeric",
            month: "short",
            day: "numeric",
          }),
        tag: (r, key) => tagValues.get(r.id)?.[key] ?? null,
        musicalKey: (raw) => formatMusicalKey(raw, t),
      }),
    [locale, tagValues, t],
  );

  /** Double-click on a resize handle: size the column to its content.
   *
   *  Measured through a canvas rather than off the DOM, because the
   *  rows are virtualised -- the ones outside the viewport have no
   *  computed layout, so a DOM measurement would size the column to
   *  whatever happens to be on screen. The header label is measured
   *  too, in its own font: a column fitted to short content otherwise
   *  shows a truncated title, which reads as the fit having failed. */
  const fitColumn = useCallback(
    (id: ColumnId) => {
      const spec = specFor(id);
      const tag = tagKeyOf(id);
      const label = tag !== null ? tag : t(`library.columns.${spec.labelKey}`);
      // This column's own cell, not simply the first one on screen.
      // The first is the title of the first row, which is a different
      // weight when that row is the one playing and a different variant
      // from any right-aligned column's `tabular-nums` -- so the
      // measurement would be taken in a font the column never uses.
      const cell =
        bodyRef.current?.querySelector(
          `[data-track-cell="${CSS.escape(id)}"]`,
        ) ?? null;
      const header =
        bodyRef.current
          ?.closest("[data-track-table]")
          ?.querySelector(`[data-track-header="${CSS.escape(id)}"]`) ?? null;
      // A bounded sample, not the whole list. Each value is a canvas
      // measurement and this runs synchronously on a double-click, so
      // a fifty-thousand-track library would freeze the window for the
      // length of it. The widest cell in two thousand rows is the
      // widest cell for any practical purpose, and `fitWidth` caps the
      // answer at `MAX_COLUMN_WIDTH` regardless.
      const FIT_SAMPLE = 2000;
      const width = fitWidth({
        values: tracks.slice(0, FIT_SAMPLE).map((row) => textFor(id, row)),
        headerLabel: label,
        cell: styleOf(cell),
        header: styleOf(header),
        // The cell's own gap, plus room for the sort caret the header
        // draws beside its label.
        padding: 28,
        min: spec.minWidth,
        max: MAX_COLUMN_WIDTH,
      });
      if (width === null) return;
      // A fit is a complete gesture on its own: preview so the header
      // shows it at once, commit so it survives.
      onPreviewColumn(id, width);
      onCommitColumn(id);
    },
    [onCommitColumn, onPreviewColumn, t, tracks, textFor],
  );

  return (
    <div
      ref={frameRef}
      data-track-table
      className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40"
      style={{ minWidth: tableMinWidth }}
    >
      {/* Sticky, not fixed. `overflow-hidden` had to go with it: it
          clips a sticky descendant against its own rounded corners, and
          the header then stops sticking at all. */}
      <TrackTableHeader
        layout={layout}
        gridCols={gridCols}
        sort={sort}
        onSort={onSort}
        onPreview={onPreviewColumn}
        onCommit={onCommitColumn}
        onFit={fitColumn}
        leadingSpacers={leadingSpacers}
        t={t}
      />

      {/* Virtualized body */}
      <div
        data-track-body
        ref={(node) => {
          parentRef.current = node;
          bodyRef.current = node;
        }}
        className={isLoading ? "opacity-50" : ""}
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((virtualRow) => {
          const index = virtualRow.index;
          const track = tracks[index];
          // A server track has no local rowid, and none of the gestures below
          // can accept one: it is in no local playlist, its rating lives in a
          // file that is not here, and the like list keys on `track.id`.
          const localId = track.source === "remote" ? null : Number(track.id);
          const local = localId !== null;
          // The row the user can act on as a local track, for the handlers
          // that still speak `Track`. Converted once here — the identifiers
          // are text on the wire, and a cast would leave them text.
          const asTrack = local ? toLocalTrack(track) : null;
          const isCurrent = localId !== null && localId === currentTrackId;
          const isMenuOpen = localId !== null && openMenuTrackId === localId;
          const isRowSelected = localId !== null && isSelected(localId);
          return (
            // Row can't be a <button> because it contains action buttons
            // (heart, more-options); nested buttons are invalid HTML.
            // Keyboard activation still works via tabIndex + onKeyDown.
            <div
              key={`${track.source}:${track.id}`}
              tabIndex={0}
              role="button"
              onClick={(e) => {
                if (asTrack) {
                  onRowSelect(asTrack, e);
                  return;
                }
                // A server row has nothing to select — selection is keyed on
                // local rowids. With single-click play on, every other row
                // responds and this one would not, which reads as a dead row
                // rather than as an unselectable one. Modifier clicks stay
                // inert: they are selection gestures, and there is no
                // selection here to extend.
                if (e.detail === 1 && isPlayClick(e, singleClickPlay)) {
                  onPlayTrack(index);
                }
              }}
              // With single-click play the first click has already started
              // the track; playing again here would restart it twice more.
              onDoubleClick={() => {
                if (!singleClickPlay) onPlayTrack(index);
              }}
              onKeyDown={(e) => {
                // Only play when the row itself is focused. Without
                // this guard, hitting Enter/Space on a nested button
                // (like, +, ArtistLink, AlbumLink) bubbles up here and
                // double-fires playback alongside the button's own
                // action.
                if (e.target !== e.currentTarget) return;
                if (asTrack) {
                  if (onRowMenuKey(e, asTrack)) return;
                } else if (
                  remoteContextMenu.openFromKeyboard(e, {
                    remoteId: track.id,
                    title: track.title,
                  })
                ) {
                  return;
                }
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onPlayTrack(index);
                }
              }}
              onKeyUp={(e) => {
                // Belt-and-suspenders: a few browsers fire spacebar
                // scroll on keyup for non-button elements even when
                // keydown was cancelled. Suppress it here too.
                if (e.target !== e.currentTarget) return;
                if (e.key === " ") e.preventDefault();
              }}
              onContextMenu={(e) => {
                if (asTrack) {
                  onContextMenuRow(e, asTrack);
                } else {
                  remoteContextMenu.open(e, {
                    remoteId: track.id,
                    title: track.title,
                  });
                }
              }}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                height: `${virtualRow.size}px`,
                transform: `translateY(${virtualRow.start - scrollMargin}px)`,
                // Hoist the row that owns the open "+" popover above its
                // sibling rows so the popover isn't painted under (or
                // click-blocked by) the rows rendered after it in DOM
                // order. Every row is `position: absolute` without a
                // z-index, so the popover's own `z-50` can't escape its
                // row's stacking context — bumping the row itself does.
                zIndex: isMenuOpen ? 20 : undefined,
                // Inline, like the header's. Since #588 `gridCols` is a
                // CSS track list built from the layout, not a Tailwind
                // class -- interpolating it into `className` produced a
                // garbage class name and every row silently fell back
                // to a single-column grid.
                gridTemplateColumns: gridCols,
              }}
              data-track-row
              data-track-last={index === tracks.length - 1 ? "" : undefined}
              className={`group grid gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
                isRowSelected
                  ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
                  : isCurrent
                    ? "bg-emerald-50 dark:bg-emerald-900/20"
                    : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
              }`}
            >
              <span
                className={`text-right text-sm tabular-nums flex items-center justify-end ${
                  isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
                }`}
              >
                {localId !== null ? (
                  <RowSelectCheckbox
                    checked={isRowSelected}
                    selectionActive={selectionActive}
                    onToggle={() => onToggleSelect(localId)}
                    title={track.title}
                  >
                    {isCurrent ? (
                      <PlayingIndicator isPlaying={isPlaying} />
                    ) : (
                      index + 1
                    )}
                  </RowSelectCheckbox>
                ) : isCurrent ? (
                  <PlayingIndicator isPlaying={isPlaying} />
                ) : (
                  index + 1
                )}
              </span>
              {view === "list" &&
                (local ? (
                  <Artwork
                    path={track.artwork_path}
                    size="1x"
                    className="w-10 h-10"
                    iconSize={18}
                    alt={track.album_title ?? track.title}
                    rounded="md"
                  />
                ) : (
                  <RemoteArtwork
                    hash={track.artwork_hash}
                    className="w-10 h-10 rounded-md"
                    iconSize={18}
                  />
                ))}
              {/* One cell per chosen column, in the chosen order
                  (#588). The five that used to be hard-coded here keep
                  their exact markup -- the title still carries the
                  source badge and the Hi-Res pill, the artist and album
                  are still links, the rating is still writable -- so
                  making the set configurable changed what is rendered
                  and not how any of it behaves. */}
              {layout.order.map((id) => {
                const spec = specFor(id);
                const align =
                  spec.align === "right"
                    ? "text-right"
                    : spec.align === "center"
                      ? "text-center"
                      : "";
                if (id === "title") {
                  return (
                    <span
                      key={id}
                      data-track-cell={id}
                      className={`text-sm truncate flex items-center gap-2 ${
                        isCurrent
                          ? "text-emerald-600 dark:text-emerald-400 font-semibold"
                          : "text-zinc-800 dark:text-zinc-200"
                      }`}
                    >
                      <span className="truncate">{track.title}</span>
                      {/* One list, and every row says where it comes from. */}
                      {!local && (
                        <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium bg-zinc-200 text-zinc-600 dark:bg-zinc-700 dark:text-zinc-300">
                          {t("library.source.remote")}
                        </span>
                      )}
                      <HiResBadge
                        bitDepth={track.bit_depth}
                        sampleRate={track.sample_rate}
                        codec={track.codec}
                        variant="inline"
                      />
                    </span>
                  );
                }
                if (id === "artist") {
                  return (
                    <ArtistLink
                      key={id}
                      name={track.artist_name}
                      artistIds={local ? track.artist_ids : null}
                      onNavigate={onNavigateToArtist}
                      onNavigateRemote={
                        local || !track.artist_id
                          ? undefined
                          : () =>
                              onNavigateToRemoteArtist(
                                track.artist_id as string,
                              )
                      }
                      fallback={unknown}
                      className="text-sm text-zinc-500 truncate"
                      cellId={id}
                    />
                  );
                }
                if (id === "album") {
                  return (
                    <AlbumLink
                      key={id}
                      title={track.album_title}
                      albumId={
                        local && track.album_id ? Number(track.album_id) : null
                      }
                      onNavigate={onNavigateToAlbum}
                      onNavigateRemote={
                        local || !track.album_id
                          ? undefined
                          : () =>
                              onNavigateToRemoteAlbum(track.album_id as string)
                      }
                      fallback={unknown}
                      className="text-sm text-zinc-500 truncate"
                      cellId={id}
                    />
                  );
                }
                if (id === "rating") {
                  return (
                    <div
                      key={id}
                      data-track-cell={id}
                      className="flex items-center"
                      onDoubleClick={(e) => e.stopPropagation()}
                    >
                      {/* Rating writes a POPM frame into the file. A
                          server track has no file here, so the control
                          is absent rather than inert -- five hollow
                          stars that do nothing read as "unrated". */}
                      {localId !== null && (
                        <StarRating
                          value={
                            ratingOverrides.has(localId)
                              ? (ratingOverrides.get(localId) ?? null)
                              : track.rating
                          }
                          size="sm"
                          onChange={(rating) => {
                            setRatingOverrides((prev) => {
                              const next = new Map(prev);
                              next.set(localId, rating);
                              return next;
                            });
                            setTrackRating(localId, rating).catch((err) => {
                              console.error(
                                "[LibraryView] set rating failed",
                                err,
                              );
                              setRatingOverrides((prev) => {
                                const next = new Map(prev);
                                next.delete(localId);
                                return next;
                              });
                            });
                          }}
                        />
                      )}
                    </div>
                  );
                }
                // Everything else is text, and goes through the same
                // formatter the column measurement uses -- so a column
                // fitted to its content is fitted to what it shows.
                const text = textFor(id, track);
                return (
                  <span
                    key={id}
                    data-track-cell={id}
                    title={text || undefined}
                    className={`text-sm truncate text-zinc-400 ${align} ${
                      spec.align === "right" ? "tabular-nums" : ""
                    }`}
                  >
                    {text}
                  </span>
                );
              })}
              <div className="flex justify-center">
                {localId !== null && (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleLike(localId);
                    }}
                    aria-label={
                      likedIds.has(localId)
                        ? t("liked.unlike")
                        : t("liked.like")
                    }
                    className={`p-1 rounded-full transition-colors ${
                      likedIds.has(localId)
                        ? "text-pink-500"
                        : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
                    }`}
                  >
                    <Heart
                      size={14}
                      className={likedIds.has(localId) ? "fill-current" : ""}
                    />
                  </button>
                )}
              </div>
              <div className="relative flex justify-center">
                {/* A server row gets the gestures that are meaningful on it
                    instead: keep the bytes for offline playback, copy them
                    into a scanned folder where they become a track of this
                    library, or correct the metadata the server holds. Plain
                    buttons rather than a menu — there are few enough to show,
                    and a menu would hide every one behind a click. */}
                {!local && (
                  // Same guard the rating column carries: without it a
                  // double-click on one of these buttons bubbles up to the
                  // row and starts playback behind the dialog that just
                  // opened.
                  <div
                    className="flex items-center gap-0.5"
                    onDoubleClick={(e) => e.stopPropagation()}
                  >
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDownloadRemote(String(track.id));
                      }}
                      disabled={
                        downloadingRemote.has(String(track.id)) ||
                        downloadedRemote.has(String(track.id))
                      }
                      aria-label={
                        downloadedRemote.has(String(track.id))
                          ? t("remote.download.kept")
                          : t("remote.download.keep")
                      }
                      title={
                        downloadedRemote.has(String(track.id))
                          ? t("remote.download.kept")
                          : t("remote.download.keep")
                      }
                      className={`p-1.5 rounded-full transition-all ${
                        downloadedRemote.has(String(track.id))
                          ? "opacity-100 text-emerald-500"
                          : downloadingRemote.has(String(track.id))
                            ? "opacity-100 text-zinc-400 animate-pulse"
                            : "opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                      }`}
                    >
                      {downloadedRemote.has(String(track.id)) ? (
                        <Check size={16} />
                      ) : (
                        <Download size={16} />
                      )}
                    </button>
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        onImportRemote(track);
                      }}
                      aria-label={t("remote.import.action")}
                      title={t("remote.import.action")}
                      className="p-1.5 rounded-full transition-all opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                    >
                      <FolderDown size={16} />
                    </button>
                    {/* Corrects what the SERVER holds, beside the track and
                        surviving its rescans. No file is rewritten, which is
                        what makes it possible on a track that lives on
                        somebody else's disk — and why the local Properties
                        dialog, which is a file inspector, is not what opens
                        here. */}
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        onEditRemoteTags(String(track.id));
                      }}
                      aria-label={t("remote.tags.action")}
                      title={t("remote.tags.action")}
                      className="p-1.5 rounded-full transition-all opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                    >
                      <Pencil size={16} />
                    </button>
                  </div>
                )}
                {/* A local playlist holds local tracks; the picker cannot
                    accept a server one. */}
                {localId !== null && (
                  <>
                    <button
                      type="button"
                      data-add-to-playlist-trigger
                      onClick={(e) => {
                        e.stopPropagation();
                        const opening = !isMenuOpen;
                        setOpenMenuTrackId(opening ? localId : null);
                        // Lazy-fetch membership the first time this track's
                        // popover is opened. Subsequent opens reuse the cached
                        // set (kept in sync via optimistic updates on toggle).
                        if (opening && !trackMembership.has(localId)) {
                          listPlaylistsContainingTrack(localId)
                            .then((ids) => {
                              setTrackMembership((prev) => {
                                const next = new Map(prev);
                                next.set(localId, new Set(ids));
                                return next;
                              });
                            })
                            .catch((err) => {
                              console.error(
                                "[LibraryView] load membership failed",
                                err,
                              );
                            });
                        }
                      }}
                      aria-label={t("trackActions.addToPlaylist")}
                      aria-haspopup="menu"
                      aria-expanded={isMenuOpen}
                      className={`p-1.5 rounded-full transition-all ${
                        isMenuOpen
                          ? "opacity-100 bg-zinc-100 dark:bg-zinc-700 text-zinc-800 dark:text-white"
                          : "opacity-0 reveal:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                      }`}
                    >
                      <Plus size={16} />
                    </button>
                    {isMenuOpen && (
                      <AddToPlaylistPopover
                        playlists={playlists}
                        trackId={localId}
                        memberPlaylistIds={trackMembership.get(localId)}
                        onPick={(playlistId) => {
                          const members = trackMembership.get(localId);
                          const isMember = members?.has(playlistId) ?? false;
                          // Optimistic membership flip — the underlying mutations
                          // are idempotent on the backend, so a failed RPC just
                          // means the visual state will drift until the next
                          // popover open, which is the worst-case loss for a
                          // single click.
                          setTrackMembership((prev) => {
                            const next = new Map(prev);
                            const set = new Set(next.get(localId) ?? []);
                            if (isMember) set.delete(playlistId);
                            else set.add(playlistId);
                            next.set(localId, set);
                            return next;
                          });
                          if (isMember) {
                            void onRemoveFromPlaylist(playlistId, localId);
                          } else {
                            void onAddToPlaylist(playlistId, localId);
                          }
                          setOpenMenuTrackId(null);
                        }}
                        onCreate={() => {
                          setOpenMenuTrackId(null);
                          onCreatePlaylist(localId);
                        }}
                        t={t}
                      />
                    )}
                  </>
                )}
              </div>
            </div>
          );
        })}
      </div>
      {remoteContextMenu.render()}
    </div>
  );
}
