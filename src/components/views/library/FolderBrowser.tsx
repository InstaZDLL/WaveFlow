import {
  folderTrackIds,
  type FolderListing,
  type FolderNode,
} from "../../../lib/tauri/browse";
import { openTagWorkshop } from "../../../lib/tagWorkshop";
import { type Translator } from "./libraryTypes";
import { type Playlist } from "../../../lib/tauri/playlist";
import { useEffect, useMemo, useState } from "react";
import {
  ChevronRight,
  CornerLeftUp,
  LayoutGrid,
  LayoutList,
  ListEnd,
  ListMusic,
  Play,
  Plus,
  TagsIcon,
  Wand2,
} from "lucide-react";
import { Tooltip } from "../../common/Tooltip";
import { MenuActionItem } from "../../common/MenuActionItem";
import { Artwork } from "../../common/Artwork";
import { formatBytes } from "../../../lib/format";

// =============================================================================
// First-load skeleton
// =============================================================================

/**
 * Layout-shaped placeholder shown during a tab's *first* fetch (state is
 * empty and `loading[tab]` is true). Each branch mirrors the real list's
 * structure so the swap to live data is a content change, not a layout
 * shift. Subsequent re-fetches (sort change, tag edit) keep the previous
 * data on screen and just dim it via `opacity-50` on the list itself.
 */
interface FolderBrowserProps {
  listing: FolderListing | null;
  isLoading: boolean;
  density: "grid" | "list";
  onDensity: (density: "grid" | "list") => void;
  t: Translator;
  /** The UI's language, for the sizes: a decimal separator is not the
   *  same in every locale WaveFlow ships, and the browser's own locale
   *  is not necessarily the one the user picked here. */
  locale: string;
  /** Descend into a child directory. */
  onOpen: (path: string) => void;
  /** Back out to the list of configured roots. */
  onRoots: () => void;
  playlists: Playlist[];
  onPlay: (path: string) => void;
  onQueue: (path: string) => void;
  onPlayNext: (path: string) => void;
  onAddToPlaylist: (playlistId: number, path: string) => void;
  onCreatePlaylist: (path: string) => void;
  onBatchTag: (path: string) => void;
}

/**
 * Browse the library the way it sits on disk (#578).
 *
 * The tab used to list the configured roots and stop there, which is
 * folder *management*, not folder *browsing* -- someone who organises
 * their music as `Artist/Album/` lost that structure entirely once the
 * library was scanned.
 *
 * The tree is derived from `track.file_path` at query time, so what this
 * shows is what the scanner indexed: a directory holding only files the
 * scanner skipped never appears, which is why no row here can read "0
 * tracks". The counts are recursive, because the question a parent
 * folder answers is "how much is under here".
 *
 * Only the directories are rendered here. The files directly inside are
 * rendered by the caller through the library's own track table, so they
 * arrive with the columns, the sort, the context menu and the properties
 * modal every other track list has.
 */
export function FolderBrowser({
  listing,
  isLoading,
  density,
  onDensity,
  t,
  locale,
  onOpen,
  onRoots,
  playlists: allPlaylists,
  onPlay,
  onQueue,
  onPlayNext,
  onAddToPlaylist,
  onCreatePlaylist,
  onBatchTag,
}: FolderBrowserProps) {
  // Only playlists a track can be added to: a smart playlist is rebuilt
  // from its rules and would drop the track at the next regeneration.
  const playlists = allPlaylists.filter((pl) => pl.is_smart !== 1);
  const [menuOpen, setMenuOpen] = useState(false);

  // Escape closes it, like every other menu here. Bound only while it is
  // open, so the app carries no listener for a menu nobody opened.
  useEffect(() => {
    if (!menuOpen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [menuOpen]);

  // Crumbs from the library root down to here. Built from the two paths
  // rather than from a stack of visited folders, so arriving by any
  // route -- a click, a restored state, a deep link later -- gives the
  // same trail.
  const crumbs = useMemo(() => {
    if (!listing) return [] as { label: string; path: string }[];
    const sep = listing.path.includes("\\") ? "\\" : "/";
    const root = listing.root_path;
    // No root means the backend matched none -- a folder outside every
    // configured root. Splitting the whole absolute path then invents
    // crumbs whose targets are relative fragments (`m`, `m/Rock`) that
    // lead nowhere, so the trail stops at where we actually are.
    if (!root) {
      const here = listing.path.split(sep).filter(Boolean).pop();
      return [{ label: here || listing.path, path: listing.path }];
    }
    // `||`, not `??`: a root of `/` leaves an empty last segment, which
    // is a value rather than a missing one, and would render a crumb
    // with no label at all.
    const rootLabel = root.split(sep).filter(Boolean).pop() || root;
    const out = [{ label: rootLabel, path: root }];
    if (listing.path.length > root.length) {
      const rest = listing.path.slice(root.length).split(sep).filter(Boolean);
      let walked = root;
      for (const segment of rest) {
        walked = `${walked}${sep}${segment}`;
        out.push({ label: segment, path: walked });
      }
    }
    return out;
  }, [listing]);

  const here = listing?.path ?? null;

  return (
    <div className="space-y-4">
      {/* Breadcrumb, and the two ways out: one level up, or all the way
          back to the list of roots. */}
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <nav
          aria-label={t("library.folderBrowser.breadcrumb")}
          className="flex items-center gap-1 min-w-0 text-sm"
        >
          <button
            type="button"
            onClick={onRoots}
            className="px-2 py-1 rounded-md text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-zinc-100 transition-colors shrink-0"
          >
            {t("library.folderBrowser.roots")}
          </button>
          {crumbs.map((crumb, index) => (
            <span key={crumb.path} className="flex items-center min-w-0">
              <ChevronRight
                size={14}
                className="text-zinc-400 dark:text-zinc-600 shrink-0"
              />
              <button
                type="button"
                onClick={() => onOpen(crumb.path)}
                aria-current={index === crumbs.length - 1 ? "page" : undefined}
                className={`px-2 py-1 rounded-md truncate transition-colors ${
                  index === crumbs.length - 1
                    ? "font-semibold text-zinc-900 dark:text-white"
                    : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-zinc-100"
                }`}
              >
                {crumb.label}
              </button>
            </span>
          ))}
        </nav>

        <div className="flex items-center gap-2">
          {/* An explicit parent entry: relying on browser-style back
              would leave no way up from a folder reached directly. */}
          <Tooltip label={t("library.folderBrowser.up")}>
            <button
              type="button"
              onClick={() =>
                listing?.parent ? onOpen(listing.parent) : onRoots()
              }
              className="p-1.5 rounded-md text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800 transition-colors"
              aria-label={t("library.folderBrowser.up")}
            >
              <CornerLeftUp size={18} />
            </button>
          </Tooltip>

          {/* Only where it governs something. This pair lays out the
              subfolders, and a leaf folder has none — the track table
              below belongs to LibraryView and has its own density. Left
              on screen there, both buttons take the pressed state and
              nothing moves, which reads as a broken page (#739). */}
          {(listing?.folders.length ?? 0) > 0 && (
            <div
              role="group"
              aria-label={t("library.folderBrowser.density")}
              className="flex items-center gap-1"
            >
              <button
                type="button"
                onClick={() => onDensity("grid")}
                aria-pressed={density === "grid"}
                aria-label={t("library.folderBrowser.densityGrid")}
                className={`p-1.5 rounded-md transition-colors ${
                  density === "grid"
                    ? "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white"
                    : "text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800"
                }`}
              >
                <LayoutGrid size={18} />
              </button>
              <button
                type="button"
                onClick={() => onDensity("list")}
                aria-pressed={density === "list"}
                aria-label={t("library.folderBrowser.densityList")}
                className={`p-1.5 rounded-md transition-colors ${
                  density === "list"
                    ? "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white"
                    : "text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800"
                }`}
              >
                <LayoutList size={18} />
              </button>
            </div>
          )}

          {/* The actions the album and artist views already offer, on the
              folder being viewed and everything under it. */}
          {here && (
            <div className="relative">
              <Tooltip label={t("library.folderBrowser.actions")}>
                <button
                  type="button"
                  onClick={() => setMenuOpen((open) => !open)}
                  aria-expanded={menuOpen}
                  aria-label={t("library.folderBrowser.actions")}
                  className="p-1.5 rounded-md text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800 transition-colors"
                >
                  <Plus size={18} />
                </button>
              </Tooltip>
              {menuOpen && (
                <>
                  <button
                    type="button"
                    aria-hidden="true"
                    tabIndex={-1}
                    className="fixed inset-0 z-40 cursor-default"
                    onClick={() => setMenuOpen(false)}
                  />
                  {/* A plain group of buttons, not `role="menu"`:
                      `MenuActionItem` renders a button with no
                      `menuitem` role, and announcing a menu promises
                      arrow-key navigation nothing here implements. */}
                  <div className="absolute right-0 top-full mt-1 z-50 w-60 rounded-xl border border-zinc-200 bg-white py-1 shadow-lg dark:border-zinc-700 dark:bg-zinc-800">
                    <MenuActionItem
                      icon={<Play size={15} />}
                      label={t("library.folderBrowser.play")}
                      onClick={() => {
                        onPlay(here);
                        setMenuOpen(false);
                      }}
                    />
                    <MenuActionItem
                      icon={<ListEnd size={15} />}
                      label={t("library.folderBrowser.queue")}
                      onClick={() => {
                        onQueue(here);
                        setMenuOpen(false);
                      }}
                    />
                    <MenuActionItem
                      icon={<ListMusic size={15} />}
                      label={t("library.folderBrowser.playNext")}
                      onClick={() => {
                        onPlayNext(here);
                        setMenuOpen(false);
                      }}
                    />
                    <MenuActionItem
                      icon={<TagsIcon size={15} />}
                      label={t("library.folderBrowser.batchTag")}
                      onClick={() => {
                        onBatchTag(here);
                        setMenuOpen(false);
                      }}
                    />
                    <MenuActionItem
                      icon={<Wand2 size={15} />}
                      label={t("tagWorkshop.openFromMenu")}
                      onClick={() => {
                        setMenuOpen(false);
                        void folderTrackIds(null, here)
                          .then((ids) => {
                            if (ids.length > 0)
                              openTagWorkshop({
                                source: { kind: "tracks", track_ids: ids },
                              });
                          })
                          .catch((err: unknown) =>
                            console.error(
                              "[FolderBrowser] folder ids failed",
                              err,
                            ),
                          );
                      }}
                    />
                    <div className="my-1 h-px bg-zinc-100 dark:bg-zinc-700/60" />
                    <MenuActionItem
                      icon={<Plus size={15} />}
                      label={t("library.folderBrowser.newPlaylist")}
                      onClick={() => {
                        onCreatePlaylist(here);
                        setMenuOpen(false);
                      }}
                    />
                    {/* Bounded like `AddToPlaylistPopover`: a user with
                        fifty playlists would otherwise get a menu taller
                        than the window, with its last rows unreachable. */}
                    <div className="max-h-64 overflow-y-auto">
                      {playlists.map((playlist) => (
                        <MenuActionItem
                          key={playlist.id}
                          icon={<ListMusic size={15} />}
                          label={playlist.name}
                          onClick={() => {
                            onAddToPlaylist(playlist.id, here);
                            setMenuOpen(false);
                          }}
                        />
                      ))}
                    </div>
                  </div>
                </>
              )}
            </div>
          )}
        </div>
      </div>

      {isLoading && !listing ? (
        <div className="h-24 rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 animate-pulse" />
      ) : listing && listing.folders.length > 0 ? (
        density === "grid" ? (
          <div
            className="grid gap-4"
            style={{
              gridTemplateColumns: "repeat(auto-fill, minmax(140px, 1fr))",
            }}
          >
            {listing.folders.map((folder) => (
              <FolderTile
                key={folder.path}
                folder={folder}
                t={t}
                locale={locale}
                onOpen={onOpen}
              />
            ))}
          </div>
        ) : (
          <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 divide-y divide-zinc-100 dark:divide-zinc-800/60">
            {listing.folders.map((folder) => (
              <button
                key={folder.path}
                type="button"
                onClick={() => onOpen(folder.path)}
                className="flex w-full items-center gap-4 p-3 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors"
              >
                <Artwork
                  path={folder.artwork_path}
                  path1x={folder.artwork_path_1x}
                  path2x={folder.artwork_path_2x}
                  size="1x"
                  className="w-10 h-10 rounded-lg shrink-0"
                  iconSize={18}
                />
                <span className="flex-1 min-w-0">
                  <span className="block text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                    {folder.name}
                  </span>
                  <span className="block text-xs text-zinc-500">
                    {t("library.folderList.trackCount", {
                      count: folder.track_count,
                    })}
                    {" · "}
                    {formatBytes(folder.total_size, locale)}
                  </span>
                </span>
                <ChevronRight
                  size={16}
                  className="text-zinc-400 dark:text-zinc-600 shrink-0"
                />
              </button>
            ))}
          </div>
        )
      ) : null}
    </div>
  );
}

/** One directory as a cover tile. */
function FolderTile({
  folder,
  t,
  locale,
  onOpen,
}: {
  folder: FolderNode;
  t: Translator;
  locale: string;
  onOpen: (path: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onOpen(folder.path)}
      className="group text-left"
    >
      <Artwork
        path={folder.artwork_path}
        path1x={folder.artwork_path_1x}
        path2x={folder.artwork_path_2x}
        size="2x"
        className="w-full aspect-square rounded-xl mb-2"
        iconSize={28}
      />
      <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate group-hover:underline">
        {folder.name}
      </div>
      <div className="text-xs text-zinc-500 truncate">
        {t("library.folderList.trackCount", { count: folder.track_count })}
        {" · "}
        {formatBytes(folder.total_size, locale)}
      </div>
    </button>
  );
}
