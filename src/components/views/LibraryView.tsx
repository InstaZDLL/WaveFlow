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
import { useLibraryPlaylists } from "../../hooks/useLibraryPlaylists";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Music2,
  Disc,
  Mic2,
  Tags,
  Folder,
  RefreshCcw,
  FileSearch,
  ListMusic,
  Clock,
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
];

const emptyStateIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  playlists: ListMusic,
  dossiers: Folder,
};

const headerIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  playlists: ListMusic,
  dossiers: Folder,
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
  const { t } = useTranslation();
  const {
    libraries,
    selectedLibraryId,
    selectLibrary,
    createLibrary,
    importFolder,
    rescanLibrary,
  } = useLibrary();
  const { playTracks, currentTrack, isPlaying } = usePlayer();
  const {
    playlists,
    addTracksToPlaylist,
    removeTrackFromPlaylist,
    addSourceToPlaylist,
    createPlaylist,
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
  });
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
  useTrackUpdated(useCallback(() => setEditRefetch((k) => k + 1), []));
  const clearSelection = selection.clear;
  useEffect(() => {
    clearSelection();
  }, [activeTab, clearSelection]);

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
    }
  };
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
      // removal / tag edits do), so the row's last-scan date and track
      // count would otherwise stay stale until some unrelated refresh —
      // same refetch the mount effect above uses.
      const list = await listFolders(null);
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
    (activeTab === "dossiers" && folders.length > 0);

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
                <RefreshCcw
                  size={18}
                  className={isRescanning ? "animate-spin" : ""}
                />
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
            <>
              <TrackTable
                tracks={tracks}
                isLoading={loading.morceaux}
                view={tracksView}
                t={t}
                onPlayTrack={(index) => playRow(index)}
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
                    console.error(
                      "[LibraryView] remove from playlist failed",
                      err,
                    );
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
                singleClickPlay={singleClickPlay}
                onRowSelect={(track, e) => {
                  // Modifier-driven selection always wins so multi-select
                  // remains accessible even with single-click play on.
                  // Selection, and everything it feeds, speaks in local
                  // rowids. The table only hands us local rows here — a remote
                  // one has no `Track` to pass — so the list it ranges over is
                  // narrowed to match.
                  const localRows = tracks
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
                    const idx = tracks.findIndex(
                      (row) =>
                        row.source === "local" && Number(row.id) === track.id,
                    );
                    if (idx >= 0) playRow(idx);
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
                    className="hidden md:flex fixed right-6 top-1/2 -translate-y-1/2 z-30 bg-white/80 dark:bg-zinc-900/70 backdrop-blur-sm rounded-full py-2 px-1.5 shadow-sm"
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
          {activeTab === "dossiers" && (
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

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => {
          setIsCreatePlaylistModalOpen(false);
          setPendingSourceForCreate(null);
        }}
        onCreate={async (data) => {
          try {
            const created = await createPlaylist({
              name: data.name,
              description: data.description || null,
              color_id: data.colorId,
              icon_id: data.iconId,
            });
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
}: TrackTableProps) {
  "use no memo";
  const unknown = t("library.table.unknown");
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
        ref={parentRef}
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
                if (singleClickPlay && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
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
                if (asTrack && onRowMenuKey(e, asTrack)) return;
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
                if (asTrack) onContextMenuRow(e, asTrack);
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
              }}
              className={`group grid ${gridCols} gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
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
              <span
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
              <ArtistLink
                name={track.artist_name}
                artistIds={local ? track.artist_ids : null}
                onNavigate={onNavigateToArtist}
                onNavigateRemote={
                  local || !track.artist_id
                    ? undefined
                    : () => onNavigateToRemoteArtist(track.artist_id as string)
                }
                fallback={unknown}
                className="text-sm text-zinc-500 truncate"
              />
              <AlbumLink
                title={track.album_title}
                albumId={local && track.album_id ? Number(track.album_id) : null}
                onNavigate={onNavigateToAlbum}
                onNavigateRemote={
                  local || !track.album_id
                    ? undefined
                    : () => onNavigateToRemoteAlbum(track.album_id as string)
                }
                fallback={unknown}
                className="text-sm text-zinc-500 truncate"
              />
              <div
                className="flex items-center"
                onDoubleClick={(e) => e.stopPropagation()}
              >
                {/* Rating writes a POPM frame into the file. A server track has
                    no file here, so the control is absent rather than inert —
                    five hollow stars that do nothing read as "unrated". */}
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
                      console.error("[LibraryView] set rating failed", err);
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
              <span className="text-sm tabular-nums text-zinc-400 text-right">
                {formatDuration(track.duration_ms)}
              </span>
              <div className="flex justify-center">
                {localId !== null && (
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    onToggleLike(localId);
                  }}
                  aria-label={
                    likedIds.has(localId) ? t("liked.unlike") : t("liked.like")
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
  const { src: remoteSrc, onError, onLoad } = useRemoteArtworkSrc(
    artist.artwork_hash,
  );
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
              <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                {folder.path}
              </div>
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
                <RefreshCcw
                  size={16}
                  className={
                    deepRescanFolderId === folder.id ? "animate-spin" : ""
                  }
                />
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
            className="grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_7rem_5rem_2rem_2.5rem] gap-4 px-5 h-14 items-center border-b border-zinc-100 dark:border-zinc-800/60"
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
