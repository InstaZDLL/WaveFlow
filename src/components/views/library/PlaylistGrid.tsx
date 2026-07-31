import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ListMusic } from "lucide-react";

import { usePageScroll } from "../../../hooks/usePageScroll";
import { PlaylistIcon } from "../../../lib/PlaylistIcon";
import { resolvePlaylistColor } from "../../../lib/playlistVisuals";
import { resolveRemoteImage } from "../../../lib/tauri/artwork";
import { formatDuration } from "../../../lib/tauri/track";
import type { SortState } from "../../../hooks/useSortMemory";
import type { Playlist } from "../../../lib/tauri/playlist";
import { EmptyState } from "../../common/EmptyState";

interface PlaylistGridProps {
  /** User playlists only — smart ones live in Home's "Made for you". */
  playlists: Playlist[];
  /** Same `{ orderBy, direction }` shape the other library tabs use, so
   *  this tab gets `SortDropdown` + persisted sort for free. `custom` is
   *  the sidebar's own manual order (`playlist.position`). */
  sort: SortState;
  onOpen: (playlistId: number) => void;
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
export function PlaylistGrid({ playlists, sort, onOpen }: PlaylistGridProps) {
  const { t, i18n } = useTranslation();

  const sorted = useMemo(() => {
    // Locale-aware compare: a byte comparison sorts "Été" after "Zoo".
    const collator = new Intl.Collator(i18n.language, {
      sensitivity: "base",
    });
    const ascending = (a: Playlist, b: Playlist): number => {
      switch (sort.orderBy) {
        case "name":
          return collator.compare(a.name, b.name);
        case "tracks":
          return a.track_count - b.track_count;
        case "duration":
          return a.total_duration_ms - b.total_duration_ms;
        case "updated":
          return a.updated_at - b.updated_at;
        case "custom":
        default:
          return a.position - b.position;
      }
    };
    const factor = sort.direction === "desc" ? -1 : 1;
    // Sorting a copy: `playlists` comes straight from the context and is
    // shared with the sidebar, which renders it in `position` order.
    return [...playlists].sort((a, b) => factor * ascending(a, b));
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
        description={t("library.playlistsGrid.emptyHint")}
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
                  key={playlist.id}
                  playlist={playlist}
                  onOpen={onOpen}
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
}: {
  playlist: Playlist;
  onOpen: (playlistId: number) => void;
}) {
  const { t } = useTranslation();
  const color = resolvePlaylistColor(playlist.color_id);
  const coverUrl = resolveRemoteImage(playlist.cover_path, null);

  return (
    // A button rather than a div with onClick: this is the whole point of
    // the tile, so it should be tabbable and Enter/Space-activatable
    // without bolting a keydown handler onto a non-interactive element.
    <button
      type="button"
      onClick={() => onOpen(playlist.id)}
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
          <PlaylistIcon iconId={playlist.icon_id} size={44} />
        </div>
      )}
      <div className="min-w-0">
        <div className="text-sm font-medium text-zinc-900 dark:text-white truncate">
          {playlist.name}
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
