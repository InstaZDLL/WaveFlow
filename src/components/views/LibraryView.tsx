import { useEffect, useRef, useState } from "react";
import {
  Library,
  Music2,
  Disc,
  Mic2,
  Tags,
  Folder,
  Share,
  RefreshCcw,
  Image as ImageIcon,
  Edit2,
  Trash2,
  Clock,
  LayoutList,
  AlignJustify,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { EmptyState } from "../common/EmptyState";
import { UploadIcon } from "../common/Icons";
import { Artwork } from "../common/Artwork";
import { Tooltip } from "../common/Tooltip";
import { CreateLibraryModal } from "../common/CreateLibraryModal";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { pickFolder } from "../../lib/tauri/dialog";
import { formatDuration, listTracks, type Track } from "../../lib/tauri/track";
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

export function LibraryView({ activeTab, setActiveTab }: LibraryViewProps) {
  const { t } = useTranslation();
  const {
    selectedLibrary,
    selectedLibraryId,
    importFolder,
    updateLibrary,
    deleteLibrary,
    rescanLibrary,
  } = useLibrary();
  const { playTracks, currentTrack } = usePlayer();
  const [isImporting, setIsImporting] = useState(false);
  const [isRescanning, setIsRescanning] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [isEditModalOpen, setIsEditModalOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [albums, setAlbums] = useState<AlbumRow[]>([]);
  const [artists, setArtists] = useState<ArtistRow[]>([]);
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [tracksView, setTracksView] = useState<TracksView>("list");
  const confirmTimeoutRef = useRef<number | null>(null);
  const EmptyIcon = emptyStateIcons[activeTab];
  const HeaderIcon = headerIcons[activeTab];

  // Tear down any pending "confirm delete" revert timer on unmount so we
  // don't setState after the component is gone.
  useEffect(() => {
    return () => {
      if (confirmTimeoutRef.current != null) {
        window.clearTimeout(confirmTimeoutRef.current);
      }
    };
  }, []);

  // Reload the dataset for the currently selected tab whenever the library
  // changes or its `updated_at` bumps (end of scan). Keying off `updated_at`
  // means a fresh import auto-refreshes the view without any manual trigger.
  const selectedLibraryUpdatedAt = selectedLibrary?.updated_at;
  useEffect(() => {
    let cancelled = false;
    if (selectedLibraryId == null) {
      setTracks([]);
      setAlbums([]);
      setArtists([]);
      setGenres([]);
      setFolders([]);
      return;
    }
    (async () => {
      setIsLoading(true);
      try {
        switch (activeTab) {
          case "morceaux": {
            const list = await listTracks(selectedLibraryId);
            if (!cancelled) setTracks(list);
            break;
          }
          case "albums": {
            const list = await listAlbums(selectedLibraryId);
            if (!cancelled) setAlbums(list);
            break;
          }
          case "artistes": {
            const list = await listArtists(selectedLibraryId);
            if (!cancelled) setArtists(list);
            break;
          }
          case "genres": {
            const list = await listGenres(selectedLibraryId);
            if (!cancelled) setGenres(list);
            break;
          }
          case "dossiers": {
            const list = await listFolders(selectedLibraryId);
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
  }, [activeTab, selectedLibraryId, selectedLibraryUpdatedAt]);

  // Per-tab header subtext: each tab surfaces the count relevant to what it
  // actually lists, using the pre-aggregated counts on `Library`.
  const countForTab = (tab: LibraryTab): number => {
    switch (tab) {
      case "morceaux":
        return selectedLibrary?.track_count ?? 0;
      case "albums":
        return selectedLibrary?.album_count ?? 0;
      case "artistes":
        return selectedLibrary?.artist_count ?? 0;
      case "genres":
        return selectedLibrary?.genre_count ?? 0;
      case "dossiers":
        return selectedLibrary?.folder_count ?? 0;
    }
  };
  const headerSubtext =
    activeTab === "dossiers"
      ? t("library.header.subtext.dossiers", { count: countForTab("dossiers") })
      : t(`library.header.subtext.${activeTab}`, { count: countForTab(activeTab) });

  const libraryName = selectedLibrary?.name ?? t("sidebar.library.none");

  const handleImport = async () => {
    if (isImporting || selectedLibraryId == null) return;
    try {
      const path = await pickFolder(t("library.actions.importFolder"));
      if (!path) return;
      setIsImporting(true);
      await importFolder(selectedLibraryId, path);
    } catch (err) {
      console.error("[LibraryView] import failed", err);
    } finally {
      setIsImporting(false);
    }
  };

  const handleRescan = async () => {
    if (isRescanning || selectedLibraryId == null) return;
    setIsRescanning(true);
    try {
      await rescanLibrary(selectedLibraryId);
    } catch (err) {
      console.error("[LibraryView] rescan failed", err);
    } finally {
      setIsRescanning(false);
    }
  };

  const handleEditSubmit = async (name: string, description: string) => {
    if (selectedLibraryId == null) return;
    try {
      await updateLibrary(selectedLibraryId, {
        name,
        description: description || null,
      });
    } catch (err) {
      console.error("[LibraryView] update failed", err);
    }
  };

  /**
   * Two-step delete: the first click flips the button into a "confirm?"
   * state that auto-reverts after 3s. The second click within that window
   * actually deletes the library. Avoids the ugly `window.confirm` dialog
   * without needing a full modal component.
   */
  const handleDeleteClick = async () => {
    if (selectedLibraryId == null || isDeleting) return;
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
    // Confirmed — actually delete.
    if (confirmTimeoutRef.current != null) {
      window.clearTimeout(confirmTimeoutRef.current);
      confirmTimeoutRef.current = null;
    }
    setIsDeleting(true);
    try {
      await deleteLibrary(selectedLibraryId);
    } catch (err) {
      console.error("[LibraryView] delete failed", err);
    } finally {
      setIsDeleting(false);
      setConfirmDelete(false);
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
            <Library size={48} />
          </div>
          <div>
            <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
              {libraryName}
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
            disabled={isImporting || selectedLibraryId == null}
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm shadow-emerald-500/30 disabled:opacity-60 disabled:cursor-not-allowed"
          >
            <Folder size={16} />
            <span>{t("library.header.addFolder")}</span>
          </button>

          <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
            <Tooltip label={t("library.actions.share")}>
              <button
                type="button"
                disabled
                aria-label={t("library.actions.share")}
                className="p-2 rounded-lg text-zinc-300 dark:text-zinc-600 cursor-not-allowed"
              >
                <Share size={18} />
              </button>
            </Tooltip>

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
                disabled={selectedLibraryId == null || isRescanning}
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

            <Tooltip label={t("library.actions.changeArtwork")}>
              <button
                type="button"
                disabled
                aria-label={t("library.actions.changeArtwork")}
                className="p-2 rounded-lg text-zinc-300 dark:text-zinc-600 cursor-not-allowed"
              >
                <ImageIcon size={18} />
              </button>
            </Tooltip>

            <Tooltip label={t("library.actions.edit")}>
              <button
                type="button"
                onClick={() => setIsEditModalOpen(true)}
                disabled={selectedLibraryId == null}
                aria-label={t("library.actions.edit")}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <Edit2 size={18} />
              </button>
            </Tooltip>

            <Tooltip
              label={
                confirmDelete
                  ? t("library.actions.deleteConfirm")
                  : t("library.actions.delete")
              }
            >
              <button
                type="button"
                onClick={handleDeleteClick}
                disabled={selectedLibraryId == null || isDeleting}
                aria-label={t("library.actions.delete")}
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
                  id: selectedLibraryId,
                })
              }
              currentTrackId={currentTrack?.id ?? null}
            />
          )}
          {activeTab === "albums" && (
            <AlbumGrid albums={albums} isLoading={isLoading} t={t} />
          )}
          {activeTab === "artistes" && (
            <ArtistList artists={artists} isLoading={isLoading} t={t} />
          )}
          {activeTab === "genres" && (
            <GenreList genres={genres} isLoading={isLoading} t={t} />
          )}
          {activeTab === "dossiers" && (
            <FolderList folders={folders} isLoading={isLoading} t={t} />
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
              disabled={isImporting || selectedLibraryId == null}
              className="px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors border border-zinc-200 bg-white hover:bg-zinc-50 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-300 disabled:opacity-60 disabled:cursor-not-allowed"
            >
              <Folder size={18} />
              <span>{t("library.actions.importFolder")}</span>
            </button>
          </div>
        </EmptyState>
      )}

      <CreateLibraryModal
        isOpen={isEditModalOpen}
        onClose={() => setIsEditModalOpen(false)}
        mode="edit"
        initialName={selectedLibrary?.name ?? ""}
        initialDescription={selectedLibrary?.description ?? ""}
        onSubmit={handleEditSubmit}
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
}

function TrackTable({
  tracks,
  isLoading,
  view,
  t,
  onPlayTrack,
  currentTrackId,
}: TrackTableProps) {
  const unknown = t("library.table.unknown");
  // List mode inserts a 2.75rem cover column between # and Title; compact
  // mode keeps the plain 5-column grid.
  const gridCols =
    view === "list"
      ? "grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem]"
      : "grid-cols-[3rem_1fr_1fr_1fr_5rem]";
  const rowPadding = view === "list" ? "py-2" : "py-2.5";
  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden">
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
      </div>
      <ul
        className={`divide-y divide-zinc-100 dark:divide-zinc-800/60 ${
          isLoading ? "opacity-50" : ""
        }`}
      >
        {tracks.map((track, index) => {
          const isCurrent = track.id === currentTrackId;
          return (
            <li
              key={track.id}
              onDoubleClick={() => onPlayTrack(index)}
              className={`grid ${gridCols} gap-4 px-5 ${rowPadding} items-center select-none transition-colors cursor-pointer ${
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
              <span className="text-sm text-zinc-500 truncate">
                {track.artist_name ?? unknown}
              </span>
              <span className="text-sm text-zinc-500 truncate">
                {track.album_title ?? unknown}
              </span>
              <span className="text-sm tabular-nums text-zinc-400 text-right">
                {formatDuration(track.duration_ms)}
              </span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

interface AlbumGridProps {
  albums: AlbumRow[];
  isLoading: boolean;
  t: Translator;
}

function AlbumGrid({ albums, isLoading, t }: AlbumGridProps) {
  const unknown = t("library.table.unknown");
  return (
    <div
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {albums.map((album) => (
        <div
          key={album.id}
          className="group flex flex-col space-y-2 cursor-pointer"
        >
          <Artwork
            path={album.artwork_path}
            alt={album.title}
            className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
            iconSize={44}
            rounded="2xl"
          />
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
        </div>
      ))}
    </div>
  );
}

interface ArtistListProps {
  artists: ArtistRow[];
  isLoading: boolean;
  t: Translator;
}

function ArtistList({ artists, isLoading, t }: ArtistListProps) {
  return (
    <div
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {artists.map((artist) => (
        <div
          key={artist.id}
          className="group flex flex-col items-center space-y-3 cursor-pointer"
        >
          <div className="w-full aspect-square rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 border border-violet-200/60 dark:border-violet-800/40 flex items-center justify-center overflow-hidden shadow-sm group-hover:shadow-md transition-shadow">
            <span className="text-5xl font-bold text-violet-500/70 dark:text-violet-400/60">
              {artist.name.trim().charAt(0).toUpperCase() || "?"}
            </span>
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
        </div>
      ))}
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
}

function FolderList({ folders, isLoading, t }: FolderListProps) {
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
      {folders.map((folder) => (
        <div
          key={folder.id}
          className="flex items-center space-x-4 p-4 hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors"
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
        </div>
      ))}
    </div>
  );
}
