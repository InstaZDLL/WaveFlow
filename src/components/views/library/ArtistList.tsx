import { type LibraryArtistRow } from "../../../lib/tauri/browse";
import { useRemoteArtworkSrc } from "../../../hooks/useRemoteArtworkSrc";
import { resolveArtwork } from "../../../lib/tauri/artwork";
import { FadeInImage } from "../../common/FadeInImage";
import { type Translator } from "./libraryTypes";
import { type Playlist } from "../../../lib/tauri/playlist";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useAlbumArtistContextMenu } from "../../../hooks/useAlbumArtistContextMenu";
import { usePageScroll } from "../../../hooks/usePageScroll";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Plus } from "lucide-react";
import { AddToPlaylistPopover } from "./AddToPlaylistPopover";

/**
 * An artist's picture, from whichever source the row came from.
 *
 * Its own component because it holds a hook: `renderArtistTile` is a plain
 * function called in a loop, and a hook cannot live there. Calling
 * `useRemoteArtworkSrc` unconditionally — with `null` for a local artist — is
 * what keeps the rule satisfied while both paths share one tile.
 *
 * The load handlers are forwarded to `FadeInImage`, so a cover evicted between
 * being resolved and being painted is invalidated and fetched again here just
 * as it is in `RemoteArtwork`. They are inert on the local path, which has no
 * cache to invalidate.
 */
function ArtistAvatar({ artist }: { artist: LibraryArtistRow }) {
  const {
    src: remoteSrc,
    onError,
    onLoad,
  } = useRemoteArtworkSrc(artist.artwork_hash);
  // Use the full-resolution source so HiDPI screens render the avatar crisp at
  // any column width — same trade-off documented on `AlbumGrid`'s Artwork
  // usage. The 128 px 2x thumbnail upscaled soft on the 180–220 px tiles.
  const localSrc = resolveArtwork(
    {
      full: artist.artwork_path ?? artist.picture_path,
      x1: artist.artwork_path_1x ?? artist.picture_path_1x,
      x2: artist.artwork_path_2x ?? artist.picture_path_2x,
      remoteUrl: artist.picture_url,
    },
    "full",
  );
  const src = artist.artwork_hash ? remoteSrc : localSrc;
  const initial = artist.name.trim().charAt(0).toUpperCase() || "?";

  if (!src) {
    return (
      <div className="w-full aspect-square rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 border border-violet-200/60 dark:border-violet-800/40 flex items-center justify-center overflow-hidden shadow-sm group-hover:shadow-md transition-shadow">
        <span className="text-5xl font-bold text-violet-500/70 dark:text-violet-400/60">
          {initial}
        </span>
      </div>
    );
  }
  return (
    <FadeInImage
      src={src}
      alt={artist.name}
      // No violet border here on purpose — `rounded-full` + a 1 px violet
      // border draws around the image clip, which reads as a visible halo on
      // dark portraits (#106). The gradient is not safe on the wrapper
      // either: `object-cover` covers it everywhere except the rounded
      // edge, where the image's anti-aliased pixels let it through as the
      // same halo, only fainter (#750). It rides the placeholder layer,
      // which fades out once the image is up.
      wrapperClassName="w-full aspect-square rounded-full shadow-sm group-hover:shadow-md transition-shadow"
      placeholderClassName="bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30"
      onError={onError}
      onLoad={onLoad}
      placeholder={
        <span className="text-5xl font-bold text-violet-500/70 dark:text-violet-400/60">
          {initial}
        </span>
      }
    />
  );
}

interface ArtistListProps {
  artists: LibraryArtistRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, artistId: number) => void;
  onCreatePlaylist: (artistId: number) => void;
  onArtistClick: (artistId: number) => void;
  /** A server artist has no local rowid and none of the gestures above. */
  onRemoteArtistClick: (remoteArtistId: string) => void;
  /**
   * Mutable ref the grid populates with a `(idx) => void` callback that
   * scrolls a specific artist into view. Used by the alphabet jump
   * index — `scrollIntoView` on the DOM no longer works because
   * off-screen rows aren't rendered.
   */
  scrollToIndexRef?: React.MutableRefObject<((idx: number) => void) | null>;
}

export function ArtistList({
  artists,
  isLoading,
  t,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onArtistClick,
  onRemoteArtistClick,
  scrollToIndexRef,
}: ArtistListProps) {
  "use no memo";
  const [openMenuArtistId, setOpenMenuArtistId] = useState<number | null>(null);
  // Right-click on a local artist: pin it to the sidebar, or unpin it.
  const artistMenu = useAlbumArtistContextMenu();
  // See AlbumGrid: the `+` button DOM nodes feed the popover's portal
  // positioning, which is the only way to escape the virtualizer's
  // transform-based stacking context.
  const triggerRefs = useRef<Map<number, HTMLButtonElement>>(new Map());

  // Virtual-grid plumbing — see AlbumGrid for the rationale; same math
  // applies to the artist tiles (same `minmax(180px,1fr)` + gap-5).
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(1);
  const [tileWidth, setTileWidth] = useState(180);
  const [scrollMargin, setScrollMargin] = useState(0);
  // Same WebKitGTK compositor workaround as AlbumGrid (#817).
  const positionRowsWithTop = document.documentElement.hasAttribute(
    "data-webkitgtk-safe",
  );

  const MIN_TILE = 180;
  const GAP = 20;
  // Round avatar (width = column width) + space-y-3 (12 px) + 2 lines
  // of text underneath (~40 px) → ~ width + 52.
  const tileHeight = tileWidth + 52;

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
  }, [pageScrollRef, artists.length]);

  const rowCount = Math.ceil(artists.length / colCount);
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => tileHeight + GAP,
    overscan: 2,
    scrollMargin,
  });

  // Expose a scroll-to-artist-index method for the AlphabetIndex (the
  // off-screen rows aren't in the DOM anymore, so the previous
  // `querySelector + scrollIntoView` path can't see them).
  useEffect(() => {
    if (!scrollToIndexRef) return;
    scrollToIndexRef.current = (idx) => {
      virtualizer.scrollToIndex(Math.floor(idx / Math.max(colCount, 1)), {
        align: "start",
      });
    };
    return () => {
      scrollToIndexRef.current = null;
    };
  }, [scrollToIndexRef, virtualizer, colCount]);

  useEffect(() => {
    if (openMenuArtistId == null) return;
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("[data-add-to-playlist-popover]")) return;
      if (target.closest("[data-add-to-playlist-trigger]")) return;
      setOpenMenuArtistId(null);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuArtistId(null);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [openMenuArtistId]);

  const renderArtistTile = (artist: LibraryArtistRow, idx: number) => {
    // A server artist has a UUID and none of the local gestures; see
    // `renderAlbumCard` for why the numeric id is computed rather than
    // coerced.
    const remote = artist.source === "remote";
    const localId = remote ? null : Number(artist.id);
    const isMenuOpen = localId !== null && openMenuArtistId === localId;
    const open = () =>
      localId !== null
        ? onArtistClick(localId)
        : onRemoteArtistClick(artist.id);
    return (
      <div
        key={`${artist.source}:${artist.id}`}
        data-artist-index={idx}
        {...(localId !== null
          ? artistMenu.bind({ kind: "artist", id: localId })
          : {})}
        className="group flex flex-col items-center space-y-3 cursor-pointer relative"
      >
        <div className="relative w-full">
          <ArtistAvatar artist={artist} />
          {/* Same chip as the album grid: one list, every tile says where it
              comes from. */}
          {remote && (
            <span className="absolute top-1 right-1 px-1.5 py-0.5 rounded-md text-[10px] font-medium bg-black/60 text-white backdrop-blur-sm">
              {t("library.source.remote")}
            </span>
          )}
          {/* See `renderAlbumCard`: a real button rather than a role on the
              tile, so the "+" below keeps its own semantics. */}
          <button
            type="button"
            data-artwork-hit-target
            onClick={open}
            aria-label={t("library.open", { name: artist.name })}
            className="absolute inset-0 rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500"
          />
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
                setOpenMenuArtistId(isMenuOpen ? null : localId);
              }}
              aria-label={t("trackActions.addToPlaylist")}
              className={`absolute bottom-1 right-1 p-1.5 rounded-full shadow-sm transition-all focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500 ${
                isMenuOpen
                  ? "opacity-100 bg-emerald-500 text-white"
                  : "opacity-0 reveal:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
              }`}
            >
              <Plus size={16} />
            </button>
          )}
        </div>
        {/* Mouse-only, like the album card: the overlay above is the
            keyboard target. */}
        <div className="text-center px-1 w-full" onClick={open}>
          <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
            {artist.name}
          </div>
          <div className="text-xs text-zinc-500">
            {t("library.artistList.trackCount", {
              count: artist.track_count,
            })}
            {artist.album_count > 0
              ? ` · ${t("library.artistList.albumCount", { count: artist.album_count })}`
              : ""}
          </div>
        </div>
        {isMenuOpen && localId !== null && (
          <AddToPlaylistPopover
            playlists={playlists}
            trackId={localId}
            anchorEl={triggerRefs.current.get(localId) ?? null}
            onPick={(playlistId) => {
              onAddToPlaylist(playlistId, localId);
              setOpenMenuArtistId(null);
            }}
            onCreate={() => {
              setOpenMenuArtistId(null);
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
          const rowItems = artists.slice(startIdx, startIdx + colCount);
          // Same stacking fix as TrackTable / AlbumGrid: bump the row that
          // owns the open `+` popover above the rows rendered after it.
          const rowHasOpenMenu = rowItems.some(
            (artist) =>
              artist.source === "local" &&
              Number(artist.id) === openMenuArtistId,
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
              {rowItems.map((artist, i) =>
                renderArtistTile(artist, startIdx + i),
              )}
            </div>
          );
        })}
      </div>
      {artistMenu.render()}
    </>
  );
}
