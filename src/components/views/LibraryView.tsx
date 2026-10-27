import {
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ColumnPicker } from "./library/ColumnPicker";
import { useTrackColumns } from "../../hooks/useTrackColumns";
import {
  listTrackTagKeys,
  listTrackTagValues,
} from "../../lib/tauri/trackTags";
import { type ColumnId } from "../../lib/trackColumns";
import {
  inventorySummary,
  inventoryTracks,
  type InventoryCategory,
} from "../../lib/tauri/inventory";
import { useLibraryPlaylists } from "../../hooks/useLibraryPlaylists";
import {
  Music2,
  Disc,
  Mic2,
  Tags,
  CalendarDays,
  Folder,
  AlertTriangle,
  RefreshCcw,
  FileSearch,
  ListMusic,
  LayoutList,
  AlignJustify,
  ArrowUpDown,
  ArrowDown,
  ArrowUp,
  Check,
  Loader2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { remotePlayTracks } from "../../lib/tauri/remoteServer";
import { useRemoteSource } from "../../hooks/useRemoteSource";
import {
  useLibrarySource,
  type LibrarySourceFilter,
} from "../../hooks/useLibrarySource";
import { EmptyState } from "../common/EmptyState";
import { Tooltip } from "../common/Tooltip";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { CoverPickerModal } from "../common/CoverPickerModal";
import {
  remoteDownloadTrack,
  remoteListDownloads,
} from "../../lib/tauri/remoteServer";
import { ImportToLibraryModal } from "../common/ImportToLibraryModal";
import { RemoteTrackTagsModal } from "../common/RemoteTrackTagsModal";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { AlphabetIndex } from "../common/AlphabetIndex";
import { useSortIgnoreArticles } from "../../hooks/useSortIgnoreArticles";
import { useSortMemory } from "../../hooks/useSortMemory";
import {
  isPlayClick,
  useSingleClickPlay,
} from "../../hooks/useSingleClickPlay";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useArtistUpdated } from "../../hooks/useArtistUpdated";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import { useSelectAllShortcut } from "../../hooks/useSelectAllShortcut";
import { pickFolder } from "../../lib/tauri/dialog";
import {
  removeFolderFromLibrary,
  setFolderWatched,
  scanFolder,
} from "../../lib/tauri/library";
import {
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  browseFolders,
  listFolderTracks,
  folderTrackIds,
  type FolderListing,
  listGenres,
  listYears,
  type YearRow,
  listFolders,
  type LibraryAlbumRow,
  type LibraryArtistRow,
  type LibraryTrackRow,
  listLibraryAlbums,
  listLibraryArtists,
  listLibraryTracks,
  type GenreRow,
  type FolderRow,
} from "../../lib/tauri/browse";
import { useCreatePlaylistFromModal } from "../../hooks/useCreatePlaylistFromModal";
import { BatchTagEditModal } from "../common/BatchTagEditModal";
import { playerAddToQueue, playerPlayNext } from "../../lib/tauri/player";
import { type TracksView, type Translator } from "./library/libraryTypes";
import { toLocalTrack } from "./library/toLocalTrack";

import {
  AlbumGrid,
  ArtistList,
  FolderBrowser,
  FolderList,
  GenreList,
  InventoryCategories,
  PhantomArtistList,
  PlaylistGrid,
  preloadLibraryTabs,
  TrackTable,
  YearList,
} from "./library/tabs";

interface LibraryViewProps {
  activeTab: LibraryTab;
  setActiveTab: (tab: LibraryTab) => void;
  onNavigateToAlbum: (albumId: number) => void;
  /** A server album opens the remote detail view; the two catalogues are
   *  never merged, so they are never the same page. */
  onNavigateToRemoteAlbum: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist: (remoteArtistId: string) => void;
  onNavigateToRemotePlaylist: (remotePlaylistId: string) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToGenre: (genreId: number) => void;
  /** `null` opens the tracks with no usable year. */
  onNavigateToYear: (year: number | null) => void;
  onNavigateToPlaylist: (playlistId: number) => void;
}

const tabConfig: { id: LibraryTab; icon: typeof Music2 }[] = [
  { id: "tracks", icon: Music2 },
  { id: "albums", icon: Disc },
  { id: "artists", icon: Mic2 },
  { id: "genres", icon: Tags },
  { id: "years", icon: CalendarDays },
  { id: "playlists", icon: ListMusic },
  { id: "folders", icon: Folder },
  { id: "needs-attention", icon: AlertTriangle },
];

const emptyStateIcons: Record<LibraryTab, typeof Music2> = {
  tracks: Music2,
  albums: Disc,
  artists: Mic2,
  genres: Tags,
  years: CalendarDays,
  playlists: ListMusic,
  folders: Folder,
  "needs-attention": AlertTriangle,
};

const headerIcons: Record<LibraryTab, typeof Music2> = {
  tracks: Music2,
  albums: Disc,
  artists: Mic2,
  genres: Tags,
  years: CalendarDays,
  playlists: ListMusic,
  folders: Folder,
  "needs-attention": AlertTriangle,
};

export function LibraryView({
  activeTab,
  setActiveTab,
  onNavigateToAlbum,
  onNavigateToRemoteAlbum,
  onNavigateToArtist,
  onNavigateToRemoteArtist,
  onNavigateToRemotePlaylist,
  onNavigateToGenre,
  onNavigateToYear,
  onNavigateToPlaylist,
}: LibraryViewProps) {
  const { t, i18n } = useTranslation();
  // Every tab's chunk, fetched right after the Library mounts.
  useEffect(() => {
    const handle = window.setTimeout(preloadLibraryTabs, 0);
    return () => window.clearTimeout(handle);
  }, []);
  const createFromModal = useCreatePlaylistFromModal();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
    rescanLibrary,
    refresh: refreshLibraries,
  } = useLibrary();
  const { playTracks, currentTrack, isPlaying } = usePlayer();
  const {
    playlists,
    addTracksToPlaylist,
    removeTrackFromPlaylist,
    addSourceToPlaylist,
  } = usePlaylist();
  const [isImporting, setIsImporting] = useState(false);
  const [isRescanning, setIsRescanning] = useState(false);
  // Separate from `isRescanning` so the two buttons disable each other
  // without either claiming the other's spinner.
  const [isDeepRescanning, setIsDeepRescanning] = useState(false);
  // Which folder a deep rescan (issue #366) is currently running against,
  // if any — drives the spinner on that row's action only, since it's a
  // per-folder action rather than the global "Rescan" button above.
  const [deepRescanFolderId, setDeepRescanFolderId] = useState<number | null>(
    null,
  );

  // Any scan in flight, whichever control started it. `scan_folder_inner`
  // is a writer and SQLite takes one writer at a time, so a folder-level
  // deep pass and a library-wide one must not overlap — before this, the
  // global buttons only guarded against each other and left the
  // per-folder button free to start a second concurrent scan.
  const isAnyRescanActive =
    isRescanning || isDeepRescanning || deepRescanFolderId != null;
  // Server tracks queued for an import, and what to call them in the modal's
  // heading. `null` closes it.
  const [importTargets, setImportTargets] = useState<{
    ids: string[];
    label: string;
  } | null>(null);
  // Server track whose tag editor is open, by its server identifier.
  const [remoteTagsTrackId, setRemoteTagsTrackId] = useState<string | null>(
    null,
  );
  const [downloadingRemote, setDownloadingRemote] = useState<Set<string>>(
    () => new Set(),
  );
  const [downloadedRemote, setDownloadedRemote] = useState<Set<string>>(
    () => new Set(),
  );
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);
  // When the create-playlist modal is opened from a popover's "+ New
  // playlist" entry, remember which source triggered it so we can add
  // its tracks to the freshly created playlist in one step instead of
  // forcing the user to reopen the popover.
  const [pendingSourceForCreate, setPendingSourceForCreate] = useState<
    | { kind: "tracks"; ids: number[] }
    | { kind: "folder" | "album" | "artist"; id: number }
    | null
  >(null);
  const [coverPickerAlbumId, setCoverPickerAlbumId] = useState<number | null>(
    null,
  );
  const [coverReloadKey, setCoverReloadKey] = useState(0);
  const [tracks, setTracks] = useState<LibraryTrackRow[]>([]);
  const [albums, setAlbums] = useState<LibraryAlbumRow[]>([]);
  const librarySource = useLibrarySource();
  const [artists, setArtists] = useState<LibraryArtistRow[]>([]);
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [years, setYears] = useState<YearRow[]>([]);
  const [folders, setFolders] = useState<FolderRow[]>([]);
  // Folder browsing (#578). `null` is the list of configured roots —
  // the tab's original content, which stays reachable because it is
  // also where a root is unwatched or removed.
  const [folderPath, setFolderPath] = useState<string | null>(null);
  const [folderListing, setFolderListing] = useState<FolderListing | null>(
    null,
  );
  const [folderTracks, setFolderTracks] = useState<LibraryTrackRow[]>([]);
  const [folderBusy, setFolderBusy] = useState(false);
  /** Folders as covers, or as rows with names and sizes. */
  const [folderDensity, setFolderDensity] = useState<"grid" | "list">("grid");
  /** Batch tag editor, opened with a whole folder as its scope. */
  const [batchTagIds, setBatchTagIds] = useState<number[] | null>(null);
  /** Drops a response for a folder the user has already left. Same rule
   *  as every other keyed fetch here: a stale answer counts as absent,
   *  not as approximate. */
  const folderRequest = useRef(0);
  // Per-tab loading state — drives both the in-place dim and the
  // first-load skeleton. Independent flags let the 5 fetches run in
  // parallel without one tab's dim leaking onto another. Initial value
  // is `true` everywhere so the skeleton paints on first render instead
  // of a one-frame EmptyState flash before the effects schedule.
  const [loading, setLoading] = useState<Record<LibraryTab, boolean>>({
    tracks: true,
    albums: true,
    artists: true,
    genres: true,
    years: true,
    // Playlists come from PlaylistContext, already loaded — there is no
    // fetch of our own to wait on, so this tab never shows a skeleton.
    playlists: false,
    folders: true,
    // The only tab that does NOT prefetch. Its counts are album-level
    // aggregates plus a walk over every track to chain probable
    // duplicates, so paying for them on every mount of the library --
    // which is what the six other tabs with a query of their own do --
    // would tax people who never open it. That is about the *fetch*,
    // not about this flag: `true`, because an inventory nobody has
    // asked for yet is an empty list, and an empty list is what
    // "nothing to fix" looks like. The first paint would congratulate
    // the user before a single count existed.
    "needs-attention": true,
  });
  // Which columns the track table shows, in what order and how wide
  // (#588). One preference for every list that renders the shared
  // table, so the library, the folder browser and the inventory agree.
  const trackColumns = useTrackColumns();
  // The width of the column currently being dragged, before it is
  // persisted. `useProfileSetting` serializes one database write per
  // call, so writing on every pointer move would queue hundreds of
  // round trips for a single gesture — this is what the table renders
  // from meanwhile, and the commit on release is the only write.
  const [columnPreview, setColumnPreview] = useState<{
    id: ColumnId;
    width: number;
  } | null>(null);
  // The same value again, in a ref, and both spellings are needed.
  //
  // The state is what the table renders from. The ref is what the
  // commit reads, because neither of the other two ways of reading it
  // works: a functional updater would put a database write inside an
  // updater React is free to replay, and the render closure would be a
  // tick behind for the keyboard, which previews and commits back to
  // back with no render in between -- an arrow key would persist the
  // width before the one it just asked for.
  const columnPreviewRef = useRef<{ id: ColumnId; width: number } | null>(null);
  // The layout as it looks right now, preview included. Everything that
  // lays the table out reads this, so the header, the rows and the
  // resize handle cannot disagree mid-drag.
  const liveColumnLayout = useMemo(
    () =>
      columnPreview
        ? {
            ...trackColumns.layout,
            widths: {
              ...trackColumns.layout.widths,
              [columnPreview.id]: columnPreview.width,
            },
          }
        : trackColumns.layout,
    [trackColumns.layout, columnPreview],
  );
  // Tag keys the library actually holds, offered in the picker with a
  // count each. Read once per library change: it is a GROUP BY over a
  // side table, and it only moves when a scan has run.
  const [tagKeys, setTagKeys] = useState<{ key: string; count: number }[]>([]);
  // Values for the chosen `tag:` columns only. Empty -- and never
  // fetched -- while no such column is shown, which is the common case.
  const [tagValues, setTagValues] = useState<
    Map<string, Record<string, string>>
  >(new Map());

  // Inventory tab (#589). Three pieces of state rather than one: the
  // categories survive a category being opened and closed, so reopening
  // one does not re-run the expensive summary.
  const [inventory, setInventory] = useState<InventoryCategory[]>([]);
  const [inventoryCategory, setInventoryCategory] = useState<string | null>(
    null,
  );
  const [inventoryRows, setInventoryRows] = useState<LibraryTrackRow[]>([]);
  const [inventoryBusy, setInventoryBusy] = useState(false);

  const [tracksView, setTracksView] = useState<TracksView>("list");
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
  // Smart playlists (Daily Mix, On Repeat) live in Home's "Made for
  // you" carousel; this tab is the user's own playlists, which is what
  // issue #461 asked for ("the ones we make").
  const userPlaylists = useMemo(
    () => playlists.filter((p) => p.is_smart === 0),
    [playlists],
  );
  // Persisted like every other tab's sort. Default `custom` = the
  // sidebar's manual order, so the grid opens in the arrangement the
  // user already curated there.
  const playlistsSort = useSortMemory("playlists", {
    orderBy: "custom",
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
  const { value: singleClickPlay } = useSingleClickPlay();

  // Scroll target for the AlphabetIndex (artists tab uses it).
  // Callback ref populated by the virtualized ArtistList so the
  // alphabet jump index can scroll a specific artist into view without
  // relying on a `querySelector` that can't reach off-screen rows.
  const artistScrollToIndexRef = useRef<((idx: number) => void) | null>(null);

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
    selectedTrackIds: [...selection.selectedIds],
  });

  // Re-fetch when any library's updated_at changes (e.g. after a scan),
  // and when the sort sets articles aside or stops: the backend sorts
  // artists and albums with it, so every listing comes back reordered.
  const { ignoreArticles } = useSortIgnoreArticles();
  const librariesSignature = `${libraries
    .map((l) => `${l.id}:${l.updated_at}`)
    .join(",")}|${ignoreArticles ? "articles" : "literal"}`;
  // Bumped when a tag edit elsewhere fires `track:updated` so the
  // active tab re-fetches and shows the new metadata.
  const [editRefetch, setEditRefetch] = useState(0);
  // `setEditRefetch` is listed even though a state setter is stable:
  // the React Compiler infers it as a dependency, and a manual list
  // that does not match makes it skip optimizing the whole component.
  useTrackUpdated(
    useCallback(() => setEditRefetch((k) => k + 1), [setEditRefetch]),
  );
  // And when an artist's picture changes: the artists grid reads that
  // image, and a picture set from the artist page used to leave the
  // grid showing the old one until a scan or a tag edit (#692). Its own
  // counter rather than `editRefetch`, which every tab's effect depends
  // on — a new artist photo says nothing about tracks, albums, genres,
  // tags or folders.
  const [artistRefetch, setArtistRefetch] = useState(0);
  useArtistUpdated(
    useCallback(() => setArtistRefetch((k) => k + 1), [setArtistRefetch]),
  );
  const clearSelection = selection.clear;
  // The track table on screen, if any: the tracks tab, an opened folder,
  // or an inventory category. Ctrl+A and the selection bar follow it, so
  // a selection built in any of the three has its actions.
  const listedTracks: LibraryTrackRow[] | null =
    activeTab === "tracks"
      ? tracks
      : // The listing must be the open folder's: right after walking
        // into another one, the previous rows are still in state.
        activeTab === "folders" &&
          folderPath != null &&
          folderListing?.path === folderPath
        ? folderTracks
        : activeTab === "needs-attention" &&
            inventoryCategory != null &&
            inventoryCategory !== "phantom_artist"
          ? inventoryRows
          : null;
  // Local rows only: selection is keyed on local rowids.
  useSelectAllShortcut(
    listedTracks?.some((row) => row.source === "local") ?? false,
    () =>
      selection.selectAll(
        (listedTracks ?? [])
          .filter((row) => row.source === "local")
          .map(toLocalTrack),
      ),
  );
  // Also on `folderPath`: a selection is a set of track ids, and the
  // action bar it feeds would otherwise act on tracks the user can no
  // longer see after walking into another folder (#578).
  useEffect(() => {
    clearSelection();
    // And on the inventory category: switching one shows other rows.
  }, [activeTab, folderPath, inventoryCategory, clearSelection]);

  const { available: remoteAvailable } = useRemoteSource();

  // Which server tracks are already kept offline, so the row's tick is right
  // on first paint rather than only after the user downloads one this session.
  // Silent on failure: a stock build has no such command, and a signed-out one
  // has nothing to report — neither is worth a console error every mount.
  useEffect(() => {
    if (!remoteAvailable) return;
    let cancelled = false;
    void remoteListDownloads()
      .then((kept) => {
        if (!cancelled) {
          setDownloadedRemote(
            new Set(kept.map((entry) => entry.remote_track_id)),
          );
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [remoteAvailable]);

  // Which custom tag keys the library holds. Only moves when a scan
  // has run, so it follows the library signature rather than any
  // render.
  useEffect(() => {
    let cancelled = false;
    listTrackTagKeys()
      .then((keys) => {
        if (!cancelled) setTagKeys(keys);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] listTrackTagKeys failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, [librariesSignature, editRefetch]);

  // Values for the chosen `tag:` columns. Nothing is fetched while none
  // is shown, which is the common case -- and the dependency is the
  // sorted key list rather than the layout, so reordering or resizing a
  // column does not re-query the whole library.
  // The chosen tag keys, as a value that only changes when the SET
  // changes -- reordering or resizing a column must not re-query the
  // whole library. `JSON.stringify` of a sorted array rather than a
  // joined string, because a tag key can contain any character a tagger
  // chose to write and there is no separator that is safe by
  // construction.
  const chosenTagKeys = useMemo(
    () =>
      JSON.stringify(
        trackColumns.layout.order
          .map((id) => (id.startsWith("tag:") ? id.slice(4) : null))
          .filter((key): key is string => key !== null)
          .sort(),
      ),
    [trackColumns.layout.order],
  );
  // Every track's values for the chosen tag columns, in one read.
  //
  // Deliberately not scoped to the rows on screen, which looks like the
  // obvious saving and is not one here: `tracks` already holds every
  // row in the library: the virtualizer windows the *render*, not the
  // data. These values are a few short strings per track against some
  // twenty fields per row, so bounding them alone would make the tag
  // columns the one part of the table fetched per scroll, against a
  // design where the rows arrive once. If the memory matters, it is the
  // row list that has to give first, and then this follows it.
  useEffect(() => {
    const keys = JSON.parse(chosenTagKeys) as string[];
    if (keys.length === 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setTagValues(new Map());
      return;
    }
    let cancelled = false;
    listTrackTagValues(keys)
      .then((byTrack) => {
        if (!cancelled) setTagValues(new Map(Object.entries(byTrack)));
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] listTrackTagValues failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, [chosenTagKeys, librariesSignature, editRefetch]);

  // The inventory is the one tab that fetches on activation rather than
  // in parallel with the others: its counts are album-level aggregates
  // plus a walk over every track, so prefetching them would tax everyone
  // who never opens it. Re-runs when the library changes underneath, and
  // after a tag edit, because fixing something is the whole point and a
  // count that does not move reads as the fix not having worked.
  useEffect(() => {
    if (activeTab !== "needs-attention") return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, "needs-attention": true }));
    inventorySummary()
      .then((list) => {
        if (!cancelled) setInventory(list);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] inventorySummary failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, "needs-attention": false }));
      });
    return () => {
      cancelled = true;
    };
  }, [activeTab, librariesSignature, editRefetch]);

  // Fixing the last track in a category is the success case, and it is
  // the one that leaves the view wrong: the counts come back with that
  // category at zero, `InventoryCategories` greys its button out, and
  // the table underneath stays mounted on an empty list under a chip
  // that can no longer be clicked to close it. So an emptied category
  // closes itself, which puts the user back on the overview -- where
  // the count they just drove to zero is the thing worth seeing.
  useEffect(() => {
    if (inventoryCategory == null) return;
    const open = inventory.find((c) => c.key === inventoryCategory);
    if (open && open.count > 0) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setInventoryCategory(null);
    setInventoryRows([]);
  }, [inventory, inventoryCategory]);

  // Opening a category. Guarded on the key rather than on a request
  // token: a second click on the same category is a no-op, and a click
  // on another one replaces the rows wholesale, so a slow answer for a
  // category the user has left must not paint.
  useEffect(() => {
    if (activeTab !== "needs-attention" || inventoryCategory == null) return;
    // The one category made of artists lists itself (#719).
    if (inventoryCategory === "phantom_artist") return;
    // Same gate the other lists use: firing before the stored sort has
    // been read loads the whole category once in the default order and
    // again in the right one.
    if (!tracksSort.isLoaded) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setInventoryBusy(true);
    inventoryTracks(inventoryCategory, tracksSort.sort)
      .then((rows) => {
        if (!cancelled) setInventoryRows(rows);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] inventoryTracks failed", err);
      })
      .finally(() => {
        if (!cancelled) setInventoryBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [
    activeTab,
    inventoryCategory,
    tracksSort.sort,
    tracksSort.isLoaded,
    // A rescan changes what a category holds, and the open category is
    // the one the user is looking at while it runs.
    librariesSignature,
    editRefetch,
  ]);

  // Per-tab parallel fetchers — each runs independently of `activeTab`,
  // so navigating into LibraryView fires all 5 SQL queries at once and
  // every subsequent tab switch hits cached state instantly. The 500 ms
  // "EmptyState flash" disappears because the data lands during the
  // very first paint instead of after the user picks a tab.
  useEffect(() => {
    // Both preferences gate the fetch, for the reason on the albums effect.
    if (!tracksSort.isLoaded || !librarySource.ready) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, tracks: true }));
    listLibraryTracks(
      null,
      librarySource.source === "all" ? null : librarySource.source,
      tracksSort.sort,
    )
      .then((list) => {
        if (!cancelled) setTracks(list);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] listLibraryTracks failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, tracks: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [
    librariesSignature,
    tracksSort.isLoaded,
    tracksSort.sort,
    librarySource.ready,
    librarySource.source,
    editRefetch,
  ]);

  useEffect(() => {
    // Both preferences gate the fetch: loading with either default and again
    // with the stored value would paint the wrong list first, and the source
    // filter is the more visible of the two to get wrong.
    if (!albumsSort.isLoaded || !librarySource.ready) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, albums: true }));
    listLibraryAlbums(
      null,
      librarySource.source === "all" ? null : librarySource.source,
      {
        orderBy: albumsSort.sort.orderBy,
        direction: albumsSort.sort.direction,
      },
    )
      .then((list) => {
        if (!cancelled) setAlbums(list);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] listLibraryAlbums failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, albums: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [
    librariesSignature,
    albumsSort.isLoaded,
    albumsSort.sort,
    librarySource.ready,
    librarySource.source,
    coverReloadKey,
    editRefetch,
  ]);

  useEffect(() => {
    // Both preferences gate the fetch, for the reason on the albums effect.
    if (!artistsSort.isLoaded || !librarySource.ready) return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, artists: true }));
    listLibraryArtists(
      null,
      librarySource.source === "all" ? null : librarySource.source,
      artistsSort.sort,
    )
      .then((list) => {
        if (!cancelled) setArtists(list);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] listLibraryArtists failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, artists: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [
    librariesSignature,
    artistsSort.isLoaded,
    artistsSort.sort,
    librarySource.ready,
    librarySource.source,
    editRefetch,
    artistRefetch,
  ]);

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, genres: true }));
    listGenres(null)
      .then((list) => {
        if (!cancelled) setGenres(list);
      })
      .catch((err) => {
        if (!cancelled) console.error("[LibraryView] listGenres failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, genres: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [librariesSignature, editRefetch]);

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, years: true }));
    listYears(null)
      .then((list) => {
        if (!cancelled) setYears(list);
      })
      .catch((err) => {
        if (!cancelled) console.error("[LibraryView] listYears failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, years: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [librariesSignature, editRefetch]);

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, folders: true }));
    listFolders(null)
      .then((list) => {
        if (!cancelled) setFolders(list);
      })
      .catch((err) => {
        if (!cancelled) console.error("[LibraryView] listFolders failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, folders: false }));
      });
    return () => {
      cancelled = true;
    };
  }, [librariesSignature]);

  // Load liked track IDs once on mount so the TrackTable can show
  // filled hearts. Re-fetches when the libraries change (scan might
  // add new tracks whose liked state we need to know).
  useEffect(() => {
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch((err) => console.error("[LibraryView] liked ids failed", err));
  }, [librariesSignature]);

  // Merged where they are read: both playlist surfaces already sorted in the
  // browser, so there is nothing for a compound select to unify.
  const libraryPlaylists = useLibraryPlaylists(
    userPlaylists,
    librarySource.source,
  );

  // Browse one folder: its child directories, and the files directly
  // inside it. Two calls rather than one because the files come back in
  // the Tracks tab's own row shape, which is what lets the folder view
  // render the same table with the same sort.
  useEffect(() => {
    // Nothing is cleared here on the way out: the folder branch does
    // not read these states while `folderPath` is null, and clearing
    // them would be a setState cascade for a value nobody looks at.
    // What protects the render instead is the stamp -- `listing.path`
    // has to match the folder being shown, so a previous folder's
    // contents count as absent rather than as approximate.
    if (folderPath == null) return;
    if (!tracksSort.isLoaded) return;
    const mine = folderRequest.current + 1;
    folderRequest.current = mine;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setFolderBusy(true);
    void Promise.all([
      browseFolders(null, folderPath),
      listFolderTracks(null, folderPath, {
        recursive: false,
        ...tracksSort.sort,
      }),
    ])
      .then(([listing, rows]) => {
        if (folderRequest.current !== mine) return;
        setFolderListing(listing);
        setFolderTracks(rows);
      })
      .catch((err: unknown) => {
        if (folderRequest.current !== mine) return;
        console.error("[LibraryView] browseFolders failed", err);
        setFolderListing(null);
        setFolderTracks([]);
      })
      .finally(() => {
        if (folderRequest.current === mine) setFolderBusy(false);
      });
    // `librariesSignature` and `editRefetch` for the same reason every
    // other tab watches them: a rescan or a tag edit changes what is in
    // the folder, and a view that only reloads on navigation would show
    // the user their own edit as missing.
  }, [
    folderPath,
    tracksSort.isLoaded,
    tracksSort.sort,
    librariesSignature,
    editRefetch,
  ]);

  /** Everything under a folder, for the actions that take ids. */
  const idsUnderFolder = useCallback(
    (path: string) =>
      folderTrackIds(null, path).catch((err: unknown) => {
        console.error("[LibraryView] folderTrackIds failed", err);
        return [] as number[];
      }),
    [],
  );

  /** Play a folder whole, in path order. Goes through the same
   *  `playTracks` the Tracks tab uses, with the recursive listing as its
   *  queue — ids alone would not carry what the player needs. */
  const playFolder = useCallback(
    async (path: string) => {
      try {
        const rows = await listFolderTracks(null, path, { recursive: true });
        if (rows.length === 0) return;
        await playTracks(rows.map(toLocalTrack), 0, {
          type: "library",
          id: null,
        });
      } catch (err) {
        console.error("[LibraryView] playFolder failed", err);
      }
    },
    [playTracks],
  );

  // Per-tab header subtext uses the fetched data lengths since we
  // aggregate across all libraries (no single Library to read counts from).
  const countForTab = (tab: LibraryTab): number => {
    switch (tab) {
      case "tracks":
        return tracks.length;
      case "albums":
        return albums.length;
      case "artists":
        return artists.length;
      case "genres":
        return genres.length;
      case "years":
        return years.length;
      case "playlists":
        // The merged list, not the local one: the header would otherwise
        // count a different set from the grid right below it.
        return libraryPlaylists.length;
      case "folders":
        return folders.length;
      case "needs-attention":
        // The number the header should carry is "how many tracks need
        // attention", which is the sum of the categories — and a
        // category can list a track that another one lists too, so this
        // is a count of findings rather than of distinct tracks. The
        // subtext key says so.
        return inventory.reduce((sum, category) => sum + category.count, 0);
    }
  };
  // Built from the tab id, so no literal key for the other tabs
  // appears anywhere in the source -- a search for
  // `library.header.subtext.needs-attention` finds nothing and proves
  // nothing. Each tab's entry is in all 17 locale files, and as CLDR
  // plural forms (`_zero` / `_one` / `_other`, plus `_few` and `_many`
  // where the language has them), because `count` is passed.
  const headerSubtext =
    activeTab === "folders"
      ? t("library.header.subtext.folders", { count: countForTab("folders") })
      : t(`library.header.subtext.${activeTab}`, {
          count: countForTab(activeTab),
        });

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
          const lib = await createLibrary({
            name: t("onboarding.defaultLibraryName"),
          });
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

  const handleDeepRescanLibrary = async () => {
    // Whole-library counterpart of the per-folder deep rescan below.
    // Same rationale (issue #457): the normal pass trusts (mtime, size)
    // and therefore cannot see tags an external editor rewrote while
    // preserving mtime.
    if (isAnyRescanActive) return;
    setIsDeepRescanning(true);
    try {
      // Per-library error handling: one unreadable library (a drive
      // that went away, a permission change) must not strand the ones
      // after it — the user asked for a full pass.
      //
      // A rejected promise is only half the story: `rescan_library`
      // swallows per-folder failures into `summary.errors` and still
      // resolves, so a partially failed pass looks identical to a clean
      // one unless the summary is inspected.
      let failedFolders = 0;
      for (const lib of libraries) {
        try {
          const summary = await rescanLibrary(lib.id, true);
          failedFolders += summary.errors;
          // The walk inside one library already gives up when the user
          // stops it; this loop is the one above it, and without this
          // a stop would kill this library and start the next. Same
          // rule, one level up.
          if (summary.cancelled) break;
        } catch (err) {
          console.error(
            `[LibraryView] deep rescan failed for library ${lib.id}`,
            err,
          );
        }
      }
      if (failedFolders > 0) {
        console.warn(
          `[LibraryView] deep rescan finished with ${failedFolders} folder error(s)`,
        );
      }
    } finally {
      setIsDeepRescanning(false);
    }
  };

  const handleRescan = async () => {
    if (isAnyRescanActive) return;
    setIsRescanning(true);
    try {
      // Rescan every library the profile owns, one failure at a time —
      // same reasoning, and the same summary caveat, as the deep pass
      // above.
      let failedFolders = 0;
      for (const lib of libraries) {
        try {
          const summary = await rescanLibrary(lib.id);
          failedFolders += summary.errors;
          // The walk inside one library already gives up when the user
          // stops it; this loop is the one above it, and without this
          // a stop would kill this library and start the next. Same
          // rule, one level up.
          if (summary.cancelled) break;
        } catch (err) {
          console.error(
            `[LibraryView] rescan failed for library ${lib.id}`,
            err,
          );
        }
      }
      if (failedFolders > 0) {
        console.warn(
          `[LibraryView] rescan finished with ${failedFolders} folder error(s)`,
        );
      }
    } finally {
      setIsRescanning(false);
    }
  };

  const handleDeepRescanFolder = async (folderId: number) => {
    if (isAnyRescanActive) return;
    setDeepRescanFolderId(folderId);
    try {
      await scanFolder(folderId, true);
      // scan_folder doesn't emit `library:rescanned` (only folder
      // removal / tag edits do), and it doesn't touch `library.updated_at`
      // either, so `librariesSignature` never moves. Without refetching
      // here the new tracks reached the database while the Songs, Albums
      // and Artists lists kept the old ones (#613): bump the same refetch
      // an import uses, refresh the library rows for their counts, and
      // reload the folder rows for their last-scan date and track count.
      setEditRefetch((k) => k + 1);
      const [list] = await Promise.all([listFolders(null), refreshLibraries()]);
      setFolders(list);
    } catch (err) {
      console.error("[LibraryView] deep rescan failed", err);
    } finally {
      setDeepRescanFolderId(null);
    }
  };

  // The albums tab can be empty for two different reasons, and they deserve
  // two different answers.
  const sourceFilterEmptied =
    librarySource.source !== "all" &&
    ((activeTab === "tracks" && tracks.length === 0) ||
      (activeTab === "albums" && albums.length === 0) ||
      (activeTab === "artists" && artists.length === 0));
  // Playlists is not in that list on purpose: `PlaylistGrid` owns its own
  // empty state and never falls through to the generic one, so a narrowed
  // source there is already explained where the user is looking.

  // The two engines keep separate queues by design (RFC-005 decision 9), so a
  // mixed list cannot produce a mixed queue. Playing a row therefore queues the
  // run of rows from *its* source — which is why the chip on every row matters,
  // and why narrowing the filter is how you get one continuous queue.
  const playRow = useCallback(
    (index: number) => {
      const row = tracks[index];
      if (!row) return;
      const run = tracks.filter((candidate) => candidate.source === row.source);
      const at = run.findIndex((candidate) => candidate.id === row.id);
      if (row.source === "remote") {
        void remotePlayTracks(
          run.map((candidate) => candidate.id),
          Math.max(at, 0),
        ).catch((err: unknown) =>
          console.error("[LibraryView] remotePlayTracks failed", err),
        );
        return;
      }
      void playTracks(run.map(toLocalTrack), Math.max(at, 0), {
        type: "library",
        id: null,
      });
    },
    [tracks, playTracks],
  );

  const hasContent =
    (activeTab === "tracks" && tracks.length > 0) ||
    (activeTab === "albums" && albums.length > 0) ||
    (activeTab === "artists" && artists.length > 0) ||
    (activeTab === "genres" && genres.length > 0) ||
    (activeTab === "years" && years.length > 0) ||
    // Playlists is renderable even when empty: `PlaylistGrid` owns its
    // own empty state ("playlists you create appear here"), which is the
    // right message. The generic one below is built for a library with
    // no music — it offers "Import a folder", which doesn't create a
    // playlist — and it reads `library.empty.<tab>.*`, keys this tab
    // deliberately doesn't define.
    activeTab === "playlists" ||
    // Inside a folder there is always something to show -- a
    // breadcrumb and a way back at the very least -- even when the
    // folder itself holds nothing.
    (activeTab === "folders" && (folders.length > 0 || folderPath != null)) ||
    // The inventory always has something to show: the categories
    // themselves, including the "nothing to fix" reading, which is the
    // answer to the question the user came here with. Listing it here
    // also keeps `activeTab` from being narrowed out of the branch that
    // renders it — `hasContent` is a `const` built from comparisons, so
    // TypeScript treats it as a discriminant.
    activeTab === "needs-attention";

  /** Play a file of the folder being browsed, from that folder's own
   *  list: the queue a click builds is the folder the user is looking
   *  at, not the whole library. */
  const playFolderRow = useCallback(
    (index: number) => {
      if (folderTracks.length === 0) return;
      void playTracks(folderTracks.map(toLocalTrack), index, {
        type: "library",
        id: null,
      });
    },
    [folderTracks, playTracks],
  );

  /** The library's track table, over whichever rows the active tab
   *  holds: the Tracks tab's own list, or the files directly inside the
   *  folder being browsed (#578). A second call site would have meant a
   *  second table, drifting from this one's columns, sort, context menu
   *  and properties modal -- which is exactly what the folder view was
   *  asked not to invent.
   *
   *  `rows` is also what selection and single-click play range over, so
   *  each caller's rows are the ones its clicks act on. */
  const renderTrackTable = (
    rows: LibraryTrackRow[],
    busy: boolean,
    onPlayRow: (index: number) => void,
  ) => (
    <TrackTable
      // Nothing paints until the stored column choice has been read
      // for the active profile. `useProfileSetting` answers with the
      // default until then, so rendering would lay out one frame of
      // default columns and then move every one of them -- under the
      // pointer, and on every profile switch.
      tracks={trackColumns.ready ? rows : []}
      isLoading={busy || !trackColumns.ready}
      view={tracksView}
      t={t}
      locale={i18n.resolvedLanguage ?? i18n.language}
      layout={liveColumnLayout}
      sort={tracksSort.sort}
      onSort={(orderBy) =>
        // Clicking the active column flips the direction; clicking
        // another one starts it at whatever reads as "most first"
        // for that column, which the backend decides.
        tracksSort.setSort({
          orderBy,
          direction:
            tracksSort.sort.orderBy === orderBy &&
            tracksSort.sort.direction === "asc"
              ? "desc"
              : "asc",
        })
      }
      onPreviewColumn={(id, width) => {
        columnPreviewRef.current = { id, width };
        setColumnPreview({ id, width });
      }}
      onCommitColumn={(id) => {
        const current = columnPreviewRef.current;
        columnPreviewRef.current = null;
        if (current && current.id === id) {
          void trackColumns.setWidth(id, current.width);
        }
        setColumnPreview(null);
      }}
      tagValues={tagValues}
      onPlayTrack={(index) => {
        // Playing a row clears the selection its own click made (#897): with
        // double-click play the first click selects the row, and the blue
        // selection then outranks the green "now playing" on that very
        // row. Single-click play already did this.
        onPlayRow(index);
        selection.clear();
      }}
      currentTrackId={currentTrack?.id ?? null}
      isPlaying={isPlaying}
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
      onRemoveFromPlaylist={async (playlistId, trackId) => {
        try {
          await removeTrackFromPlaylist(playlistId, trackId);
        } catch (err) {
          console.error("[LibraryView] remove from playlist failed", err);
        }
      }}
      onCreatePlaylist={(trackId) => {
        setPendingSourceForCreate({ kind: "tracks", ids: [trackId] });
        setIsCreatePlaylistModalOpen(true);
      }}
      onNavigateToAlbum={onNavigateToAlbum}
      onNavigateToArtist={onNavigateToArtist}
      onContextMenuRow={trackContextMenu.open}
      onRowMenuKey={trackContextMenu.openFromKeyboard}
      isSelected={selection.isSelected}
      selectionActive={selection.checkboxMode}
      onToggleSelect={selection.toggleOne}
      onNavigateToRemoteAlbum={onNavigateToRemoteAlbum}
      onNavigateToRemoteArtist={onNavigateToRemoteArtist}
      downloadingRemote={downloadingRemote}
      downloadedRemote={downloadedRemote}
      onDownloadRemote={(remoteTrackId) => {
        setDownloadingRemote((prev) => new Set(prev).add(remoteTrackId));
        void remoteDownloadTrack(remoteTrackId)
          .then(() => {
            setDownloadedRemote((prev) => new Set(prev).add(remoteTrackId));
          })
          .catch((err) => {
            console.error("[LibraryView] download failed", err);
          })
          .finally(() => {
            setDownloadingRemote((prev) => {
              const next = new Set(prev);
              next.delete(remoteTrackId);
              return next;
            });
          });
      }}
      onImportRemote={(row) => {
        setImportTargets({
          ids: [String(row.id)],
          label: row.title,
        });
      }}
      onEditRemoteTags={setRemoteTagsTrackId}
      singleClickPlay={singleClickPlay}
      onRowSelect={(track, e) => {
        // Modifier-driven selection always wins so multi-select
        // remains accessible even with single-click play on.
        // Selection, and everything it feeds, speaks in local
        // rowids. The table only hands us local rows here — a remote
        // one has no `Track` to pass — so the list it ranges over is
        // narrowed to match.
        const localRows = rows
          .filter((row) => row.source === "local")
          .map(toLocalTrack);
        if (e.shiftKey) {
          selection.selectRange(track.id, localRows);
          return;
        }
        if (e.ctrlKey || e.metaKey) {
          selection.toggleOne(track.id);
          return;
        }
        // The same test every list of tracks applies: a plain click on
        // the row, so Alt+click does not play here either.
        if (isPlayClick(e, singleClickPlay)) {
          const idx = rows.findIndex(
            (row) => row.source === "local" && Number(row.id) === track.id,
          );
          if (idx >= 0) onPlayRow(idx);
          selection.clear();
          return;
        }
        // The second click of a double-click: the first one already
        // started the track, and the row's `dblclick` stands down in
        // this mode — one gesture, one play, not three.
        if (singleClickPlay && e.detail > 1) return;
        selection.setSingle(track.id);
      }}
    />
  );

  return (
    <div className="space-y-6 animate-fade-in pb-12">
      {/* Header */}
      <div data-page-hero className="flex items-start justify-between">
        <div className="flex items-center space-x-5">
          <div className="w-20 h-20 rounded-2xl bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400 flex items-center justify-center shadow-sm">
            <Music2 size={40} />
          </div>
          <div>
            <h1 className="text-3xl md:text-4xl font-bold mb-1 text-zinc-900 dark:text-white">
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
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-4 py-2 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm shadow-emerald-500/30 disabled:opacity-60 disabled:cursor-not-allowed"
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
                disabled={libraries.length === 0 || isAnyRescanActive}
                aria-label={t("library.actions.rescan")}
                aria-busy={isRescanning}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {isRescanning ? (
                  <Loader2 size={18} className="animate-spin" />
                ) : (
                  <RefreshCcw size={18} />
                )}
              </button>
            </Tooltip>
            {/* Deep pass, mirroring the per-folder button in the folder
                list. Separate control rather than a modifier on the one
                above: it is markedly slower, so it should be chosen, not
                triggered by accident. */}
            <Tooltip
              label={
                isDeepRescanning
                  ? t("library.actions.deepRescanning")
                  : t("library.actions.deepRescan")
              }
            >
              <button
                type="button"
                onClick={handleDeepRescanLibrary}
                disabled={libraries.length === 0 || isAnyRescanActive}
                aria-label={t("library.actions.deepRescan")}
                aria-busy={isDeepRescanning}
                className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <FileSearch
                  size={18}
                  className={isDeepRescanning ? "animate-pulse" : ""}
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
            disabled={activeTab !== "tracks"}
            className={`p-1.5 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
              tracksView === "list" && activeTab === "tracks"
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
            disabled={activeTab !== "tracks"}
            className={`p-1.5 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
              tracksView === "compact" && activeTab === "tracks"
                ? "bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white"
                : "text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800"
            }`}
          >
            <AlignJustify size={18} />
          </button>
          {/* Beside the density toggle, because both answer "how should
              this list look" -- and only on the tabs that render the
              track table, since on an album or artist grid the control
              would have nothing to configure. */}
          {(activeTab === "tracks" ||
            // The folders tab shows the table only once a folder is
            // open; above that it is a list of directories, and a
            // column picker there configures nothing on screen.
            (activeTab === "folders" && folderPath != null) ||
            activeTab === "needs-attention") && (
            <ColumnPicker
              layout={trackColumns.layout}
              tagKeys={tagKeys}
              onToggle={(id) => {
                void trackColumns.toggle(id);
              }}
              onReorder={(from, to) => {
                void trackColumns.move(from, to);
              }}
              onResetWidths={() => {
                void trackColumns.resetWidths();
              }}
              t={t}
            />
          )}
        </div>
      </div>

      {/* Outside the content gate on purpose. Narrowing to a source that has
          nothing yet empties the list, and a control that disappears with the
          content it emptied leaves no way back. The sort dropdown has no such
          problem — it did not cause the emptiness — so it stays gated. */}
      {(activeTab === "tracks" ||
        activeTab === "albums" ||
        activeTab === "artists" ||
        activeTab === "playlists") && (
        <div className="flex items-center justify-end space-x-3 -mt-4">
          <SourceFilter
            current={librarySource.source}
            onChange={librarySource.setSource}
            t={t}
          />
          {activeTab === "tracks" && tracks.length > 0 && (
            <SortDropdown
              options={trackSortOptions(t)}
              current={tracksSort.sort}
              onChange={tracksSort.setSort}
              t={t}
            />
          )}
          {activeTab === "albums" && albums.length > 0 && (
            <SortDropdown
              options={albumSortOptions(t)}
              current={albumsSort.sort}
              onChange={albumsSort.setSort}
              t={t}
            />
          )}
          {activeTab === "playlists" && libraryPlaylists.length > 0 && (
            <SortDropdown
              options={playlistSortOptions(t)}
              current={playlistsSort.sort}
              onChange={playlistsSort.setSort}
              t={t}
            />
          )}
          {activeTab === "artists" && artists.length > 0 && (
            <SortDropdown
              options={artistSortOptions(t)}
              current={artistsSort.sort}
              onChange={artistsSort.setSort}
              t={t}
            />
          )}
        </div>
      )}

      {hasContent ? (
        <Suspense fallback={<LibraryTabSkeleton tab={activeTab} t={t} />}>
          {activeTab === "tracks" && (
            <>{renderTrackTable(tracks, loading.tracks, playRow)}</>
          )}
          {activeTab === "albums" && (
            <>
              <AlbumGrid
                albums={albums}
                isLoading={loading.albums}
                t={t}
                playlists={playlists}
                onAddToPlaylist={(playlistId, albumId) =>
                  addSourceToPlaylist(playlistId, "album", albumId)
                }
                onCreatePlaylist={(albumId) => {
                  setPendingSourceForCreate({ kind: "album", id: albumId });
                  setIsCreatePlaylistModalOpen(true);
                }}
                onAlbumClick={onNavigateToAlbum}
                onRemoteAlbumClick={onNavigateToRemoteAlbum}
                onChangeCover={(albumId) => setCoverPickerAlbumId(albumId)}
              />
            </>
          )}
          {activeTab === "artists" && (
            <>
              <div className="relative">
                <ArtistList
                  artists={artists}
                  isLoading={loading.artists}
                  t={t}
                  playlists={playlists}
                  onAddToPlaylist={(playlistId, artistId) =>
                    addSourceToPlaylist(playlistId, "artist", artistId)
                  }
                  onCreatePlaylist={(artistId) => {
                    setPendingSourceForCreate({ kind: "artist", id: artistId });
                    setIsCreatePlaylistModalOpen(true);
                  }}
                  onArtistClick={onNavigateToArtist}
                  onRemoteArtistClick={onNavigateToRemoteArtist}
                  scrollToIndexRef={artistScrollToIndexRef}
                />
                {artistsSort.sort.orderBy === "name" && artists.length > 0 && (
                  <AlphabetIndex
                    items={artists}
                    onLetterClick={(idx) => {
                      artistScrollToIndexRef.current?.(idx);
                    }}
                    className="hidden md:flex fixed right-6 top-1/2 -translate-y-1/2 z-30 bg-white/80 dark:bg-zinc-900/70 backdrop-blur-sm wf-glass rounded-full py-2 px-1.5 shadow-sm"
                  />
                )}
              </div>
            </>
          )}
          {activeTab === "genres" && (
            <GenreList
              genres={genres}
              isLoading={loading.genres}
              t={t}
              onSelect={onNavigateToGenre}
            />
          )}
          {activeTab === "years" && (
            <YearList
              years={years}
              isLoading={loading.years}
              t={t}
              onSelect={onNavigateToYear}
            />
          )}
          {activeTab === "playlists" && (
            <>
              <PlaylistGrid
                playlists={libraryPlaylists}
                sort={playlistsSort.sort}
                onOpen={onNavigateToPlaylist}
                onOpenRemote={onNavigateToRemotePlaylist}
                sourceFiltered={librarySource.source !== "all"}
              />
            </>
          )}
          {activeTab === "folders" && folderPath != null && (
            <>
              <FolderBrowser
                listing={
                  folderListing?.path === folderPath ? folderListing : null
                }
                isLoading={folderBusy}
                density={folderDensity}
                onDensity={setFolderDensity}
                t={t}
                locale={i18n.resolvedLanguage ?? i18n.language}
                onOpen={setFolderPath}
                onRoots={() => setFolderPath(null)}
                playlists={playlists}
                onPlay={(path) => void playFolder(path)}
                onQueue={(path) => {
                  void idsUnderFolder(path).then((ids) => {
                    if (ids.length > 0) void playerAddToQueue(ids);
                  });
                }}
                onPlayNext={(path) => {
                  void idsUnderFolder(path).then((ids) => {
                    if (ids.length > 0) void playerPlayNext(ids);
                  });
                }}
                onAddToPlaylist={(playlistId, path) => {
                  void idsUnderFolder(path).then((ids) => {
                    if (ids.length === 0) return;
                    void addTracksToPlaylist(playlistId, ids).catch(
                      (err: unknown) => {
                        console.error(
                          "[LibraryView] add folder to playlist failed",
                          err,
                        );
                      },
                    );
                  });
                }}
                onCreatePlaylist={(path) => {
                  void idsUnderFolder(path).then((ids) => {
                    if (ids.length === 0) return;
                    setPendingSourceForCreate({ kind: "tracks", ids });
                    setIsCreatePlaylistModalOpen(true);
                  });
                }}
                onBatchTag={(path) => {
                  void idsUnderFolder(path).then((ids) => {
                    if (ids.length > 0) setBatchTagIds(ids);
                  });
                }}
              />
              {/* The files directly inside, in the library's own table --
                  same columns, same sort, same context menu. */}
              {folderListing?.path === folderPath &&
                (folderTracks.length > 0 || folderBusy) &&
                renderTrackTable(folderTracks, folderBusy, playFolderRow)}
            </>
          )}
          {activeTab === "needs-attention" && (
            <div className="space-y-4">
              <InventoryCategories
                categories={inventory}
                isLoading={loading["needs-attention"]}
                activeKey={inventoryCategory}
                onSelect={(key) => {
                  // Clicking the open category closes it, so the user
                  // gets back to the overview without a second control.
                  const next = inventoryCategory === key ? null : key;
                  // The rows belong to the category that was open, and
                  // the query replacing them runs over the whole
                  // library. Left in place, the table spends that time
                  // showing one category's tracks under another's name
                  // -- and a click plays them.
                  setInventoryRows([]);
                  setInventoryCategory(next);
                }}
                t={t}
              />
              {/* The library's own table, so a category is an entry
                  point and not a report: same columns, same sort, same
                  context menu, same properties modal -- which is what
                  makes fixing a track from here possible at all. */}
              {inventoryCategory === "phantom_artist" && (
                <PhantomArtistList
                  refreshKey={editRefetch}
                  onChanged={() => setEditRefetch((k) => k + 1)}
                  t={t}
                />
              )}
              {inventoryCategory != null &&
                inventoryCategory !== "phantom_artist" &&
                renderTrackTable(inventoryRows, inventoryBusy, (index) => {
                  void playTracks(inventoryRows.map(toLocalTrack), index, {
                    type: "library",
                    id: null,
                  });
                })}
            </div>
          )}
          {activeTab === "folders" && folderPath == null && (
            <FolderList
              folders={folders}
              isLoading={loading.folders}
              t={t}
              playlists={playlists}
              onAddToPlaylist={(playlistId, folderId) =>
                addSourceToPlaylist(playlistId, "folder", folderId)
              }
              onCreatePlaylist={(folderId) => {
                setPendingSourceForCreate({ kind: "folder", id: folderId });
                setIsCreatePlaylistModalOpen(true);
              }}
              onRemove={(folderId) => {
                // Optimistic removal — the backend cascade-deletes
                // tracks too, so the LibraryContext will refresh on
                // the `library:rescanned` event the command emits.
                setFolders((prev) => prev.filter((f) => f.id !== folderId));
                removeFolderFromLibrary(folderId).catch((err) => {
                  console.error("[LibraryView] remove folder failed", err);
                });
              }}
              onOpen={setFolderPath}
              onDeepRescan={handleDeepRescanFolder}
              deepRescanFolderId={deepRescanFolderId}
              isAnyRescanActive={isAnyRescanActive}
              onToggleWatched={(folderId, enable) => {
                // Optimistic flip — the watcher hookup is fire-and-
                // forget on the backend so the UI shouldn't block on it.
                setFolders((prev) =>
                  prev.map((f) =>
                    f.id === folderId
                      ? { ...f, is_watched: enable ? 1 : 0 }
                      : f,
                  ),
                );
                setFolderWatched(folderId, enable).catch((err) => {
                  console.error("[LibraryView] toggle watched failed", err);
                  // Roll back on error.
                  setFolders((prev) =>
                    prev.map((f) =>
                      f.id === folderId
                        ? { ...f, is_watched: enable ? 0 : 1 }
                        : f,
                    ),
                  );
                });
              }}
            />
          )}
        </Suspense>
      ) : loading[activeTab] ? (
        // First-load skeleton — keeps the layout occupied while the
        // initial SQL query lands, instead of flashing the "No X
        // found" EmptyState for the duration of the fetch.
        <LibraryTabSkeleton tab={activeTab} t={t} />
      ) : (
        <EmptyState
          icon={<EmptyIcon size={40} />}
          title={t(`library.empty.${activeTab}.title`)}
          // A narrowed source is a different emptiness: the library may be
          // full and the answer is not to import a folder, which would add
          // nothing to the half being looked at.
          description={t(
            sourceFilterEmptied
              ? "library.empty.sourceFiltered.description"
              : `library.empty.${activeTab}.description`,
          )}
          className="py-20"
        >
          {!sourceFilterEmptied && (
            <div className="mt-8 flex items-center flex-wrap justify-center gap-4">
              <button
                type="button"
                onClick={handleImport}
                disabled={isImporting}
                className="bg-emerald-500 hover:bg-emerald-600 text-white px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm disabled:opacity-60 disabled:cursor-not-allowed"
              >
                <Folder size={18} />
                <span>{t("library.actions.importFolder")}</span>
              </button>
            </div>
          )}
        </EmptyState>
      )}

      {trackContextMenu.render()}

      {/* Batch tag editor, scoped to a folder: the selection someone
          wants to act on is usually exactly one folder (#578). */}
      <BatchTagEditModal
        trackIds={batchTagIds}
        onClose={() => setBatchTagIds(null)}
      />
      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => {
          setIsCreatePlaylistModalOpen(false);
          setPendingSourceForCreate(null);
        }}
        onCreate={async (data) => {
          try {
            const created = await createFromModal(data);
            const pending = pendingSourceForCreate;
            if (pending && created?.id != null) {
              try {
                if (pending.kind === "tracks") {
                  await addTracksToPlaylist(created.id, pending.ids);
                } else {
                  await addSourceToPlaylist(
                    created.id,
                    pending.kind,
                    pending.id,
                  );
                }
              } catch (err) {
                // The playlist already exists. Return its id so the modal
                // does not offer a retry that would create a duplicate.
                console.error(
                  "[LibraryView] add tracks to new playlist failed",
                  err,
                );
              }
            }
            return created;
          } catch (err) {
            console.error("[LibraryView] create playlist failed", err);
            throw err;
          } finally {
            setPendingSourceForCreate(null);
          }
        }}
      />

      <ImportToLibraryModal
        isOpen={importTargets !== null}
        onClose={() => setImportTargets(null)}
        trackIds={importTargets?.ids ?? []}
        label={importTargets?.label ?? ""}
        onImported={() => {
          // The imported files are local tracks now, so every list this view
          // shows is one row out of date. Same refetch a tag edit triggers,
          // rather than trusting the backend's `library:rescanned` to move
          // `librariesSignature` — it only does when a library row itself
          // changed.
          setEditRefetch((k) => k + 1);
        }}
      />

      {/* Re-keyed on the identifier, so closing and reopening mounts a
          fresh dialog instead of one still holding the previous form.
          Without it, reopening the SAME track keeps `loadedId` equal to
          `trackId`, the form reads as ready while its reload is still in
          flight, and a quick Save would send stale values — which under a
          wholesale patch withdraws whatever changed in between. Same
          strategy the Properties dialog uses. */}
      <RemoteTrackTagsModal
        key={remoteTagsTrackId ?? "none"}
        trackId={remoteTagsTrackId}
        onClose={() => setRemoteTagsTrackId(null)}
        onSaved={() => {
          // The mirror row was rewritten in the same transaction that
          // queued the patch, so the list is one row out of date the
          // moment the modal closes. Same refetch an import triggers.
          setEditRefetch((k) => k + 1);
        }}
      />

      {coverPickerAlbumId != null && (
        <CoverPickerModal
          albumId={coverPickerAlbumId}
          initialQuery={(() => {
            // Only a local album has a cover picker, and only a local id is
            // a rowid — matching on the text id alone would find a server
            // album whose UUID happened to read as a number.
            const a = albums.find(
              (al) =>
                al.source === "local" && Number(al.id) === coverPickerAlbumId,
            );
            if (!a) return "";
            return a.artist_name ? `${a.title} ${a.artist_name}` : a.title;
          })()}
          isOpen={coverPickerAlbumId != null}
          onClose={() => setCoverPickerAlbumId(null)}
          onSuccess={() => setCoverReloadKey((k) => k + 1)}
        />
      )}

      {listedTracks != null && (
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

function playlistSortOptions(t: Translator): SortOption[] {
  return [
    { value: "custom", label: t("sort.customOrder") },
    { value: "name", label: t("sort.name") },
    { value: "tracks", label: t("sort.tracksCount") },
    { value: "duration", label: t("sort.duration") },
    { value: "updated", label: t("sort.updatedAt") },
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

interface SourceFilterProps {
  current: LibrarySourceFilter;
  onChange: (next: LibrarySourceFilter) => void;
  t: Translator;
}

/**
 * Which half of the library to show — a filter *inside* the list, which is
 * the whole difference between a unified library and two tabs.
 *
 * Renders nothing when no server is bound: a filter whose second option is
 * permanently empty is worse than no filter, and most profiles are local-only.
 */
function SourceFilter({ current, onChange, t }: SourceFilterProps) {
  const remote = useRemoteSource();
  if (!remote.available) return null;

  const options: { id: LibrarySourceFilter; label: string }[] = [
    { id: "all", label: t("library.source.all") },
    { id: "local", label: t("library.source.local") },
    // The server's own name when we know it — "Serveur" is what it is, but
    // the name is what the user recognises.
    { id: "remote", label: remote.serverName ?? t("library.source.remote") },
  ];

  return (
    <div
      role="group"
      aria-label={t("library.source.label")}
      className="inline-flex items-center rounded-lg border border-zinc-200 dark:border-zinc-700 p-0.5 text-xs"
    >
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          onClick={() => onChange(option.id)}
          aria-pressed={current === option.id}
          className={`px-2.5 py-1 rounded-md transition-colors max-w-40 truncate ${
            current === option.id
              ? "bg-zinc-100 dark:bg-zinc-700 text-zinc-900 dark:text-zinc-100 font-medium"
              : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-800 dark:hover:text-zinc-200"
          }`}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
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
                    onChange({
                      orderBy: opt.value,
                      direction: current.direction,
                    });
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
                  ? t("sort.ascending")
                  : t("sort.descending")}
              </span>
              {current.direction === "asc" ? (
                <ArrowUp size={14} />
              ) : (
                <ArrowDown size={14} />
              )}
            </button>
          </li>
        </ul>
      )}
    </div>
  );
}

function LibraryTabSkeleton({ tab, t }: { tab: LibraryTab; t: Translator }) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  // Screen readers announce "Loading <tab name>…" via role=status. The
  // name is fed from the existing tab label so we don't fork a second
  // copy in every locale.
  const ariaLabel = t("library.skeletonAriaLabel", {
    name: t(`library.tabs.${tab}`),
  });
  if (tab === "tracks") {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={ariaLabel}
        className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden animate-pulse"
      >
        {Array.from({ length: 12 }).map((_, i) => (
          <div
            key={i}
            className="grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_7rem_5rem_2rem_5.5rem] gap-4 px-5 h-14 items-center border-b border-zinc-100 dark:border-zinc-800/60"
          >
            <div className={`h-3 w-4 rounded ${tile} justify-self-end`} />
            <div className={`w-10 h-10 rounded-md ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 w-10 rounded ${tile} justify-self-end`} />
            <div className={`h-3 w-3 rounded ${tile} justify-self-center`} />
            <div />
          </div>
        ))}
      </div>
    );
  }
  if (tab === "albums") {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={ariaLabel}
        className="grid gap-5 animate-pulse"
        style={{ gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))" }}
      >
        {Array.from({ length: 18 }).map((_, i) => (
          <div key={i} className="space-y-3">
            <div className={`aspect-square rounded-xl ${tile}`} />
            <div className={`h-3 rounded ${tile}`} />
            <div className={`h-3 w-2/3 rounded ${tile}`} />
          </div>
        ))}
      </div>
    );
  }
  if (tab === "artists") {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={ariaLabel}
        className="grid gap-5 animate-pulse"
        style={{ gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))" }}
      >
        {Array.from({ length: 18 }).map((_, i) => (
          <div key={i} className="flex flex-col items-center space-y-3">
            <div className={`aspect-square w-full rounded-full ${tile}`} />
            <div className={`h-3 w-2/3 rounded ${tile}`} />
            <div className={`h-3 w-1/2 rounded ${tile}`} />
          </div>
        ))}
      </div>
    );
  }
  // Years lay out as the same grid of tiles.
  if (tab === "genres" || tab === "years") {
    return (
      <div
        role="status"
        aria-busy="true"
        aria-label={ariaLabel}
        className="grid gap-4 animate-pulse"
        style={{ gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))" }}
      >
        {Array.from({ length: 12 }).map((_, i) => (
          <div
            key={i}
            className="flex items-center space-x-3 p-4 rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40"
          >
            <div className={`w-12 h-12 rounded-xl ${tile} shrink-0`} />
            <div className="flex-1 space-y-2">
              <div className={`h-3 w-2/3 rounded ${tile}`} />
              <div className={`h-3 w-1/3 rounded ${tile}`} />
            </div>
          </div>
        ))}
      </div>
    );
  }
  // folders
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={ariaLabel}
      className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 divide-y divide-zinc-100 dark:divide-zinc-800/60 animate-pulse"
    >
      {Array.from({ length: 5 }).map((_, i) => (
        <div key={i} className="flex items-center space-x-4 p-4">
          <div className={`w-10 h-10 rounded-lg ${tile} shrink-0`} />
          <div className="flex-1 space-y-2">
            <div className={`h-3 w-1/2 rounded ${tile}`} />
            <div className={`h-3 w-1/3 rounded ${tile}`} />
          </div>
        </div>
      ))}
    </div>
  );
}
