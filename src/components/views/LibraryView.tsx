import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Music2,
  Disc,
  Mic2,
  Tags,
  Folder,
  RefreshCcw,
  Clock,
  LayoutList,
  AlignJustify,
  Plus,
  Heart,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { EmptyState } from "../common/EmptyState";
import { UploadIcon } from "../common/Icons";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { Tooltip } from "../common/Tooltip";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import type { Playlist } from "../../lib/tauri/playlist";
import { pickFolder } from "../../lib/tauri/dialog";
import {
  formatDuration,
  listTracks,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  listAlbums,
  listArtists,
  listGenres,
  listFolders,
  type AlbumRow,
  type ArtistRow,
  type GenreRow,
  type FolderRow,
} from "../../lib/tauri/browse";

/** View density for the tracks list: `list` shows cover art, `compact` doesn't. */
type TracksView = "list" | "compact";

interface LibraryViewProps {
  activeTab: LibraryTab;
  setActiveTab: (tab: LibraryTab) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
}

type Translator = (key: string, options?: Record<string, unknown>) => string;

const tabConfig: { id: LibraryTab; icon: typeof Music2 }[] = [
  { id: "morceaux", icon: Music2 },
  { id: "albums", icon: Disc },
  { id: "artistes", icon: Mic2 },
  { id: "genres", icon: Tags },
  { id: "dossiers", icon: Folder },
];

const emptyStateIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  dossiers: Folder,
};

const headerIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  dossiers: Folder,
};

export function LibraryView({ activeTab, setActiveTab, onNavigateToAlbum, onNavigateToArtist }: LibraryViewProps) {
  const { t } = useTranslation();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
    rescanLibrary,
  } = useLibrary();
  const { playTracks, currentTrack } = usePlayer();
  const { playlists, addTracksToPlaylist, addSourceToPlaylist, createPlaylist } = usePlaylist();
  const [isImporting, setIsImporting] = useState(false);
  const [isRescanning, setIsRescanning] = useState(false);
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] = useState(false);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [albums, setAlbums] = useState<AlbumRow[]>([]);
  const [artists, setArtists] = useState<ArtistRow[]>([]);
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [tracksView, setTracksView] = useState<TracksView>("list");
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const EmptyIcon = emptyStateIcons[activeTab];
  const HeaderIcon = headerIcons[activeTab];

  const trackContextMenu = useTrackContextMenu({
    likedIds,
    onLikedChanged: (trackId, nowLiked) =>
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      }),
    onCreatePlaylist: () => setIsCreatePlaylistModalOpen(true),
    onNavigateToAlbum,
    onNavigateToArtist,
  });

  // Re-fetch when any library's updated_at changes (e.g. after a scan).
  const librariesSignature = libraries
    .map((l) => `${l.id}:${l.updated_at}`)
    .join(",");
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        // Pass null → aggregate across ALL libraries ("Ma musique" mode).
        switch (activeTab) {
          case "morceaux": {
            const list = await listTracks(null);
            if (!cancelled) setTracks(list);
            break;
          }
          case "albums": {
            const list = await listAlbums(null);
            if (!cancelled) setAlbums(list);
            break;
          }
          case "artistes": {
            const list = await listArtists(null);
            if (!cancelled) setArtists(list);
            break;
          }
          case "genres": {
            const list = await listGenres(null);
            if (!cancelled) setGenres(list);
            break;
          }
          case "dossiers": {
            const list = await listFolders(null);
            if (!cancelled) setFolders(list);
            break;
          }
        }
      } catch (err) {
        if (!cancelled) {
          console.error("[LibraryView] failed to load tab data", err);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeTab, librariesSignature]);

  // Load liked track IDs once on mount so the TrackTable can show
  // filled hearts. Re-fetches when the libraries change (scan might
  // add new tracks whose liked state we need to know).
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch((err) => console.error("[LibraryView] liked ids failed", err));
  }, [librariesSignature]);

  // Per-tab header subtext uses the fetched data lengths since we
  // aggregate across all libraries (no single Library to read counts from).
  const countForTab = (tab: LibraryTab): number => {
    switch (tab) {
      case "morceaux": return tracks.length;
      case "albums": return albums.length;
      case "artistes": return artists.length;
      case "genres": return genres.length;
      case "dossiers": return folders.length;
    }
  };
  const headerSubtext =
    activeTab === "dossiers"
      ? t("library.header.subtext.dossiers", { count: countForTab("dossiers") })
      : t(`library.header.subtext.${activeTab}`, { count: countForTab(activeTab) });

  const handleImport = async () => {
    if (isImporting) return;
    try {
      const path = await pickFolder(t("library.actions.importFolder"));
      if (!path) return;
      setIsImporting(true);
      // Auto-create a default library if the profile has none.
      let libId = selectedLibraryId;
      if (libId == null) {
        if (libraries.length > 0) {
          libId = libraries[0].id;
          selectLibrary(libId);
        } else {
          const lib = await createLibrary({ name: "Ma musique" });
          libId = lib.id;
          selectLibrary(libId);
        }
      }
      await importFolder(libId, path);
    } catch (err) {
      console.error("[LibraryView] import failed", err);
    } finally {
      setIsImporting(false);
    }
  };

  const handleRescan = async () => {
    if (isRescanning) return;
    setIsRescanning(true);
    try {
      // Rescan every library the profile owns.
      for (const lib of libraries) {
        await rescanLibrary(lib.id);
      }
    } catch (err) {
      console.error("[LibraryView] rescan failed", err);
    } finally {
      setIsRescanning(false);
    }
  };

  const hasContent =
    (activeTab === "morceaux" && tracks.length > 0) ||
    (activeTab === "albums" && albums.length > 0) ||
    (activeTab === "artistes" && artists.length > 0) ||
    (activeTab === "genres" && genres.length > 0) ||
    (activeTab === "dossiers" && folders.length > 0);

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-start justify-between">
        <div className="flex items-center space-x-6">
          <div className="w-24 h-24 rounded-2xl bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400 flex items-center justify-center shadow-sm">
            <Music2 size={48} />
          </div>
          <div>
            <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
              {t("sidebar.myMusic.title")}
            </h1>
            <div className="flex items-center text-sm text-zinc-500 space-x-2">
              <HeaderIcon size={16} />
              <span>{headerSubtext}</span>
            </div>
          </div>
        </div>

        <div className="flex items-center space-x-3">
          <button
            type="button"
            onClick={handleImport}
            disabled={isImporting}
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm shadow-emerald-500/30 disabled:opacity-60 disabled:cursor-not-allowed"
          >
            <Folder size={16} />
            <span>{t("library.header.addFolder")}</span>
          </button>

          <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
            <Tooltip
              label={
                isRescanning
                  ? t("library.actions.rescanning")
                  : t("library.actions.rescan")
              }
            >
              <button
                type="button"
                onClick={handleRescan}
                disabled={libraries.length === 0 || isRescanning}
                aria-label={t("library.actions.rescan")}
                aria-busy={isRescanning}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCcw
                  size={18}
                  className={isRescanning ? "animate-spin" : ""}
                />
              </button>
            </Tooltip>
          </div>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800">
        <div className="flex space-x-6">
          {tabConfig.map((tab) => (
            <Tab
              key={tab.id}
              active={activeTab === tab.id}
              icon={<tab.icon size={18} />}
              label={t(`library.tabs.${tab.id}`)}
              onClick={() => setActiveTab(tab.id)}
            />
          ))}
        </div>

        {/* View density toggle — only meaningful on the tracks tab, kept
            visible elsewhere for layout stability but disabled. */}
        <div
          role="group"
          aria-label={t("library.viewToggle.label")}
          className="flex items-center space-x-1 mb-2"
        >
          <button
            type="button"
            onClick={() => setTracksView("list")}
            aria-pressed={tracksView === "list"}
            aria-label={t("library.viewToggle.list")}
            disabled={activeTab !== "morceaux"}
            className={`p-1.5 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
              tracksView === "list" && activeTab === "morceaux"
                ? "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white"
                : "text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800"
            }`}
          >
            <LayoutList size={18} />
          </button>
          <button
            type="button"
            onClick={() => setTracksView("compact")}
            aria-pressed={tracksView === "compact"}
            aria-label={t("library.viewToggle.compact")}
            disabled={activeTab !== "morceaux"}
            className={`p-1.5 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
              tracksView === "compact" && activeTab === "morceaux"
                ? "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white"
                : "text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800"
            }`}
          >
            <AlignJustify size={18} />
          </button>
        </div>
      </div>

      {hasContent ? (
        <>
          {activeTab === "morceaux" && (
            <TrackTable
              tracks={tracks}
              isLoading={isLoading}
              view={tracksView}
              t={t}
              onPlayTrack={(index) =>
                playTracks(tracks, index, {
                  type: "library",
                  id: null,
                })
              }
              currentTrackId={currentTrack?.id ?? null}
              likedIds={likedIds}
              onToggleLike={async (trackId) => {
                try {
                  const nowLiked = await toggleLikeTrack(trackId);
                  setLikedIds((prev) => {
                    const next = new Set(prev);
                    if (nowLiked) next.add(trackId);
                    else next.delete(trackId);
                    return next;
                  });
                } catch (err) {
                  console.error("[LibraryView] toggle like failed", err);
                }
              }}
              playlists={playlists}
              onAddToPlaylist={async (playlistId, trackId) => {
                try {
                  await addTracksToPlaylist(playlistId, [trackId]);
                } catch (err) {
                  console.error("[LibraryView] add to playlist failed", err);
                }
              }}
              onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
              onNavigateToArtist={onNavigateToArtist}
              onContextMenuRow={trackContextMenu.open}
            />
          )}
          {activeTab === "albums" && (
            <AlbumGrid
              albums={albums}
              isLoading={isLoading}
              t={t}
              playlists={playlists}
              onAddToPlaylist={(playlistId, albumId) =>
                addSourceToPlaylist(playlistId, "album", albumId)
              }
              onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
              onAlbumClick={onNavigateToAlbum}
            />
          )}
          {activeTab === "artistes" && (
            <ArtistList
              artists={artists}
              isLoading={isLoading}
              t={t}
              playlists={playlists}
              onAddToPlaylist={(playlistId, artistId) =>
                addSourceToPlaylist(playlistId, "artist", artistId)
              }
              onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
              onArtistClick={onNavigateToArtist}
            />
          )}
          {activeTab === "genres" && (
            <GenreList genres={genres} isLoading={isLoading} t={t} />
          )}
          {activeTab === "dossiers" && (
            <FolderList
              folders={folders}
              isLoading={isLoading}
              t={t}
              playlists={playlists}
              onAddToPlaylist={(playlistId, folderId) =>
                addSourceToPlaylist(playlistId, "folder", folderId)
              }
              onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
            />
          )}
        </>
      ) : (
        <EmptyState
          icon={<EmptyIcon size={40} />}
          title={t(`library.empty.${activeTab}.title`)}
          description={t(`library.empty.${activeTab}.description`)}
          className="py-20"
        >
          <div className="mt-8 flex items-center flex-wrap justify-center gap-4">
            <button className="bg-emerald-500 hover:bg-emerald-600 text-white px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm">
              <UploadIcon size={18} />
              <span>{t("library.actions.importFiles")}</span>
            </button>
            <button
              type="button"
              onClick={handleImport}
              disabled={isImporting}
              className="px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors border border-zinc-200 bg-white hover:bg-zinc-50 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-300 disabled:opacity-60 disabled:cursor-not-allowed"
            >
              <Folder size={18} />
              <span>{t("library.actions.importFolder")}</span>
            </button>
          </div>
        </EmptyState>
      )}

      {trackContextMenu.render()}

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
            console.error("[LibraryView] create playlist failed", err);
          }
        }}
      />
    </div>
  );
}

// =============================================================================
// Tab-specific list components
// =============================================================================

interface TrackTableProps {
  tracks: Track[];
  isLoading: boolean;
  view: TracksView;
  t: Translator;
  onPlayTrack: (index: number) => void;
  currentTrackId: number | null;
  likedIds: Set<number>;
  onToggleLike: (trackId: number) => void;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, trackId: number) => void;
  onCreatePlaylist: () => void;
  onNavigateToArtist: (artistId: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
}

function TrackTable({
  tracks,
  isLoading,
  view,
  t,
  onPlayTrack,
  currentTrackId,
  likedIds,
  onToggleLike,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onNavigateToArtist,
  onContextMenuRow,
}: TrackTableProps) {
  const unknown = t("library.table.unknown");
  const [openMenuTrackId, setOpenMenuTrackId] = useState<number | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  // Virtual scroll — only the visible rows are in the DOM.
  const ROW_HEIGHT = view === "list" ? 56 : 44;
  const virtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 15,
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

  const gridCols =
    view === "list"
      ? "grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem_2rem_2.5rem]"
      : "grid-cols-[3rem_1fr_1fr_1fr_5rem_2rem_2.5rem]";

  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
      {/* Fixed header */}
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-100 dark:border-zinc-800`}
      >
        <span className="text-right">{t("library.table.number")}</span>
        {view === "list" && <span aria-hidden="true" />}
        <span>{t("library.table.title")}</span>
        <span>{t("library.table.artist")}</span>
        <span>{t("library.table.album")}</span>
        <span
          className="flex justify-end"
          aria-label={t("library.table.duration")}
        >
          <Clock size={14} />
        </span>
        <span aria-hidden="true" />
        <span aria-hidden="true" />
      </div>

      {/* Virtualized body */}
      <div
        ref={scrollRef}
        className={`max-h-[65vh] overflow-y-auto scrollbar-hide ${
          isLoading ? "opacity-50" : ""
        }`}
      >
        <div
          style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}
        >
          {virtualizer.getVirtualItems().map((virtualRow) => {
            const index = virtualRow.index;
            const track = tracks[index];
            const isCurrent = track.id === currentTrackId;
            const isMenuOpen = openMenuTrackId === track.id;
            return (
              <div
                key={track.id}
                onDoubleClick={() => onPlayTrack(index)}
                onContextMenu={(e) => onContextMenuRow(e, track)}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  height: `${virtualRow.size}px`,
                  transform: `translateY(${virtualRow.start}px)`,
                }}
                className={`group grid ${gridCols} gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 ${
                  isCurrent
                    ? "bg-emerald-50 dark:bg-emerald-900/20"
                    : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
                }`}
              >
                <span
                  className={`text-right text-sm tabular-nums ${
                    isCurrent
                      ? "text-emerald-500 font-semibold"
                      : "text-zinc-400"
                  }`}
                >
                  {index + 1}
                </span>
                {view === "list" && (
                  <Artwork
                    path={track.artwork_path}
                    className="w-10 h-10"
                    iconSize={18}
                    alt={track.album_title ?? track.title}
                    rounded="md"
                  />
                )}
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
                  fallback={unknown}
                  className="text-sm text-zinc-500 truncate"
                />
                <span className="text-sm text-zinc-500 truncate">
                  {track.album_title ?? unknown}
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
                    aria-label={likedIds.has(track.id) ? t("liked.unlike") : t("liked.like")}
                    className={`p-1 rounded-full transition-colors ${
                      likedIds.has(track.id)
                        ? "text-pink-500"
                        : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
                    }`}
                  >
                    <Heart
                      size={14}
                      className={likedIds.has(track.id) ? "fill-current" : ""}
                    />
                  </button>
                </div>
                <div className="relative flex justify-center">
                  <button
                    type="button"
                    data-add-to-playlist-trigger
                    onClick={(e) => {
                      e.stopPropagation();
                      setOpenMenuTrackId(isMenuOpen ? null : track.id);
                    }}
                    aria-label={t("trackActions.addToPlaylist")}
                    aria-haspopup="menu"
                    aria-expanded={isMenuOpen}
                    className={`p-1.5 rounded-full transition-all ${
                      isMenuOpen
                        ? "opacity-100 bg-zinc-100 dark:bg-zinc-700 text-zinc-800 dark:text-white"
                        : "opacity-0 group-hover:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                    }`}
                  >
                    <Plus size={16} />
                  </button>
                  {isMenuOpen && (
                    <AddToPlaylistPopover
                      playlists={playlists}
                      trackId={track.id}
                      onPick={(playlistId) => {
                        onAddToPlaylist(playlistId, track.id);
                        setOpenMenuTrackId(null);
                      }}
                      onCreate={() => {
                        setOpenMenuTrackId(null);
                        onCreatePlaylist();
                      }}
                      t={t}
                    />
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

interface AddToPlaylistPopoverProps {
  playlists: Playlist[];
  trackId: number;
  onPick: (playlistId: number) => void;
  onCreate: () => void;
  t: Translator;
}

/**
 * Tiny popover anchored to the trigger button. Lists every playlist of
 * the active profile (resolved color tile + name) plus a "create new"
 * shortcut at the bottom. Picking a row calls `onPick(playlistId)`.
 *
 * Stops `onDoubleClick` from bubbling to the parent `<li>` so clicking a
 * playlist doesn't accidentally start playback of the row underneath.
 */
function AddToPlaylistPopover({
  playlists,
  onPick,
  onCreate,
  t,
}: AddToPlaylistPopoverProps) {
  return (
    <div
      data-add-to-playlist-popover
      role="menu"
      onDoubleClick={(e) => e.stopPropagation()}
      className="absolute top-full right-0 mt-1 z-50 w-56 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in"
    >
      <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase px-3 pt-3 pb-2">
        {t("trackActions.addToPlaylist")}
      </div>
      <div className="px-2 max-h-64 overflow-y-auto">
        {playlists.length === 0 ? (
          <div className="px-2 py-3 text-xs text-zinc-400 text-center">
            {t("trackActions.noPlaylists")}
          </div>
        ) : (
          playlists.map((pl) => {
            const color = resolvePlaylistColor(pl.color_id);
            return (
              <button
                key={pl.id}
                type="button"
                role="menuitem"
                onClick={() => onPick(pl.id)}
                className="w-full flex items-center space-x-2 p-2 rounded-lg text-left hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
              >
                <div
                  className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
                >
                  <PlaylistIcon iconId={pl.icon_id} size={14} />
                </div>
                <span className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                  {pl.name}
                </span>
              </button>
            );
          })
        )}
      </div>
      <div className="border-t border-zinc-100 dark:border-zinc-700/50">
        <button
          type="button"
          role="menuitem"
          onClick={onCreate}
          className="w-full flex items-center space-x-2 px-3 py-2 text-left text-sm font-medium text-emerald-500 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 transition-colors"
        >
          <Plus size={14} />
          <span>{t("trackActions.createPlaylist")}</span>
        </button>
      </div>
    </div>
  );
}

interface AlbumGridProps {
  albums: AlbumRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, albumId: number) => void;
  onCreatePlaylist: () => void;
  onAlbumClick: (albumId: number) => void;
}

function AlbumGrid({ albums, isLoading, t, playlists, onAddToPlaylist, onCreatePlaylist, onAlbumClick }: AlbumGridProps) {
  const unknown = t("library.table.unknown");
  const [openMenuAlbumId, setOpenMenuAlbumId] = useState<number | null>(null);

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

  return (
    <div
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {albums.map((album) => {
        const isMenuOpen = openMenuAlbumId === album.id;
        return (
          <div
            key={album.id}
            onClick={() => onAlbumClick(album.id)}
            className="group flex flex-col space-y-2 cursor-pointer relative"
          >
            <div className="relative">
              <Artwork
                path={album.artwork_path}
                alt={album.title}
                className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                iconSize={44}
                rounded="2xl"
              />
              <button
                type="button"
                data-add-to-playlist-trigger
                onClick={(e) => {
                  e.stopPropagation();
                  setOpenMenuAlbumId(isMenuOpen ? null : album.id);
                }}
                aria-label={t("trackActions.addToPlaylist")}
                className={`absolute bottom-2 right-2 p-1.5 rounded-full shadow-sm transition-all ${
                  isMenuOpen
                    ? "opacity-100 bg-emerald-500 text-white"
                    : "opacity-0 group-hover:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
                }`}
              >
                <Plus size={16} />
              </button>
            </div>
            <div className="px-1">
              <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
                {album.title}
              </div>
              <div className="text-xs text-zinc-500 truncate">
                {album.artist_name ?? unknown}
              </div>
              <div className="text-[11px] text-zinc-400 mt-1">
                {t("library.albumGrid.trackCount", { count: album.track_count })}
                {album.year ? ` · ${album.year}` : ""}
              </div>
            </div>
            {isMenuOpen && (
              <AddToPlaylistPopover
                playlists={playlists}
                trackId={album.id}
                onPick={(playlistId) => {
                  onAddToPlaylist(playlistId, album.id);
                  setOpenMenuAlbumId(null);
                }}
                onCreate={() => {
                  setOpenMenuAlbumId(null);
                  onCreatePlaylist();
                }}
                t={t}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

interface ArtistListProps {
  artists: ArtistRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, artistId: number) => void;
  onCreatePlaylist: () => void;
  onArtistClick: (artistId: number) => void;
}

function ArtistList({ artists, isLoading, t, playlists, onAddToPlaylist, onCreatePlaylist, onArtistClick }: ArtistListProps) {
  const [openMenuArtistId, setOpenMenuArtistId] = useState<number | null>(null);

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

  return (
    <div
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {artists.map((artist) => {
        const isMenuOpen = openMenuArtistId === artist.id;
        const artistPictureSrc = resolveRemoteImage(
          artist.picture_path,
          artist.picture_url,
        );
        return (
          <div
            key={artist.id}
            onClick={() => onArtistClick(artist.id)}
            className="group flex flex-col items-center space-y-3 cursor-pointer relative"
          >
            <div className="relative w-full">
              {artistPictureSrc ? (
                <img
                  src={artistPictureSrc}
                  alt={artist.name}
                  loading="lazy"
                  className="w-full aspect-square rounded-full object-cover shadow-sm group-hover:shadow-md transition-shadow"
                />
              ) : (
                <div className="w-full aspect-square rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 border border-violet-200/60 dark:border-violet-800/40 flex items-center justify-center overflow-hidden shadow-sm group-hover:shadow-md transition-shadow">
                  <span className="text-5xl font-bold text-violet-500/70 dark:text-violet-400/60">
                    {artist.name.trim().charAt(0).toUpperCase() || "?"}
                  </span>
                </div>
              )}
              <button
                type="button"
                data-add-to-playlist-trigger
                onClick={(e) => {
                  e.stopPropagation();
                  setOpenMenuArtistId(isMenuOpen ? null : artist.id);
                }}
                aria-label={t("trackActions.addToPlaylist")}
                className={`absolute bottom-1 right-1 p-1.5 rounded-full shadow-sm transition-all ${
                  isMenuOpen
                    ? "opacity-100 bg-emerald-500 text-white"
                    : "opacity-0 group-hover:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
                }`}
              >
                <Plus size={16} />
              </button>
            </div>
            <div className="text-center px-1 w-full">
              <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
                {artist.name}
              </div>
              <div className="text-xs text-zinc-500">
                {t("library.artistList.trackCount", { count: artist.track_count })}
                {artist.album_count > 0
                  ? ` · ${t("library.artistList.albumCount", { count: artist.album_count })}`
                  : ""}
              </div>
            </div>
            {isMenuOpen && (
              <AddToPlaylistPopover
                playlists={playlists}
                trackId={artist.id}
                onPick={(playlistId) => {
                  onAddToPlaylist(playlistId, artist.id);
                  setOpenMenuArtistId(null);
                }}
                onCreate={() => {
                  setOpenMenuArtistId(null);
                  onCreatePlaylist();
                }}
                t={t}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

interface GenreListProps {
  genres: GenreRow[];
  isLoading: boolean;
  t: Translator;
}

function GenreList({ genres, isLoading, t }: GenreListProps) {
  return (
    <div
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-4 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {genres.map((genre) => (
        <div
          key={genre.id}
          className="flex items-center space-x-3 p-4 rounded-2xl border border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-800/40 dark:hover:bg-zinc-800/70 transition-colors cursor-pointer"
        >
          <div className="w-12 h-12 rounded-xl bg-amber-100 text-amber-600 dark:bg-amber-950/60 dark:text-amber-400 flex items-center justify-center shrink-0">
            <Tags size={22} />
          </div>
          <div className="flex-1 min-w-0">
            <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
              {genre.name}
            </div>
            <div className="text-xs text-zinc-500">
              {t("library.genreList.trackCount", { count: genre.track_count })}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

interface FolderListProps {
  folders: FolderRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, folderId: number) => void;
  onCreatePlaylist: () => void;
}

function FolderList({ folders, isLoading, t, playlists, onAddToPlaylist, onCreatePlaylist }: FolderListProps) {
  const [openMenuFolderId, setOpenMenuFolderId] = useState<number | null>(null);

  useEffect(() => {
    if (openMenuFolderId == null) return;
    const handleMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement;
      if (target.closest("[data-add-to-playlist-popover]")) return;
      if (target.closest("[data-add-to-playlist-trigger]")) return;
      setOpenMenuFolderId(null);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuFolderId(null);
    };
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [openMenuFolderId]);

  const formatScannedAt = (ts: number | null): string => {
    if (ts == null) return t("library.folderList.neverScanned");
    const d = new Date(ts);
    return d.toLocaleString();
  };
  return (
    <div
      className={`rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 divide-y divide-zinc-100 dark:divide-zinc-800/60 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {folders.map((folder) => {
        const isMenuOpen = openMenuFolderId === folder.id;
        return (
          <div
            key={folder.id}
            className="group flex items-center space-x-4 p-4 hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors relative"
          >
            <div className="w-10 h-10 rounded-lg bg-blue-100 text-blue-600 dark:bg-blue-950/60 dark:text-blue-400 flex items-center justify-center shrink-0">
              <Folder size={20} />
            </div>
            <div className="flex-1 min-w-0">
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                {folder.path}
              </div>
              <div className="text-xs text-zinc-500">
                {t("library.folderList.trackCount", { count: folder.track_count })}
                {" · "}
                {t("library.folderList.lastScanned", {
                  date: formatScannedAt(folder.last_scanned_at),
                })}
              </div>
            </div>
            {folder.is_watched === 1 && (
              <span className="text-[10px] font-bold tracking-widest text-emerald-500 uppercase">
                {t("library.folderList.watched")}
              </span>
            )}
            <div className="relative">
              <button
                type="button"
                data-add-to-playlist-trigger
                onClick={(e) => {
                  e.stopPropagation();
                  setOpenMenuFolderId(isMenuOpen ? null : folder.id);
                }}
                aria-label={t("trackActions.addToPlaylist")}
                className={`p-1.5 rounded-full transition-all ${
                  isMenuOpen
                    ? "opacity-100 bg-emerald-500 text-white"
                    : "opacity-0 group-hover:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                }`}
              >
                <Plus size={16} />
              </button>
              {isMenuOpen && (
                <AddToPlaylistPopover
                  playlists={playlists}
                  trackId={folder.id}
                  onPick={(playlistId) => {
                    onAddToPlaylist(playlistId, folder.id);
                    setOpenMenuFolderId(null);
                  }}
                  onCreate={() => {
                    setOpenMenuFolderId(null);
                    onCreatePlaylist();
                  }}
                  t={t}
                />
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
