import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { PlaylistGrid } from "./library/PlaylistGrid";
import { InventoryCategories } from "./library/InventoryCategories";
import { TrackTableHeader } from "./library/TrackTableHeader";
import { ColumnPicker } from "./library/ColumnPicker";
import { useTrackColumns } from "../../hooks/useTrackColumns";
import {
  listTrackTagKeys,
  listTrackTagValues,
} from "../../lib/tauri/trackTags";
import {
  cellText,
  MAX_COLUMN_WIDTH,
  specFor,
  tagKeyOf,
  trackMinWidthFor,
  trackSizeFor,
  type ColumnId,
  type ColumnLayout,
} from "../../lib/trackColumns";
import { fitWidth, styleOf } from "../../lib/measureText";
import {
  inventorySummary,
  inventoryTracks,
  type InventoryCategory,
} from "../../lib/tauri/inventory";
import { useLibraryPlaylists } from "../../hooks/useLibraryPlaylists";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Music2,
  Disc,
  Mic2,
  Tags,
  Folder,
  AlertTriangle,
  RefreshCcw,
  FileSearch,
  ListMusic,
  LayoutList,
  AlignJustify,
  Plus,
  Heart,
  Eye,
  EyeOff,
  Trash2,
  ImageIcon,
  ArrowUpDown,
  ArrowDown,
  ArrowUp,
  Check,
  Download,
  FolderDown,
  Pencil,
  ChevronRight,
  CornerLeftUp,
  LayoutGrid,
  Play,
  ListEnd,
  TagsIcon,
  Loader2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { RemoteArtwork } from "../common/RemoteArtwork";
import { remotePlayTracks } from "../../lib/tauri/remoteServer";
import { useRemoteArtworkSrc } from "../../hooks/useRemoteArtworkSrc";
import { useRemoteSource } from "../../hooks/useRemoteSource";
import {
  useLibrarySource,
  type LibrarySourceFilter,
} from "../../hooks/useLibrarySource";
import { EmptyState } from "../common/EmptyState";
import { Artwork } from "../common/Artwork";
import { AlbumLink } from "../common/AlbumLink";
import { ArtistLink } from "../common/ArtistLink";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { Tooltip } from "../common/Tooltip";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { CoverPickerModal } from "../common/CoverPickerModal";
import {
  remoteDownloadTrack,
  remoteListDownloads,
} from "../../lib/tauri/remoteServer";
import { ImportToLibraryModal } from "../common/ImportToLibraryModal";
import { RemoteTrackTagsModal } from "../common/RemoteTrackTagsModal";
import { StarRating } from "../common/StarRating";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { AlphabetIndex } from "../common/AlphabetIndex";
import { useSortMemory } from "../../hooks/useSortMemory";
import { usePageScroll } from "../../hooks/usePageScroll";
import { getProfileSetting } from "../../lib/tauri/profile";
import { useLibrary } from "../../hooks/useLibrary";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useRemoteTrackContextMenu } from "../../hooks/useRemoteTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import { resolvePlaylistColor } from "../../lib/playlistVisuals";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { FadeInImage } from "../common/FadeInImage";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import {
  listPlaylistsContainingTrack,
  type Playlist,
} from "../../lib/tauri/playlist";
import { pickFolder } from "../../lib/tauri/dialog";
import {
  removeFolderFromLibrary,
  countFolderPlayEvents,
  setFolderWatched,
  scanFolder,
} from "../../lib/tauri/library";
import {
  formatDuration,
  listLikedTrackIds,
  setTrackRating,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  browseFolders,
  listFolderTracks,
  folderTrackIds,
  type FolderListing,
  type FolderNode,
  listGenres,
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
import { MenuActionItem } from "../common/MenuActionItem";
import { playerAddToQueue, playerPlayNext } from "../../lib/tauri/player";
import { formatBytes } from "../../lib/format";

/** View density for the tracks list: `list` shows cover art, `compact` doesn't. */
type TracksView = "list" | "compact";

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
  onNavigateToPlaylist: (playlistId: number) => void;
}

type Translator = (key: string, options?: Record<string, unknown>) => string;

const tabConfig: { id: LibraryTab; icon: typeof Music2 }[] = [
  { id: "morceaux", icon: Music2 },
  { id: "albums", icon: Disc },
  { id: "artistes", icon: Mic2 },
  { id: "genres", icon: Tags },
  { id: "playlists", icon: ListMusic },
  { id: "dossiers", icon: Folder },
  { id: "a-corriger", icon: AlertTriangle },
];

const emptyStateIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  playlists: ListMusic,
  dossiers: Folder,
  "a-corriger": AlertTriangle,
};

const headerIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  playlists: ListMusic,
  dossiers: Folder,
  "a-corriger": AlertTriangle,
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
  onNavigateToPlaylist,
}: LibraryViewProps) {
  const { t, i18n } = useTranslation();
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
    morceaux: true,
    albums: true,
    artistes: true,
    genres: true,
    // Playlists come from PlaylistContext, already loaded — there is no
    // fetch of our own to wait on, so this tab never shows a skeleton.
    playlists: false,
    dossiers: true,
    // The only tab that does NOT prefetch. Its counts are album-level
    // aggregates plus a walk over every track to chain probable
    // duplicates, so paying for them on every mount of the library --
    // which is what the other six do -- would tax people who never open
    // it. That is about the *fetch*, not about this flag: `true`,
    // because an inventory nobody has asked for yet is an empty list,
    // and an empty list is what "nothing to fix" looks like. The first
    // paint would congratulate the user before a single count existed.
    "a-corriger": true,
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

  // Re-fetch when any library's updated_at changes (e.g. after a scan).
  const librariesSignature = libraries
    .map((l) => `${l.id}:${l.updated_at}`)
    .join(",");
  // Bumped when a tag edit elsewhere fires `track:updated` so the
  // active tab re-fetches and shows the new metadata.
  const [editRefetch, setEditRefetch] = useState(0);
  // `setEditRefetch` is listed even though a state setter is stable:
  // the React Compiler infers it as a dependency, and a manual list
  // that does not match makes it skip optimizing the whole component.
  useTrackUpdated(
    useCallback(() => setEditRefetch((k) => k + 1), [setEditRefetch]),
  );
  const clearSelection = selection.clear;
  // Also on `folderPath`: a selection is a set of track ids, and the
  // action bar it feeds would otherwise act on tracks the user can no
  // longer see after walking into another folder (#578).
  useEffect(() => {
    clearSelection();
  }, [activeTab, folderPath, clearSelection]);

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
    if (activeTab !== "a-corriger") return;
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading((p) => ({ ...p, "a-corriger": true }));
    inventorySummary()
      .then((list) => {
        if (!cancelled) setInventory(list);
      })
      .catch((err) => {
        if (!cancelled)
          console.error("[LibraryView] inventorySummary failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, "a-corriger": false }));
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
    if (activeTab !== "a-corriger" || inventoryCategory == null) return;
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
    setLoading((p) => ({ ...p, morceaux: true }));
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
        if (!cancelled) setLoading((p) => ({ ...p, morceaux: false }));
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
    setLoading((p) => ({ ...p, artistes: true }));
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
        if (!cancelled) setLoading((p) => ({ ...p, artistes: false }));
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
    setLoading((p) => ({ ...p, dossiers: true }));
    listFolders(null)
      .then((list) => {
        if (!cancelled) setFolders(list);
      })
      .catch((err) => {
        if (!cancelled) console.error("[LibraryView] listFolders failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading((p) => ({ ...p, dossiers: false }));
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
      case "morceaux":
        return tracks.length;
      case "albums":
        return albums.length;
      case "artistes":
        return artists.length;
      case "genres":
        return genres.length;
      case "playlists":
        // The merged list, not the local one: the header would otherwise
        // count a different set from the grid right below it.
        return libraryPlaylists.length;
      case "dossiers":
        return folders.length;
      case "a-corriger":
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
  // `library.header.subtext.a-corriger` finds nothing and proves
  // nothing. Each tab's entry is in all 17 locale files, and as CLDR
  // plural forms (`_zero` / `_one` / `_other`, plus `_few` and `_many`
  // where the language has them), because `count` is passed.
  const headerSubtext =
    activeTab === "dossiers"
      ? t("library.header.subtext.dossiers", { count: countForTab("dossiers") })
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
    ((activeTab === "morceaux" && tracks.length === 0) ||
      (activeTab === "albums" && albums.length === 0) ||
      (activeTab === "artistes" && artists.length === 0));
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
    (activeTab === "morceaux" && tracks.length > 0) ||
    (activeTab === "albums" && albums.length > 0) ||
    (activeTab === "artistes" && artists.length > 0) ||
    (activeTab === "genres" && genres.length > 0) ||
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
    (activeTab === "dossiers" && (folders.length > 0 || folderPath != null)) ||
    // The inventory always has something to show: the categories
    // themselves, including the "nothing to fix" reading, which is the
    // answer to the question the user came here with. Listing it here
    // also keeps `activeTab` from being narrowed out of the branch that
    // renders it — `hasContent` is a `const` built from comparisons, so
    // TypeScript treats it as a discriminant.
    activeTab === "a-corriger";

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
      onPlayTrack={(index) => onPlayRow(index)}
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
        if (singleClickPlay) {
          const idx = rows.findIndex(
            (row) => row.source === "local" && Number(row.id) === track.id,
          );
          if (idx >= 0) onPlayRow(idx);
          selection.clear();
          return;
        }
        selection.setSingle(track.id);
      }}
    />
  );

  return (
    <div className="space-y-6 animate-fade-in pb-12">
      {/* Header */}
      <div className="flex items-start justify-between">
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
          {/* Beside the density toggle, because both answer "how should
              this list look" -- and only on the tabs that render the
              track table, since on an album or artist grid the control
              would have nothing to configure. */}
          {(activeTab === "morceaux" ||
            // The folders tab shows the table only once a folder is
            // open; above that it is a list of directories, and a
            // column picker there configures nothing on screen.
            (activeTab === "dossiers" && folderPath != null) ||
            activeTab === "a-corriger") && (
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
      {(activeTab === "morceaux" ||
        activeTab === "albums" ||
        activeTab === "artistes" ||
        activeTab === "playlists") && (
        <div className="flex items-center justify-end space-x-3 -mt-4">
          <SourceFilter
            current={librarySource.source}
            onChange={librarySource.setSource}
            t={t}
          />
          {activeTab === "morceaux" && tracks.length > 0 && (
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
          {activeTab === "artistes" && artists.length > 0 && (
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
        <>
          {activeTab === "morceaux" && (
            <>{renderTrackTable(tracks, loading.morceaux, playRow)}</>
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
          {activeTab === "artistes" && (
            <>
              <div className="relative">
                <ArtistList
                  artists={artists}
                  isLoading={loading.artistes}
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
          {activeTab === "dossiers" && folderPath != null && (
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
          {activeTab === "a-corriger" && (
            <div className="space-y-4">
              <InventoryCategories
                categories={inventory}
                isLoading={loading["a-corriger"]}
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
              {inventoryCategory != null &&
                renderTrackTable(inventoryRows, inventoryBusy, (index) => {
                  void playTracks(inventoryRows.map(toLocalTrack), index, {
                    type: "library",
                    id: null,
                  });
                })}
            </div>
          )}
          {activeTab === "dossiers" && folderPath == null && (
            <FolderList
              folders={folders}
              isLoading={loading.dossiers}
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
        </>
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
              if (pending.kind === "tracks") {
                await addTracksToPlaylist(created.id, pending.ids);
              } else {
                await addSourceToPlaylist(created.id, pending.kind, pending.id);
              }
            }
          } catch (err) {
            console.error("[LibraryView] create playlist failed", err);
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

// =============================================================================
// Tab-specific list components
// =============================================================================

/**
 * A local library row as the `Track` the player, the selection and the
 * playlist calls all speak.
 *
 * Not a cast. The row's identifiers are **text** — the two sources do not
 * share an identifier type, so the unified listing hands both back as strings
 * — and handing that object straight to code that compares `id` numerically
 * makes every comparison silently false: no row ever reads as selected, and
 * the queue is built from tracks the engine cannot match. Only ever called for
 * a row whose source is local; a server row has no `Track` to become.
 */
function toLocalTrack(row: LibraryTrackRow): Track {
  return {
    id: Number(row.id),
    library_id: row.library_id ?? 0,
    title: row.title,
    album_id: row.album_id != null ? Number(row.album_id) : null,
    album_title: row.album_title,
    artist_id: row.artist_id != null ? Number(row.artist_id) : null,
    artist_name: row.artist_name,
    artist_ids: row.artist_ids,
    duration_ms: row.duration_ms,
    track_number: row.track_number,
    disc_number: row.disc_number,
    year: row.year,
    bitrate: row.bitrate,
    sample_rate: row.sample_rate,
    channels: row.channels,
    bit_depth: row.bit_depth,
    codec: row.codec,
    musical_key: row.musical_key,
    file_path: row.file_path ?? "",
    file_size: row.file_size ?? 0,
    added_at: row.added_at,
    artwork_path: row.artwork_path,
    artwork_path_1x: row.artwork_path_1x,
    artwork_path_2x: row.artwork_path_2x,
    rating: row.rating,
  };
}

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

function TrackTable({
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
  layout,
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
  const leadingTracks = ["3rem", ...(view === "list" ? ["2.75rem"] : [])];
  const trailingTracks = ["2rem", "5.5rem"];
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
      }),
    [locale, tagValues],
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
                if (
                  singleClickPlay &&
                  !e.shiftKey &&
                  !e.ctrlKey &&
                  !e.metaKey
                ) {
                  onPlayTrack(index);
                }
              }}
              onDoubleClick={() => onPlayTrack(index)}
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
                {isCurrent ? (
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
                      className={`p-1.5 rounded-full transition-all focus-visible:opacity-100 ${
                        downloadedRemote.has(String(track.id))
                          ? "opacity-100 text-emerald-500"
                          : downloadingRemote.has(String(track.id))
                            ? "opacity-100 text-zinc-400 animate-pulse"
                            : "opacity-0 group-hover:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
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
                      className="p-1.5 rounded-full transition-all opacity-0 group-hover:opacity-100 focus-visible:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
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
                      className="p-1.5 rounded-full transition-all opacity-0 group-hover:opacity-100 focus-visible:opacity-100 text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700"
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
                      className={`p-1.5 rounded-full transition-all focus-visible:opacity-100 ${
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

interface AddToPlaylistPopoverProps {
  playlists: Playlist[];
  trackId: number;
  onPick: (playlistId: number) => void;
  onCreate: () => void;
  t: Translator;
  /**
   * Optional set of playlist IDs the target is already in. Only meaningful
   * for the track popover — when provided, matching rows render a green
   * checkmark and the caller is expected to toggle (remove) rather than
   * add on click. Albums/artists/folders skip this prop because their
   * "+ to playlist" action is a bulk add with no symmetric remove.
   */
  memberPlaylistIds?: ReadonlySet<number>;
  /**
   * Trigger element the popover anchors to. When provided, the popover
   * is rendered through a portal at `document.body` and positioned via
   * `getBoundingClientRect`, escaping every ancestor stacking context
   * (virtualizer rows use `transform`, which traps `z-index` inside).
   * Required for album / artist grids where the popover would otherwise
   * paint under the row below it.
   */
  anchorEl?: HTMLElement | null;
}

/**
 * Tiny popover anchored to the trigger button. Lists every playlist of
 * the active profile (resolved color tile + name) plus a "create new"
 * shortcut at the bottom. Picking a row calls `onPick(playlistId)`.
 *
 * When `anchorEl` is supplied, the popover is rendered via React portal
 * to `document.body` and positioned absolutely against the anchor's
 * client rect. Without it, the popover falls back to absolute positioning
 * inside its parent — only safe where the parent isn't sitting inside a
 * `transform`-clipped stacking context (TrackTable rows qualify; album /
 * artist grids don't).
 *
 * Stops `onDoubleClick` from bubbling to the parent so clicking a
 * playlist doesn't accidentally start playback of the row underneath.
 */
function AddToPlaylistPopover({
  playlists,
  onPick,
  onCreate,
  t,
  memberPlaylistIds,
  anchorEl,
}: AddToPlaylistPopoverProps) {
  // Portal mode: track the anchor's viewport rect AND the popover's own
  // height so we can flip / clamp against the viewport. `null` rect =
  // first render before the layout effect runs; we keep the popover
  // invisible until we know where it goes so it never flashes at (0,0).
  const POPOVER_WIDTH = 224; // matches `w-56`
  const VIEWPORT_MARGIN = 8;
  const popoverRef = useRef<HTMLDivElement | null>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const [popoverHeight, setPopoverHeight] = useState(0);
  useLayoutEffect(() => {
    if (!anchorEl) return;
    const update = () => setRect(anchorEl.getBoundingClientRect());
    update();
    const ro = new ResizeObserver(update);
    ro.observe(anchorEl);
    window.addEventListener("scroll", update, true);
    window.addEventListener("resize", update);
    return () => {
      ro.disconnect();
      window.removeEventListener("scroll", update, true);
      window.removeEventListener("resize", update);
    };
  }, [anchorEl]);
  // Measure the popover the first time it lays out and on content
  // resize so the flip-above check has a real height. We intentionally
  // do NOT depend on `rect` — scroll updates `rect` many times per
  // second, and re-running this effect would tear down the
  // ResizeObserver and force a synchronous `offsetHeight` reflow each
  // tick. The ResizeObserver already covers every real height change
  // (translated label wrap, scrollable list growth, etc.).
  useLayoutEffect(() => {
    if (!anchorEl) return;
    const el = popoverRef.current;
    if (!el) return;
    setPopoverHeight(el.offsetHeight);
    const ro = new ResizeObserver(() => setPopoverHeight(el.offsetHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, [anchorEl]);

  // Compute placement: prefer below, flip above when below would clip,
  // then clamp horizontally so the first-column trigger doesn't push
  // the popover off the left edge.
  const placement = (() => {
    if (!anchorEl || !rect) return null;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    let top = rect.bottom + 4;
    if (
      popoverHeight > 0 &&
      top + popoverHeight > vh - VIEWPORT_MARGIN &&
      rect.top - 4 - popoverHeight >= VIEWPORT_MARGIN
    ) {
      top = rect.top - 4 - popoverHeight;
    }
    top = Math.max(
      VIEWPORT_MARGIN,
      Math.min(top, vh - popoverHeight - VIEWPORT_MARGIN),
    );
    let left = rect.right - POPOVER_WIDTH;
    left = Math.max(
      VIEWPORT_MARGIN,
      Math.min(left, vw - POPOVER_WIDTH - VIEWPORT_MARGIN),
    );
    return { top, left };
  })();

  const inner = (
    <div
      ref={popoverRef}
      data-add-to-playlist-popover
      role="menu"
      // Stop click + double-click + mousedown from bubbling to the
      // album / artist tile underneath. Portals re-parent the DOM but
      // React events still bubble through the React tree, so without
      // this picking a playlist would also navigate to the album.
      onClick={(e) => e.stopPropagation()}
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={
        anchorEl
          ? placement
            ? {
                position: "fixed",
                top: placement.top,
                left: placement.left,
                width: POPOVER_WIDTH,
              }
            : { position: "fixed", visibility: "hidden" }
          : undefined
      }
      className={`${
        anchorEl ? "z-100" : "absolute top-full right-0 mt-1 z-50 w-56"
      } rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden animate-fade-in`}
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
            const isMember = memberPlaylistIds?.has(pl.id) ?? false;
            return (
              <button
                key={pl.id}
                type="button"
                role="menuitem"
                aria-label={
                  isMember
                    ? t("trackActions.removeFromPlaylistNamed", {
                        name: pl.name,
                      })
                    : t("trackActions.addToPlaylistNamed", { name: pl.name })
                }
                onClick={() => onPick(pl.id)}
                className="w-full flex items-center space-x-2 p-2 rounded-lg text-left hover:bg-zinc-50 dark:hover:bg-zinc-700/30 transition-colors"
              >
                <div
                  className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
                >
                  <PlaylistIcon iconId={pl.icon_id} size={14} />
                </div>
                <span className="flex-1 text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                  {pl.name}
                </span>
                {isMember && (
                  <Check
                    size={14}
                    className="shrink-0 text-emerald-500"
                    aria-hidden="true"
                  />
                )}
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
  return anchorEl ? createPortal(inner, document.body) : inner;
}

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

function AlbumGrid({
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
  const [contextMenu, setContextMenu] = useState<{
    albumId: number;
    x: number;
    y: number;
  } | null>(null);

  // Virtual-grid plumbing — without this a 800-album library mounts 800
  // <Artwork> components on every tab switch, blowing the main thread
  // for ~1 s before the first paint.
  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(1);
  const [tileWidth, setTileWidth] = useState(180);
  const [scrollMargin, setScrollMargin] = useState(0);

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
        onContextMenu={(e) => {
          if (localId === null) return;
          e.preventDefault();
          setContextMenu({
            albumId: localId,
            x: e.clientX,
            y: e.clientY,
          });
        }}
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
              // `focus-visible:opacity-100` is not decoration: without it the
              // button is invisible exactly when the keyboard reaches it.
              className={`absolute bottom-2 right-2 p-1.5 rounded-full shadow-sm transition-all focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500 ${
                isMenuOpen
                  ? "opacity-100 bg-emerald-500 text-white"
                  : "opacity-0 group-hover:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
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
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${row.start - scrollMargin}px)`,
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
      {contextMenu &&
        createPortal(
          // Portaled to <body>: the menu uses viewport `fixed` coords,
          // but an ancestor album card gets a `backdrop-filter` under
          // the Lounge / Liquid skins, which would otherwise become the
          // containing block and trap / mis-stack the menu.
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
          </div>,
          document.body,
        )}
    </>
  );
}

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
      // dark portraits (#106). The placeholder bg gradient is fine because
      // `object-cover` fully covers it once the image decodes.
      wrapperClassName="w-full aspect-square rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 shadow-sm group-hover:shadow-md transition-shadow"
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

function ArtistList({
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
              className={`absolute bottom-1 right-1 p-1.5 rounded-full shadow-sm transition-all focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-emerald-500 ${
                isMenuOpen
                  ? "opacity-100 bg-emerald-500 text-white"
                  : "opacity-0 group-hover:opacity-100 bg-white/90 dark:bg-zinc-800/90 text-zinc-600 dark:text-zinc-300 hover:bg-emerald-500 hover:text-white"
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
            artist.source === "local" && Number(artist.id) === openMenuArtistId,
        );
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
  );
}

interface GenreListProps {
  genres: GenreRow[];
  isLoading: boolean;
  t: Translator;
  onSelect: (genreId: number) => void;
}

function GenreList({ genres, isLoading, t, onSelect }: GenreListProps) {
  return (
    <div
      className={`grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-4 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {genres.map((genre) => (
        <button
          type="button"
          key={genre.id}
          onClick={() => onSelect(genre.id)}
          className="flex items-center space-x-3 p-4 rounded-2xl border border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-800/40 dark:hover:bg-zinc-800/70 transition-colors cursor-pointer text-left focus:outline-none focus:ring-2 focus:ring-emerald-500/40"
        >
          <Artwork
            path={genre.artwork_path}
            path1x={genre.artwork_path_1x}
            path2x={genre.artwork_path_2x}
            size="1x"
            className="w-12 h-12"
            rounded="xl"
            iconSize={22}
            placeholderIcon={Tags}
            alt=""
          />
          <div className="flex-1 min-w-0">
            <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
              {genre.name}
            </div>
            <div className="text-xs text-zinc-500">
              {t("library.genreList.trackCount", { count: genre.track_count })}
            </div>
          </div>
        </button>
      ))}
    </div>
  );
}

interface FolderListProps {
  /** Descend into a root and start browsing it (#578). */
  onOpen: (path: string) => void;
  folders: FolderRow[];
  isLoading: boolean;
  t: Translator;
  playlists: Playlist[];
  onAddToPlaylist: (playlistId: number, folderId: number) => void;
  onCreatePlaylist: (folderId: number) => void;
  onToggleWatched: (folderId: number, enable: boolean) => void;
  onRemove: (folderId: number) => void;
  /** Issue #366 — force a full re-hash/re-read of every file in the
   *  folder, bypassing the normal (mtime, size) fast path. */
  onDeepRescan: (folderId: number) => void;
  /** The folder a deep rescan is currently running against, if any. */
  deepRescanFolderId: number | null;
  /** True while ANY scan is running — a library-wide pass included, not
   *  just a folder one. SQLite takes a single writer, so this row's
   *  button has to stand down for the global controls too. */
  isAnyRescanActive: boolean;
}

function FolderList({
  folders,
  onOpen,
  isLoading,
  t,
  playlists,
  onAddToPlaylist,
  onCreatePlaylist,
  onToggleWatched,
  onRemove,
  onDeepRescan,
  deepRescanFolderId,
  isAnyRescanActive,
}: FolderListProps) {
  const [openMenuFolderId, setOpenMenuFolderId] = useState<number | null>(null);
  // Two-step delete: first click arms the confirm state, second click
  // commits. Auto-clears after 3 s so the button doesn't stay armed
  // forever after the user wandered off.
  const [confirmDeleteId, setConfirmDeleteId] = useState<number | null>(null);
  // Listening events attached to the armed folder. Fetched on arm rather
  // than up-front so listing folders stays one query. Keyed by folder id
  // so a result for a folder that is no longer armed is simply ignored at
  // render — no reset pass, and nothing to clear when disarming.
  const [confirmPlayCount, setConfirmPlayCount] = useState<{
    folderId: number;
    count: number;
  } | null>(null);
  useEffect(() => {
    if (confirmDeleteId == null) return;
    const timer = setTimeout(() => setConfirmDeleteId(null), 3_000);
    return () => clearTimeout(timer);
  }, [confirmDeleteId]);

  // Resolve how much history the armed folder carries. The cancel flag
  // drops a response whose folder was disarmed mid-flight; the folder id
  // travels with the result so a late one can never be shown against a
  // different folder either.
  useEffect(() => {
    if (confirmDeleteId == null) return;
    const folderId = confirmDeleteId;
    let cancelled = false;
    void (async () => {
      try {
        const count = await countFolderPlayEvents(folderId);
        if (!cancelled) setConfirmPlayCount({ folderId, count });
      } catch (err) {
        // Non-fatal: the confirm falls back to its plain copy.
        console.error("[LibraryView] count folder play events failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [confirmDeleteId]);

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
              <button
                type="button"
                onClick={() => onOpen(folder.path)}
                title={t("library.folderBrowser.open")}
                className="block w-full text-left text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate hover:underline focus-visible:underline focus-visible:outline-none"
              >
                {folder.path}
              </button>
              <div className="text-xs text-zinc-500">
                {t("library.folderList.trackCount", {
                  count: folder.track_count,
                })}
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
                {folder.is_watched === 1 ? (
                  <Eye size={16} />
                ) : (
                  <EyeOff size={16} />
                )}
              </button>
            </Tooltip>
            <Tooltip label={t("library.folderList.deepRescan")}>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onDeepRescan(folder.id);
                }}
                disabled={isAnyRescanActive}
                aria-label={t("library.folderList.deepRescan")}
                aria-busy={deepRescanFolderId === folder.id}
                className={`p-1.5 rounded-full transition-colors text-zinc-400 hover:text-zinc-800 dark:hover:text-white hover:bg-zinc-100 dark:hover:bg-zinc-700 disabled:opacity-50 ${
                  deepRescanFolderId === folder.id
                    ? "opacity-100"
                    : "opacity-0 group-hover:opacity-100"
                }`}
              >
                {deepRescanFolderId === folder.id ? (
                  <Loader2 size={16} className="animate-spin" />
                ) : (
                  <RefreshCcw size={16} />
                )}
              </button>
            </Tooltip>
            <Tooltip
              label={
                confirmDeleteId === folder.id
                  ? confirmPlayCount?.folderId === folder.id &&
                    confirmPlayCount.count > 0
                    ? t("library.folderList.removeConfirmWithPlays", {
                        count: confirmPlayCount.count,
                      })
                    : t("library.folderList.removeConfirm")
                  : t("library.folderList.remove")
              }
            >
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  if (confirmDeleteId === folder.id) {
                    setConfirmDeleteId(null);
                    onRemove(folder.id);
                  } else {
                    setConfirmDeleteId(folder.id);
                  }
                }}
                aria-label={
                  confirmDeleteId === folder.id
                    ? t("library.folderList.removeConfirm")
                    : t("library.folderList.remove")
                }
                className={`p-1.5 rounded-full transition-colors ${
                  confirmDeleteId === folder.id
                    ? "bg-red-500 text-white"
                    : "opacity-0 group-hover:opacity-100 text-zinc-400 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-500/10"
                }`}
              >
                <Trash2 size={16} />
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
                    onCreatePlaylist(folder.id);
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
function FolderBrowser({
  listing,
  isLoading,
  density,
  onDensity,
  t,
  locale,
  onOpen,
  onRoots,
  playlists,
  onPlay,
  onQueue,
  onPlayNext,
  onAddToPlaylist,
  onCreatePlaylist,
  onBatchTag,
}: FolderBrowserProps) {
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

function LibraryTabSkeleton({ tab, t }: { tab: LibraryTab; t: Translator }) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  // Screen readers announce "Loading <tab name>…" via role=status. The
  // name is fed from the existing tab label so we don't fork a second
  // copy in every locale.
  const ariaLabel = t("library.skeletonAriaLabel", {
    name: t(`library.tabs.${tab}`),
  });
  if (tab === "morceaux") {
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
  if (tab === "artistes") {
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
  if (tab === "genres") {
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
  // dossiers
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
