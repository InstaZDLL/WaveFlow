import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ListMusic } from "lucide-react";

import { usePageScroll } from "../../../hooks/usePageScroll";
import { PlaylistIcon } from "../../../lib/PlaylistIcon";
import {
  colorForPlaylistId,
  resolvePlaylistColor,
} from "../../../lib/playlistVisuals";
import { resolveRemoteImage } from "../../../lib/tauri/artwork";
import { formatDuration } from "../../../lib/tauri/track";
import type { SortState } from "../../../hooks/useSortMemory";
import type { LibrarySource } from "../../../lib/tauri/browse";
import { EmptyState } from "../../common/EmptyState";

/**
 * A playlist of the library, from either source.
 *
 * Built in the view rather than fetched: unlike the other three tabs, the
 * playlist grid already sorted in the browser, so there is no SQL ordering to
 * unify and nothing to gain from a compound select. The two shapes are merged
 * where they are read.
 */
export interface LibraryPlaylistRow {
  source: LibrarySource;
  /** Local rowid as text, or the server's playlist identifier. */
  id: string;
  name: string;
  track_count: number;
  total_duration_ms: number;
  /** Local only: the server's summary carries no modification time. */
  updated_at: number | null;
  /** Local only: the sidebar's manual order. */
  position: number | null;
  color_id: string;
  icon_id: string | null;
  cover_path: string | null;
  /** Remote only: created here and not yet sent to the server. */
  pending_creation: boolean;
}

interface PlaylistGridProps {
  /** User playlists only — smart ones live in Home's "Made for you". */
  playlists: LibraryPlaylistRow[];
  /** Same `{ orderBy, direction }` shape the other library tabs use, so
   *  this tab gets `SortDropdown` + persisted sort for free. `custom` is
   *  the sidebar's own manual order (`playlist.position`). */
  sort: SortState;
  onOpen: (playlistId: number) => void;
  /** A server playlist has no local rowid and opens its own view. */
  onOpenRemote: (remotePlaylistId: string) => void;
  /** Whether the list is empty because the source filter narrowed it, rather
   *  than because there is nothing to show. Two different messages. */
  sourceFiltered: boolean;
}

/**
 * Grid of the user's own playlists, as a tile per playlist rather than
 * the sidebar's one-line-at-a-time list (issue #461: someone who builds
 * their own playlists thinks of them as albums and wants to see them all
 * at once).
 *
 * Row-virtualized against the page scroller, mirroring the albums grid:
 * a profile with a few hundred playlists would otherwise mount that many
 * cover images on every tab switch. Consumes `usePageScroll` rather than
 * nesting its own `overflow-y-auto`, which is what keeps the app on a
 * single Spotify-style scrollbar.
 */
export function PlaylistGrid({
  playlists,
  sort,
  onOpen,
  onOpenRemote,
  sourceFiltered,
}: PlaylistGridProps) {
  const { t, i18n } = useTranslation();

  const sorted = useMemo(() => {
    // Locale-aware compare: a byte comparison sorts "Été" after "Zoo".
    const collator = new Intl.Collator(i18n.language, {
      sensitivity: "base",
    });
    // Two of the sort keys exist on the local half only: the server's summary
    // carries no modification time, and manual order is the sidebar's, which a
    // server playlist is not in. Rather than reading a missing key as zero —
    // which would file every remote playlist as the oldest, or first — they
    // fall to the end of the list and settle among themselves by name. Same
    // reading as the unratable tracks: absent is not smallest.
    const factor = sort.direction === "desc" ? -1 : 1;
    // The direction applies to the values, never to the missing-last rule.
    // Multiplying the whole comparison by the factor would invert the
    // sentinels too, and "last" would become "first" the moment someone
    // reversed the sort — which is the same defect the track list had when
    // its NULL ratings were left to the ORDER BY direction.
    const missingLast = (
      a: number | null,
      b: number | null,
      byName: number,
    ): number => {
      if (a == null && b == null) return factor * byName;
      if (a == null) return 1;
      if (b == null) return -1;
      return factor * (a - b);
    };
    const compare = (a: LibraryPlaylistRow, b: LibraryPlaylistRow): number => {
      const byName = collator.compare(a.name, b.name);
      switch (sort.orderBy) {
        case "name":
          return factor * byName;
        case "tracks":
          return factor * (a.track_count - b.track_count);
        case "duration":
          return factor * (a.total_duration_ms - b.total_duration_ms);
        case "updated":
          return missingLast(a.updated_at, b.updated_at, byName);
        case "custom":
        default:
          return missingLast(a.position, b.position, byName);
      }
    };
    // Sorting a copy: `playlists` comes straight from the context and is
    // shared with the sidebar, which renders it in `position` order.
    return [...playlists].sort(compare);
  }, [playlists, sort.orderBy, sort.direction, i18n.language]);

  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(1);
  const [tileWidth, setTileWidth] = useState(180);
  const [scrollMargin, setScrollMargin] = useState(0);

  // Same tile metrics as the albums grid so the two read as one system.
  const MIN_TILE = 180;
  const GAP = 20;
  // Square cover + ~64 px of text (name + "N tracks · duration").
  const tileHeight = tileWidth + 64;

  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const recompute = () => {
      const width = el.getBoundingClientRect().width;
      if (width === 0) return;
      const n = Math.max(1, Math.floor((width + GAP) / (MIN_TILE + GAP)));
      setColCount(n);
      setTileWidth((width - (n - 1) * GAP) / n);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Offset the virtual rows by this grid's position inside the page
  // scroller — without it the rows are placed as if the grid started at
  // the top of the scroller, and the tiles drift as the page scrolls.
  useLayoutEffect(() => {
    const parent = parentRef.current;
    const scroller = pageScrollRef?.current;
    if (!parent || !scroller) return;
    const recompute = () => {
      const pr = parent.getBoundingClientRect();
      const sr = scroller.getBoundingClientRect();
      setScrollMargin(pr.top - sr.top + scroller.scrollTop);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(parent);
    ro.observe(scroller);
    return () => ro.disconnect();
  }, [pageScrollRef, sorted.length]);

  const rowCount = Math.ceil(sorted.length / colCount);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => tileHeight + GAP,
    overscan: 2,
    scrollMargin,
  });

  // Re-measure when the column count changes: the estimate is derived
  // from tile width, so a window resize otherwise leaves stale offsets.
  useEffect(() => {
    virtualizer.measure();
  }, [virtualizer, tileHeight]);

  if (playlists.length === 0) {
    return (
      <EmptyState
        icon={<ListMusic size={32} />}
        title={t("library.playlistsGrid.emptyTitle")}
        // A narrowed source is a different emptiness from "you have not made
        // any playlists yet", and telling the user to create one would not
        // help: the half they are looking at is simply not this one.
        description={t(
          sourceFiltered
            ? "library.empty.sourceFiltered.description"
            : "library.playlistsGrid.emptyHint",
        )}
      />
    );
  }

  return (
    <div ref={parentRef}>
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((row) => {
          const startIdx = row.index * colCount;
          const rowItems = sorted.slice(startIdx, startIdx + colCount);
          return (
            <div
              key={row.key}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${row.start - scrollMargin}px)`,
                display: "grid",
                gridTemplateColumns: `repeat(${colCount}, minmax(0, 1fr))`,
                gap: `${GAP}px`,
                paddingBottom: `${GAP}px`,
              }}
            >
              {rowItems.map((playlist) => (
                <PlaylistCard
                  key={`${playlist.source}:${playlist.id}`}
                  playlist={playlist}
                  onOpen={onOpen}
                  onOpenRemote={onOpenRemote}
                />
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function PlaylistCard({
  playlist,
  onOpen,
  onOpenRemote,
}: {
  playlist: LibraryPlaylistRow;
  onOpen: (playlistId: number) => void;
  onOpenRemote: (remotePlaylistId: string) => void;
}) {
  const { t } = useTranslation();
  const remote = playlist.source === "remote";
  // A server playlist carries no `color_id`; the colour is derived from its
  // identifier so it is stable and not the same swatch for all of them.
  const color = remote
    ? colorForPlaylistId(playlist.id)
    : resolvePlaylistColor(playlist.color_id);
  const coverUrl = resolveRemoteImage(playlist.cover_path, null);

  return (
    // A button rather than a div with onClick: this is the whole point of
    // the tile, so it should be tabbable and Enter/Space-activatable
    // without bolting a keydown handler onto a non-interactive element.
    <button
      type="button"
      onClick={() =>
        remote ? onOpenRemote(playlist.id) : onOpen(Number(playlist.id))
      }
      className="group flex flex-col space-y-2 text-left cursor-pointer rounded-2xl focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
    >
      {coverUrl ? (
        <img
          src={coverUrl}
          alt=""
          loading="lazy"
          className="w-full aspect-square rounded-2xl object-cover shadow-sm group-hover:shadow-md transition-shadow"
        />
      ) : (
        // No cover: the icon + gradient tile the sidebar already uses for
        // this playlist, so the same playlist looks the same everywhere.
        <div
          className={`w-full aspect-square rounded-2xl flex items-center justify-center shadow-sm group-hover:shadow-md transition-shadow ${color.tileBg} ${color.tileText}`}
        >
          {/* A server playlist has no icon of its own; the default one keeps
              the tile from being an empty colour block. */}
          <PlaylistIcon iconId={playlist.icon_id ?? "music"} size={44} />
        </div>
      )}
      <div className="min-w-0">
        <div className="text-sm font-medium text-zinc-900 dark:text-white truncate flex items-center gap-1.5">
          <span className="truncate">{playlist.name}</span>
          {/* One list, and every tile says where it comes from. */}
          {remote && (
            <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-medium bg-zinc-200 text-zinc-600 dark:bg-zinc-700 dark:text-zinc-300">
              {t("library.source.remote")}
            </span>
          )}
        </div>
        <div className="text-xs text-zinc-500 dark:text-zinc-400 truncate">
          {t("library.playlistsGrid.meta", {
            count: playlist.track_count,
            duration: formatDuration(playlist.total_duration_ms),
          })}
        </div>
      </div>
    </button>
  );
}
