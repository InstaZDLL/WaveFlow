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
  Eye,
  EyeOff,
  ImageIcon,
  ArrowUpDown,
  ArrowDown,
  ArrowUp,
  Check,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { EmptyState } from "../common/EmptyState";
import { UploadIcon } from "../common/Icons";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { HiResBadge } from "../common/HiResBadge";
import { Tooltip } from "../common/Tooltip";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { CoverPickerModal } from "../common/CoverPickerModal";
import { StarRating } from "../common/StarRating";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { AlphabetIndex } from "../common/AlphabetIndex";
import { useSortMemory } from "../../hooks/useSortMemory";
import { getProfileSetting } from "../../lib/tauri/profile";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import type { Playlist } from "../../lib/tauri/playlist";
import { pickFolder } from "../../lib/tauri/dialog";
import { setFolderWatched } from "../../lib/tauri/library";
import {
  formatDuration,
  listTracks,
  listLikedTrackIds,
  setTrackRating,
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
  const [coverPickerAlbumId, setCoverPickerAlbumId] = useState<number | null>(null);
  const [coverReloadKey, setCoverReloadKey] = useState(0);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [albums, setAlbums] = useState<AlbumRow[]>([]);
  const [artists, setArtists] = useState<ArtistRow[]>([]);
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [tracksView, setTracksView] = useState<TracksView>("list");
  const [albumsNoCoverFilter, setAlbumsNoCoverFilter] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const selection = useMultiSelect<Track>();
  const EmptyIcon = emptyStateIcons[activeTab];
  const HeaderIcon = headerIcons[activeTab];

  // Sort memory per tab — restored from `profile_setting['sort.<ctx>']`
  // and persisted on every change. The lists wait for `isLoaded` before
  // their first fetch so we don't query twice on mount.
  const tracksSort = useSortMemory("tracks", {
    orderBy: "title",
    direction: "asc",
  });
  const albumsSort = useSortMemory("albums", {
    orderBy: "title",
    direction: "asc",
  });
  const artistsSort = useSortMemory("artists", {
    orderBy: "name",
    direction: "asc",
  });

  // Single-click play. Coexists with multi-select: ctrl/shift always
  // takes precedence (selection), single-click triggers play only on a
  // bare click and only when the toggle is on.
  const [singleClickPlay, setSingleClickPlay] = useState(false);
  useEffect(() => {
    let cancelled = false;
    getProfileSetting("ui.single_click_play")
      .then((v) => {
        if (cancelled) return;
        if (v === "true" || v === "1") setSingleClickPlay(true);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  // Scroll target for the AlphabetIndex (artists tab uses it).
  const artistGridRef = useRef<HTMLDivElement>(null);

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
  const clearSelection = selection.clear;
  useEffect(() => {
    clearSelection();
  }, [activeTab, clearSelection]);

  useEffect(() => {
    // Hold off the first fetch until the relevant sort memory has
    // resolved — otherwise we'd query the default ordering and then
    // re-query the persisted one a tick later.
    if (activeTab === "morceaux" && !tracksSort.isLoaded) return;
    if (activeTab === "albums" && !albumsSort.isLoaded) return;
    if (activeTab === "artistes" && !artistsSort.isLoaded) return;

    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        // Pass null → aggregate across ALL libraries ("Ma musique" mode).
        switch (activeTab) {
          case "morceaux": {
            const list = await listTracks(null, tracksSort.sort);
            if (!cancelled) setTracks(list);
            break;
          }
          case "albums": {
            const list = await listAlbums(null, {
              filterNoCover: albumsNoCoverFilter,
              orderBy: albumsSort.sort.orderBy,
              direction: albumsSort.sort.direction,
            });
            if (!cancelled) setAlbums(list);
            break;
          }
          case "artistes": {
            const list = await listArtists(null, artistsSort.sort);
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
  }, [
    activeTab,
    librariesSignature,
    albumsNoCoverFilter,
    coverReloadKey,
    tracksSort.isLoaded,
    tracksSort.sort,
    albumsSort.isLoaded,
    albumsSort.sort,
    artistsSort.isLoaded,
    artistsSort.sort,
  ]);

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
            <>
              <div className="flex items-center justify-end -mt-4">
                <SortDropdown
                  options={trackSortOptions(t)}
                  current={tracksSort.sort}
                  onChange={tracksSort.setSort}
                  t={t}
                />
              </div>
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
                isSelected={selection.isSelected}
                onRowSelect={(track, e) => {
                  // Modifier-driven selection always wins so multi-select
                  // remains accessible even with single-click play on.
                  if (e.shiftKey) {
                    selection.selectRange(track.id, tracks);
                    return;
                  }
                  if (e.ctrlKey || e.metaKey) {
                    selection.toggleOne(track.id);
                    return;
                  }
                  if (singleClickPlay) {
                    const idx = tracks.findIndex((tr) => tr.id === track.id);
                    if (idx >= 0) {
                      playTracks(tracks, idx, { type: "library", id: null });
                    }
                    selection.clear();
                    return;
                  }
                  selection.setSingle(track.id);
                }}
              />
            </>
          )}
          {activeTab === "albums" && (
            <>
              <div className="flex items-center justify-end space-x-3 -mt-4">
                <label className="inline-flex items-center space-x-2 cursor-pointer select-none text-sm text-zinc-600 dark:text-zinc-300">
                  <input
                    type="checkbox"
                    checked={albumsNoCoverFilter}
                    onChange={(e) => setAlbumsNoCoverFilter(e.target.checked)}
                    className="accent-emerald-500"
                  />
                  <span>{t("library.noCover")}</span>
                </label>
                <SortDropdown
                  options={albumSortOptions(t)}
                  current={albumsSort.sort}
                  onChange={albumsSort.setSort}
                  t={t}
                />
              </div>
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
                onChangeCover={(albumId) => setCoverPickerAlbumId(albumId)}
              />
            </>
          )}
          {activeTab === "artistes" && (
            <>
              <div className="flex items-center justify-end -mt-4">
                <SortDropdown
                  options={artistSortOptions(t)}
                  current={artistsSort.sort}
                  onChange={artistsSort.setSort}
                  t={t}
                />
              </div>
              <div className="relative">
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
                  gridRef={artistGridRef}
                />
                {artistsSort.sort.orderBy === "name" && artists.length > 0 && (
                  <AlphabetIndex
                    items={artists}
                    onLetterClick={(idx) => {
                      const grid = artistGridRef.current;
                      if (!grid) return;
                      const target = grid.querySelector<HTMLElement>(
                        `[data-artist-index="${idx}"]`,
                      );
                      if (target) {
                        target.scrollIntoView({ behavior: "smooth", block: "start" });
                      }
                    }}
                    className="hidden md:flex fixed right-6 top-1/2 -translate-y-1/2 z-30 bg-white/80 dark:bg-zinc-900/70 backdrop-blur-sm rounded-full py-2 px-1.5 shadow-sm"
                  />
                )}
              </div>
            </>
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
              onToggleWatched={(folderId, enable) => {
                // Optimistic flip — the watcher hookup is fire-and-
                // forget on the backend so the UI shouldn't block on it.
                setFolders((prev) =>
                  prev.map((f) =>
                    f.id === folderId ? { ...f, is_watched: enable ? 1 : 0 } : f,
                  ),
                );
                setFolderWatched(folderId, enable).catch((err) => {
                  console.error("[LibraryView] toggle watched failed", err);
                  // Roll back on error.
                  setFolders((prev) =>
                    prev.map((f) =>
                      f.id === folderId ? { ...f, is_watched: enable ? 0 : 1 } : f,
                    ),
                  );
                });
              }}
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

      {coverPickerAlbumId != null && (
        <CoverPickerModal
          albumId={coverPickerAlbumId}
          initialQuery={(() => {
            const a = albums.find((al) => al.id === coverPickerAlbumId);
            if (!a) return "";
            return a.artist_name ? `${a.title} ${a.artist_name}` : a.title;
          })()}
          isOpen={coverPickerAlbumId != null}
          onClose={() => setCoverPickerAlbumId(null)}
          onSuccess={() => setCoverReloadKey((k) => k + 1)}
        />
      )}

      {activeTab === "morceaux" && (
        <SelectionActionBar
          trackIds={[...selection.selectedIds]}
          onClear={selection.clear}
          onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
        />
      )}
    </div>
  );
}

// =============================================================================
// Sort dropdown
// =============================================================================

interface SortOption {
  value: string;
  label: string;
}

function trackSortOptions(t: Translator): SortOption[] {
  return [
    { value: "title", label: t("sort.title") },
    { value: "artist", label: t("sort.artist") },
    { value: "album", label: t("sort.album") },
    { value: "duration_ms", label: t("sort.duration") },
    { value: "year", label: t("sort.year") },
    { value: "added_at", label: t("sort.addedAt") },
    { value: "rating", label: t("sort.rating") },
  ];
}

function albumSortOptions(t: Translator): SortOption[] {
  return [
    { value: "title", label: t("sort.title") },
    { value: "artist", label: t("sort.artist") },
    { value: "year", label: t("sort.year") },
    { value: "added_at", label: t("sort.addedAt") },
  ];
}

function artistSortOptions(t: Translator): SortOption[] {
  return [
    { value: "name", label: t("sort.name") },
    { value: "albums_count", label: t("sort.albumsCount") },
    { value: "tracks_count", label: t("sort.tracksCount") },
  ];
}

interface SortDropdownProps {
  options: SortOption[];
  current: { orderBy: string; direction: "asc" | "desc" };
  onChange: (next: { orderBy: string; direction: "asc" | "desc" }) => void;
  t: Translator;
}

function SortDropdown({ options, current, onChange, t }: SortDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const handleClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKey);
    };
  }, [isOpen]);

  const currentLabel =
    options.find((o) => o.value === current.orderBy)?.label ?? current.orderBy;

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setIsOpen((p) => !p)}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        className="flex items-center space-x-2 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
      >
        <ArrowUpDown size={14} />
        <span>{currentLabel}</span>
        {current.direction === "desc" ? (
          <ArrowDown size={12} />
        ) : (
          <ArrowUp size={12} />
        )}
      </button>
      {isOpen && (
        <ul
          role="listbox"
          className="absolute top-full right-0 mt-2 min-w-56 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated overflow-hidden z-50 animate-fade-in py-1"
        >
          {options.map((opt) => {
            const isSelected = opt.value === current.orderBy;
            return (
              <li key={opt.value} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => {
                    onChange({ orderBy: opt.value, direction: current.direction });
                  }}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30"
                  }`}
                >
                  <span>{opt.label}</span>
                  {isSelected && <Check size={14} />}
                </button>
              </li>
            );
          })}
          <li className="border-t border-zinc-100 dark:border-zinc-700/50 mt-1 pt-1">
            <button
              type="button"
              onClick={() =>
                onChange({
                  orderBy: current.orderBy,
                  direction: current.direction === "asc" ? "desc" : "asc",
                })
              }
              className="w-full flex items-center justify-between px-4 py-2 text-sm text-left text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
            >
              <span>
                {current.direction === "asc"
                  ? t("sort.descending")
                  : t("sort.ascending")}
              </span>
              {current.direction === "asc" ? (
                <ArrowDown size={14} />
              ) : (
                <ArrowUp size={14} />
              )}
            </button>
          </li>
        </ul>
      )}
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
  isSelected: (id: number) => boolean;
  onRowSelect: (track: Track, e: React.MouseEvent) => void;
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
  isSelected,
  onRowSelect,
}: TrackTableProps) {
  "use no memo";
  const unknown = t("library.table.unknown");
  const [openMenuTrackId, setOpenMenuTrackId] = useState<number | null>(null);
  const [ratingOverrides, setRatingOverrides] = useState<Map<number, number | null>>(
    new Map(),
  );
  const scrollRef = useRef<HTMLDivElement>(null);

  // Virtual scroll — only the visible rows are in the DOM.
  const ROW_HEIGHT = view === "list" ? 56 : 44;
  // eslint-disable-next-line react-hooks/incompatible-library
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
      ? "grid-cols-[3rem_2.75rem_1fr_1fr_1fr_7rem_5rem_2rem_2.5rem]"
      : "grid-cols-[3rem_1fr_1fr_1fr_7rem_5rem_2rem_2.5rem]";

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
        <span>{t("library.rating")}</span>
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
            const isRowSelected = isSelected(track.id);
            return (
              <div
                key={track.id}
                onClick={(e) => onRowSelect(track, e)}
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
                  isRowSelected
                    ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
                    : isCurrent
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
                    size="1x"
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
                  <span className="truncate">{track.title}</span>
                  <HiResBadge
                    bitDepth={track.bit_depth}
                    sampleRate={track.sample_rate}
                    variant="inline"
                  />
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
                <div
                  className="flex items-center"
                  onDoubleClick={(e) => e.stopPropagation()}
                >
                  <StarRating
                    value={
                      ratingOverrides.has(track.id)
                        ? ratingOverrides.get(track.id) ?? null
                        : track.rating
                    }
                    size="sm"
                    onChange={(rating) => {
                      setRatingOverrides((prev) => {
                        const next = new Map(prev);
                        next.set(track.id, rating);
                        return next;
                      });
                      setTrackRating(track.id, rating).catch((err) => {
                        console.error("[LibraryView] set rating failed", err);
                        setRatingOverrides((prev) => {
                          const next = new Map(prev);
                          next.delete(track.id);
                          return next;
                        });
                      });
                    }}
                  />
                </div>
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
  onChangeCover: (albumId: number) => void;
}

function AlbumGrid({ albums, isLoading, t, playlists, onAddToPlaylist, onCreatePlaylist, onAlbumClick, onChangeCover }: AlbumGridProps) {
  const unknown = t("library.table.unknown");
  const [openMenuAlbumId, setOpenMenuAlbumId] = useState<number | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    albumId: number;
    x: number;
    y: number;
  } | null>(null);

  useEffect(() => {
    if (contextMenu == null) return;
    const handleClick = () => setContextMenu(null);
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setContextMenu(null);
    };
    document.addEventListener("click", handleClick);
    document.addEventListener("contextmenu", handleClick);
    document.addEventListener("keydown", handleEscape);
    return () => {
      document.removeEventListener("click", handleClick);
      document.removeEventListener("contextmenu", handleClick);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [contextMenu]);

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
    <>
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
            onContextMenu={(e) => {
              e.preventDefault();
              setContextMenu({ albumId: album.id, x: e.clientX, y: e.clientY });
            }}
            className="group flex flex-col space-y-2 cursor-pointer relative"
          >
            <div className="relative">
              <Artwork
                path={album.artwork_path}
                path1x={album.artwork_path_1x}
                path2x={album.artwork_path_2x}
                size="2x"
                alt={album.title}
                className="w-full aspect-square shadow-sm group-hover:shadow-md transition-shadow"
                iconSize={44}
                rounded="2xl"
              />
              <HiResBadge
                bitDepth={album.max_bit_depth}
                sampleRate={album.max_sample_rate}
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
    {contextMenu && (
      <div
        role="menu"
        style={{ top: contextMenu.y, left: contextMenu.x }}
        className="fixed z-100 min-w-48 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in py-1"
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          role="menuitem"
          onClick={() => {
            const id = contextMenu.albumId;
            setContextMenu(null);
            onChangeCover(id);
          }}
          className="w-full flex items-center space-x-2 px-3 py-2 text-left text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
        >
          <ImageIcon size={14} />
          <span>{t("library.changeCover")}</span>
        </button>
      </div>
    )}
    </>
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
  gridRef?: React.RefObject<HTMLDivElement | null>;
}

function ArtistList({ artists, isLoading, t, playlists, onAddToPlaylist, onCreatePlaylist, onArtistClick, gridRef }: ArtistListProps) {
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
      ref={gridRef}
      className={`grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-5 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {artists.map((artist, idx) => {
        const isMenuOpen = openMenuArtistId === artist.id;
        const artistPictureSrc = resolveArtwork(
          {
            full: artist.picture_path,
            x1: artist.picture_path_1x,
            x2: artist.picture_path_2x,
            remoteUrl: artist.picture_url,
          },
          "2x",
        );
        return (
          <div
            key={artist.id}
            data-artist-index={idx}
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
  onToggleWatched: (folderId: number, enable: boolean) => void;
}

function FolderList({
  folders,
  isLoading,
  t,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onToggleWatched,
}: FolderListProps) {
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
            <Tooltip
              label={
                folder.is_watched === 1
                  ? t("library.folderList.watchOff")
                  : t("library.folderList.watchOn")
              }
            >
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleWatched(folder.id, folder.is_watched !== 1);
                }}
                aria-label={
                  folder.is_watched === 1
                    ? t("library.folderList.watchOff")
                    : t("library.folderList.watchOn")
                }
                aria-pressed={folder.is_watched === 1}
                className={`p-1.5 rounded-full transition-colors ${
                  folder.is_watched === 1
                    ? "text-emerald-500 hover:text-emerald-600 hover:bg-emerald-50 dark:hover:bg-emerald-500/10"
                    : "opacity-0 group-hover:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
                }`}
              >
                {folder.is_watched === 1 ? <Eye size={16} /> : <EyeOff size={16} />}
              </button>
            </Tooltip>
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
