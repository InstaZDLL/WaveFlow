import { type LibraryAlbumRow } from "../../../lib/tauri/browse";
import { type Translator } from "./libraryTypes";
import { type Playlist } from "../../../lib/tauri/playlist";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useAlbumArtistContextMenu } from "../../../hooks/useAlbumArtistContextMenu";
import { usePageScroll } from "../../../hooks/usePageScroll";
import { useVirtualizer } from "@tanstack/react-virtual";
import { RemoteArtwork } from "../../common/RemoteArtwork";
import { Artwork } from "../../common/Artwork";
import { HiResBadge } from "../../common/HiResBadge";
import { Plus } from "lucide-react";
import { AddToPlaylistPopover } from "./AddToPlaylistPopover";

interface AlbumGridProps {
  albums: LibraryAlbumRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, albumId: number) => void;
  onCreatePlaylist: (albumId: number) => void;
  onAlbumClick: (albumId: number) => void;
  /** A server album has no local rowid and none of the gestures below —
   *  no playlist, no cover picker — so it gets its own click path. */
  onRemoteAlbumClick: (remoteAlbumId: string) => void;
  onChangeCover: (albumId: number) => void;
}

export function AlbumGrid({
  albums,
  isLoading,
  t,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onAlbumClick,
  onRemoteAlbumClick,
  onChangeCover,
}: AlbumGridProps) {
  "use no memo";
  const unknown = t("library.table.unknown");
  const [openMenuAlbumId, setOpenMenuAlbumId] = useState<number | null>(null);
  // Map album.id → the `+` button DOM node. The popover uses the live
  // node to compute its portal position via `getBoundingClientRect`,
  // sidestepping every ancestor stacking context.
  const triggerRefs = useRef<Map<number, HTMLButtonElement>>(new Map());
  // Right-click on a local album: change its cover, pin it.
  const albumMenu = useAlbumArtistContextMenu({ onChangeCover });

  // Virtual-grid plumbing — without this a 800-album library mounts 800
  // <Artwork> components on every tab switch, blowing the main thread
  // for ~1 s before the first paint.
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(1);
  const [tileWidth, setTileWidth] = useState(180);
  const [scrollMargin, setScrollMargin] = useState(0);
  // WebKitGTK can re-rasterize transformed rows on a descendant hover and
  // round their fractional offsets differently, making every cover jump
  // together (#817). Absolute top positioning avoids that compositor path.
  const positionRowsWithTop = document.documentElement.hasAttribute(
    "data-webkitgtk-safe",
  );

  // Match the original Tailwind grid: `auto-fill,minmax(180px,1fr)` + gap-5.
  const MIN_TILE = 180;
  const GAP = 20;
  // Tile = aspect-square cover (width = column width) + ~70 px of text
  // beneath it (title + artist + meta + the space-y-2 separator).
  const tileHeight = tileWidth + 70;

  useLayoutEffect(() => {
    const el = parentRef.current;
    if (!el) return;
    const recompute = () => {
      const width = el.getBoundingClientRect().width;
      if (width === 0) return;
      const n = Math.max(1, Math.floor((width + GAP) / (MIN_TILE + GAP)));
      const actual = (width - (n - 1) * GAP) / n;
      setColCount(n);
      setTileWidth(actual);
    };
    recompute();
    const ro = new ResizeObserver(recompute);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Mirror TrackTable's scrollMargin trick so the virtual row offsets
  // line up with the actual position of this grid inside the page
  // scroller.
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
  }, [pageScrollRef, albums.length]);

  const rowCount = Math.ceil(albums.length / colCount);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => tileHeight + GAP,
    overscan: 2,
    scrollMargin,
  });

  useEffect(() => {
    if (openMenuAlbumId == null) return;
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("[data-add-to-playlist-popover]")) return;
      if (target.closest("[data-add-to-playlist-trigger]")) return;
      setOpenMenuAlbumId(null);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuAlbumId(null);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [openMenuAlbumId]);

  const renderAlbumCard = (album: LibraryAlbumRow) => {
    // A server album carries a UUID, not a rowid, and none of the local
    // gestures apply to it: it is in no local playlist, and its cover lives
    // on the server. So everything keyed on a numeric id is computed only
    // for the local half rather than coerced for both.
    const remote = album.source === "remote";
    const localId = remote ? null : Number(album.id);
    const isMenuOpen = localId !== null && openMenuAlbumId === localId;
    const open = () =>
      localId !== null ? onAlbumClick(localId) : onRemoteAlbumClick(album.id);
    return (
      <div
        key={`${album.source}:${album.id}`}
        {...(localId !== null
          ? albumMenu.bind({ kind: "album", id: localId })
          : {})}
        className="group flex flex-col space-y-2 cursor-pointer relative"
      >
        <div className="relative">
          {remote ? (
            <RemoteArtwork
              hash={album.artwork_hash}
              className="w-full aspect-square rounded-2xl shadow-sm group-hover:shadow-md transition-shadow"
              iconSize={44}
            />
          ) : (
            <Artwork
              path={album.artwork_path}
              path1x={album.artwork_path_1x}
              path2x={album.artwork_path_2x}
              // Album grid tile renders ~150-200 px wide; the 128 px
              // 2x thumbnail upscales soft on a HiDPI display. Source
              // originals are 600-1500 px square — small enough to
              // decode instantly and crisp at any tile size.
              size="full"
              alt={album.title}
              className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
              iconSize={44}
              rounded="2xl"
            />
          )}
          <HiResBadge
            bitDepth={album.max_bit_depth}
            sampleRate={album.max_sample_rate}
          />
          {/* The chip is the whole point of unifying the navigation: one
              list, and every row says where it comes from. Local rows carry
              none — the device is the unmarked case. */}
          {remote && (
            <span className="absolute top-2 left-2 px-1.5 py-0.5 rounded-md text-[10px] font-medium bg-black/60 text-white backdrop-blur-sm">
              {t("library.source.remote")}
            </span>
          )}
          {/* A real button rather than a role on the card. `role="button"`
              makes its descendants presentational, which would have hidden the
              "+" below from assistive technology — one gap traded for another.
              Sized to the cover and declared before the "+" so that one paints
              and clicks on top of it. */}
          <button
            type="button"
            data-artwork-hit-target
            onClick={open}
            aria-label={t("library.open", { name: album.title })}
            className="absolute inset-0 rounded-2xl focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500"
          />
          {/* Local playlists hold local tracks. Offering the gesture on a
              server album would open a picker that cannot accept it. */}
          {localId !== null && (
            <button
              type="button"
              data-add-to-playlist-trigger
              ref={(el) => {
                if (el) triggerRefs.current.set(localId, el);
                else triggerRefs.current.delete(localId);
              }}
              onClick={(e) => {
                e.stopPropagation();
                setOpenMenuAlbumId(isMenuOpen ? null : localId);
              }}
              aria-label={t("trackActions.addToPlaylist")}
              // `reveal:`, not `group-hover:`: the hover-only form left the
              // button invisible exactly when the keyboard reached it.
              className={`absolute bottom-2 right-2 p-1.5 rounded-full shadow-sm transition-all focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500 ${
                isMenuOpen
                  ? "opacity-100 bg-emerald-500 text-white"
                  : "opacity-0 reveal:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
              }`}
            >
              <Plus size={16} />
            </button>
          )}
        </div>
        {/* Mouse-only: the overlay button above is the keyboard target, so
            duplicating it here would put two stops on one card. */}
        <div className="px-1" onClick={open}>
          <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
            {album.title}
          </div>
          <div className="text-xs text-zinc-500 truncate">
            {album.artist_name ?? unknown}
          </div>
          <div className="text-[11px] text-zinc-400 mt-1">
            {t("library.albumGrid.trackCount", {
              count: album.track_count,
            })}
            {album.year ? ` · ${album.year}` : ""}
          </div>
        </div>
        {isMenuOpen && localId !== null && (
          <AddToPlaylistPopover
            playlists={playlists}
            trackId={localId}
            anchorEl={triggerRefs.current.get(localId) ?? null}
            onPick={(playlistId) => {
              onAddToPlaylist(playlistId, localId);
              setOpenMenuAlbumId(null);
            }}
            onCreate={() => {
              setOpenMenuAlbumId(null);
              onCreatePlaylist(localId);
            }}
            t={t}
          />
        )}
      </div>
    );
  };

  return (
    <>
      <div
        ref={parentRef}
        className={isLoading ? "opacity-50" : ""}
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((row) => {
          const startIdx = row.index * colCount;
          const rowItems = albums.slice(startIdx, startIdx + colCount);
          // Hoist the row that owns the open card popover above the rows
          // rendered after it in DOM order. Same stacking-context trap as
          // TrackTable: every virtualized row is `position: absolute`, so
          // a `z-50` inside one card can't escape its row.
          const rowHasOpenMenu = rowItems.some(
            (album) =>
              album.source === "local" && Number(album.id) === openMenuAlbumId,
          );
          return (
            <div
              key={row.key}
              style={{
                position: "absolute",
                top: positionRowsWithTop ? row.start - scrollMargin : 0,
                left: 0,
                width: "100%",
                transform: positionRowsWithTop
                  ? undefined
                  : "translateY(" + (row.start - scrollMargin) + "px)",
                display: "grid",
                gridTemplateColumns: `repeat(${colCount}, minmax(0, 1fr))`,
                gap: `${GAP}px`,
                paddingBottom: `${GAP}px`,
                zIndex: rowHasOpenMenu ? 20 : undefined,
              }}
            >
              {rowItems.map((album) => renderAlbumCard(album))}
            </div>
          );
        })}
      </div>
      {albumMenu.render()}
    </>
  );
}
