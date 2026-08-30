import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  Play,
  Shuffle,
  Edit2,
  Trash2,
  Clock,
  Music2,
  Heart,
  GripVertical,
  Download,
  ArrowUpDown,
  Check,
  ArrowDownToLine,
  ListMusic,
  ListPlus,
  Loader2,
  Pencil,
  Plus,
  Search,
  X,
} from "lucide-react";
import {
  DndContext,
  DragOverlay,
  MeasuringStrategy,
  PointerSensor,
  useSensor,
  useSensors,
  closestCenter,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import { useVirtualizer } from "@tanstack/react-virtual";
import { usePageScroll } from "../../hooks/usePageScroll";
import { Artwork } from "../common/Artwork";
import { AlbumLink } from "../common/AlbumLink";
import { ArtistLink } from "../common/ArtistLink";
import { Tooltip } from "../common/Tooltip";
import { EmptyState } from "../common/EmptyState";
import { CreatePlaylistModal } from "../common/CreatePlaylistModal";
import { HiResBadge } from "../common/HiResBadge";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { SelectionActionBar } from "../common/SelectionActionBar";
import { usePlayer } from "../../hooks/usePlayer";
import { usePlaylist } from "../../hooks/usePlaylist";
import { useTrackContextMenu } from "../../hooks/useTrackContextMenu";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useMultiSelect } from "../../hooks/useMultiSelect";
import {
  formatDuration,
  listLikedTrackIds,
  toggleLikeTrack,
  type Track,
} from "../../lib/tauri/track";
import {
  exportPlaylistM3u,
  getPlaylist,
  reorderPlaylistTrack,
  type Playlist,
} from "../../lib/tauri/playlist";
import {
  remoteAddPlaylistTracks,
  remoteDeletePlaylist,
  remoteDownloadTrack,
  remoteListDownloads,
  remoteGetPlayQueue,
  remoteListPlaylists,
  remoteListPlaylistTracks,
  remotePlayPlaylist,
  remotePlayTracks,
  remoteRemovePlaylistTrack,
  remoteReorderPlaylistTrack,
  remoteSearchCatalogue,
  remoteSetFavorite,
  remoteUpdatePlaylist,
  type RemotePlaylistSummary,
  type RemoteTrack,
} from "../../lib/tauri/remoteServer";
import type { LibrarySource } from "../../lib/tauri/browse";
import { pickSaveFile } from "../../lib/tauri/dialog";
import {
  colorForPlaylistId,
  resolvePlaylistColor,
} from "../../lib/playlistVisuals";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import { RemoteArtwork } from "../common/RemoteArtwork";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { isRemoteTrack } from "../../lib/playerSources";
import { notifyRemoteChanged } from "../../hooks/useRemoteSource";
import { useSortMemory } from "../../hooks/useSortMemory";

/**
 * Sort modes for the playlist track list. "custom" preserves the
 * user-curated drag-and-drop order stored as `playlist_track.position` —
 * any other mode is a display-only client-side sort that doesn't touch
 * the DB. Switching back to "custom" restores the persisted order
 * verbatim, Spotify-style.
 */
type PlaylistSortMode =
  | "custom"
  | "title"
  | "artist"
  | "album"
  | "added_at"
  | "duration_ms"
  | "filename";

const PLAYLIST_SORT_MODES: ReadonlyArray<PlaylistSortMode> = [
  "custom",
  "title",
  "artist",
  "album",
  "added_at",
  "duration_ms",
  "filename",
];

/**
 * The subset a server playlist can actually back. `added_at` and
 * `filename` are dropped rather than shown greyed out: a remote track
 * carries neither an add date nor a file, so those two would sort by a
 * value that does not exist — an option that silently does nothing is
 * worse than an absent one.
 */
const REMOTE_SORT_MODES: ReadonlyArray<PlaylistSortMode> = [
  "custom",
  "title",
  "artist",
  "album",
  "duration_ms",
];

/**
 * A track row in this view, from the device or from the bound server.
 *
 * The remote fields are additions rather than reuses on purpose: the
 * server's ids are strings and the local ones are rowids, so carrying a
 * server id in `artist_id` would need a cast that satisfies the compiler
 * and converts nothing, leaving every comparison quietly false.
 */
export type PlaylistTrack = Track & {
  /** Absent on a local track, which is the unmarked case. */
  source?: LibrarySource;
  /** The server's track id: what plays, what the like acts on, and what
   *  the local `id` deliberately is not. */
  remote_id?: string;
  /** Remote only: resolved through the server cover cache. */
  artwork_hash?: string | null;
  /** Remote only: the server's ids for the artist / album links. */
  remote_artist_id?: string | null;
  remote_album_id?: string | null;
  /** Remote only: the synced favourite flag. The local liked-id set is
   *  keyed by rowid and knows nothing about a server track. */
  remote_starred?: boolean;
};

/**
 * A server playlist's tracks in the shape the table already speaks.
 *
 * Mapping rather than branching everywhere: the header, the sort, the
 * virtualizer and the row all read one shape, and a second one would mean
 * a second version of each. What the server has no answer for is `null` —
 * which is what those fields already mean locally before enrichment.
 */
function toPlaylistTracks(rows: RemoteTrack[]): PlaylistTrack[] {
  return rows.map((row, index) => ({
    source: "remote" as const,
    remote_id: row.id,
    artwork_hash: row.artwork_hash,
    remote_artist_id: row.artist_id,
    remote_album_id: row.album_id,
    remote_starred: row.starred,
    // Negative so a leak is obvious rather than colliding with a real
    // rowid, and distinct per row so drag-and-drop keeps a stable handle
    // even when the server holds the same track twice in one playlist.
    id: -(index + 1),
    library_id: -1,
    title: row.title ?? "",
    album_id: null,
    album_title: row.album,
    artist_id: null,
    artist_name: row.artist,
    artist_ids: null,
    duration_ms: row.duration_ms ?? 0,
    track_number: index + 1,
    disc_number: 1,
    year: null,
    bitrate: null,
    sample_rate: null,
    channels: null,
    bit_depth: null,
    codec: null,
    musical_key: null,
    file_path: "",
    file_size: 0,
    added_at: 0,
    artwork_path: null,
    artwork_path_1x: null,
    artwork_path_2x: null,
    rating: null,
  }));
}

/**
 * A server playlist's header data in the local `Playlist` shape.
 *
 * Counts come from the rows we just fetched, not from the summary: the
 * summary's duration sums only the tracks whose metadata is cached, and
 * the rows are what the table is about to render.
 */
function toPlaylist(
  summary: RemotePlaylistSummary,
  remoteId: string,
  rows: RemoteTrack[],
): Playlist {
  return {
    // Never read: every site that acts on a rowid checks `remote` first.
    id: -1,
    name: summary.name,
    description: summary.comment,
    // A server playlist carries no colour, so it is derived from the id —
    // the same hash always landing on the same swatch.
    color_id: colorForPlaylistId(remoteId).id,
    icon_id: "music",
    is_smart: 0,
    cover_hash: null,
    cover_path: null,
    cover_is_auto: 0,
    position: 0,
    created_at: 0,
    updated_at: 0,
    track_count: rows.length,
    total_duration_ms: rows.reduce((sum, r) => sum + (r.duration_ms ?? 0), 0),
    smart_rules: null,
  };
}

/** Cross-platform basename — handles both Windows (`\`) and POSIX
 *  (`/`) separators since profiles can ship libraries scanned on
 *  either OS, and an imported `.waveflow` archive may cross
 *  platforms. */
function basename(path: string): string {
  const slash = path.lastIndexOf("/");
  const back = path.lastIndexOf("\\");
  return path.slice(Math.max(slash, back) + 1);
}

function isPlaylistSortMode(value: string): value is PlaylistSortMode {
  return (PLAYLIST_SORT_MODES as readonly string[]).includes(value);
}

interface PlaylistViewProps {
  playlistId: number | null;
  /** Set instead of `playlistId` when the playlist is the bound server's.
   *  Exactly one of the two is ever set: they are two catalogues, not two
   *  ids for one playlist. */
  remotePlaylistId?: string | null;
  /** Called when the active playlist gets deleted so AppLayout can swap. */
  onAfterDelete: () => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
}

/**
 * One playlist's detail, from the device or from the bound server.
 *
 * The server's playlists had a view of their own until now. That twin was
 * the one case in the series where the copy was the *richer* of the two —
 * it had an inline rename, a per-row remove and a live catalogue search
 * the local view has no equivalent for — so absorbing it means keeping
 * those, not dropping them: they are remote-only affordances here, gated
 * the same way the local-only ones are.
 */
export function PlaylistView({
  playlistId,
  remotePlaylistId = null,
  onAfterDelete,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
}: PlaylistViewProps) {
  const { t } = useTranslation();
  // Which catalogue this playlist came from. Everything that touches a
  // local rowid, a file or the local user data is gated on it.
  const remote = remotePlaylistId != null;
  const { playTracks, currentTrack, toggleShuffle, isPlaying } = usePlayer();
  const {
    updatePlaylist,
    deletePlaylist,
    getPlaylistTracks,
    playlists,
    removeTrackFromPlaylist,
    createPlaylist,
    refresh: refreshPlaylists,
  } = usePlaylist();
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] =
    useState(false);

  const [playlist, setPlaylist] = useState<Playlist | null>(null);
  const [tracks, setTracks] = useState<PlaylistTrack[]>([]);
  /** Remote only: created here and never sent to the server yet. */
  const [remotePending, setRemotePending] = useState(false);
  // Init `true` so the skeleton paints on first render — the
  // not-found EmptyState (`playlist == null && !isLoading` early return
  // a few lines down) is also predicated on this flag, so leaving it
  // `false` here would flash "playlist not found" for one frame
  // before the fetch effect schedules.
  const [isLoading, setIsLoading] = useState(true);
  // Per-playlist sort mode, persisted in `profile_setting['sort.playlist:<id>']`
  // via `useSortMemory`. The hook keeps a `direction` field for API
  // symmetry with the library view, but the playlist UI only exposes
  // the orderBy axis — direction is implied by the mode (title/artist/
  // album = asc, added_at/duration_ms = desc, custom = stored order).
  const sortContextKey = remote
    ? `playlist:remote:${remotePlaylistId}`
    : playlistId != null
      ? `playlist:${playlistId}`
      : "playlist:none";
  const playlistSort = useSortMemory(sortContextKey, {
    orderBy: "custom",
    direction: "asc",
  });
  // The modes offered depend on the source, so a persisted value is
  // checked against the list actually on screen — not just against the
  // union — or a server playlist could restore a sort its menu no longer
  // offers and show a mode nothing selects.
  const sortModes = remote ? REMOTE_SORT_MODES : PLAYLIST_SORT_MODES;
  const rawSortMode = playlistSort.sort.orderBy;
  const sortMode: PlaylistSortMode =
    isPlaylistSortMode(rawSortMode) && sortModes.includes(rawSortMode)
      ? rawSortMode
      : "custom";
  const setSortMode = useCallback(
    (mode: PlaylistSortMode) => {
      playlistSort.setSort({ orderBy: mode, direction: "asc" });
    },
    [playlistSort],
  );
  const [isEditOpen, setIsEditOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [likedIds, setLikedIds] = useState<Set<number>>(new Set());
  const [isDeleting, setIsDeleting] = useState(false);
  // Remote only. The server round-trips are the ones worth blocking on —
  // a second reorder mid-write would race the first — and they are the
  // only ones that surface their failure in the page rather than the
  // console, since a queued write that never leaves is not visible
  // anywhere else.
  const [remoteBusy, setRemoteBusy] = useState(false);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [isRenaming, setIsRenaming] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  // Add-tracks panel: a live catalogue search with a "+" per hit. The
  // local side has no equivalent — tracks get added from the library — so
  // this is the twin's own affordance, kept.
  const [isAdding, setIsAdding] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<RemoteTrack[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  // Server id of the remote track playing right now, so the matching row
  // lights up the way a local one does off `currentTrack.id`.
  const [playingRemoteId, setPlayingRemoteId] = useState<string | null>(null);
  // Remote only: which of this playlist's tracks already have an offline copy,
  // and how far a "keep offline" run has got. Kept as a set of server ids
  // rather than per-row state — the question is asked once for the whole
  // playlist, and a row does not need to answer it on its own.
  const [offlineIds, setOfflineIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [keeping, setKeeping] = useState<{
    done: number;
    total: number;
  } | null>(null);
  // Bumped by the `track:updated` listener and by every remote write, to
  // flip the fetch effect's deps and pull a fresh snapshot when neither
  // the playlist id nor its `updated_at` changed.
  const [refetchKey, setRefetchKey] = useState(0);
  const selection = useMultiSelect<PlaylistTrack>();
  const confirmTimeoutRef = useRef<number | null>(null);
  // Latest tracks snapshot kept in a ref so row callbacks (play, reorder)
  // can stay reference-stable across optimistic reorders. Without this
  // they'd close over `tracks` and bust the memo() on every row each time
  // the array changes mid-drag.
  const tracksRef = useRef<PlaylistTrack[]>(tracks);
  useEffect(() => {
    tracksRef.current = tracks;
  }, [tracks]);

  // Client-side sort. "custom" preserves the stored order verbatim;
  // every other mode is a stable JS sort by the relevant field. The
  // comparator picks an axis-appropriate direction (alphabetical for
  // title/artist/album, most-recent-first for added_at, longest-first
  // for duration) so the dropdown stays single-axis — the screen we
  // mirror (Spotify) doesn't expose a direction toggle either.
  const displayTracks = useMemo<PlaylistTrack[]>(() => {
    if (sortMode === "custom") return tracks;
    const collator = new Intl.Collator(undefined, {
      numeric: true,
      sensitivity: "base",
    });
    const sorted = [...tracks];
    switch (sortMode) {
      case "title":
        sorted.sort((a, b) => collator.compare(a.title, b.title));
        break;
      case "artist":
        sorted.sort((a, b) =>
          collator.compare(a.artist_name ?? "", b.artist_name ?? ""),
        );
        break;
      case "album":
        sorted.sort((a, b) =>
          collator.compare(a.album_title ?? "", b.album_title ?? ""),
        );
        break;
      case "added_at":
        sorted.sort((a, b) => (b.added_at ?? 0) - (a.added_at ?? 0));
        break;
      case "duration_ms":
        sorted.sort((a, b) => (b.duration_ms ?? 0) - (a.duration_ms ?? 0));
        break;
      case "filename":
        // Numeric collator gives a natural order on "1 …", "2 …",
        // "10 …" filenames — the most common manual-numbering scheme
        // (matches Explorer / Finder behaviour). Sorted on the basename
        // only so users grouping by parent folder still see filename
        // order, not full-path lexicographic order.
        sorted.sort((a, b) =>
          collator.compare(basename(a.file_path), basename(b.file_path)),
        );
        break;
    }
    return sorted;
  }, [tracks, sortMode]);
  const displayTracksRef = useRef<PlaylistTrack[]>(displayTracks);
  useEffect(() => {
    displayTracksRef.current = displayTracks;
  }, [displayTracks]);

  useEffect(() => {
    return () => {
      if (confirmTimeoutRef.current != null) {
        window.clearTimeout(confirmTimeoutRef.current);
      }
    };
  }, []);

  // Load liked IDs so hearts render correctly. The liked list is keyed by
  // rowid, so it says nothing about a server track — whose heart reads
  // `remote_starred` off the row instead.
  useEffect(() => {
    if (remote) return;
    listLikedTrackIds()
      .then((ids) => setLikedIds(new Set(ids)))
      .catch(() => {});
  }, [playlistId, remote]);

  // Clear selection when switching playlists.
  const clearSelection = selection.clear;
  useEffect(() => {
    clearSelection();
  }, [playlistId, remotePlaylistId, clearSelection]);

  // Which of our rows is the remote track playing right now. The
  // synthesized remote Track carries a negative sentinel id, not the
  // server id — that lives on the live remote queue, so it is read from
  // there whenever the current track is a remote stream.
  const currentIsRemote = isRemoteTrack(currentTrack);
  const currentSentinelId = currentTrack?.id ?? null;
  useEffect(() => {
    if (!currentIsRemote) return;
    let cancelled = false;
    remoteGetPlayQueue()
      .then((q) => {
        if (cancelled) return;
        setPlayingRemoteId(q?.entries[q.index]?.id ?? null);
      })
      .catch(() => {
        if (!cancelled) setPlayingRemoteId(null);
      });
    return () => {
      cancelled = true;
    };
  }, [currentIsRemote, currentSentinelId]);
  // Read through the flag rather than clearing the state when the current
  // track stops being a remote one: a stale server id is then never read,
  // and the effect has no reset to write.
  const currentRemoteId = currentIsRemote ? playingRemoteId : null;

  const handleRowSelect = useCallback(
    (track: PlaylistTrack, e: React.MouseEvent) => {
      // Selection is keyed by rowid and every action it feeds is local.
      if (track.source === "remote") return;
      // Range selection follows what the user sees on screen — sorting
      // doesn't change which rows belong to the visual range between
      // anchor and target, so the displayed array is the right input.
      const items = displayTracksRef.current;
      if (e.shiftKey) {
        selection.selectRange(track.id, items);
      } else if (e.ctrlKey || e.metaKey) {
        selection.toggleOne(track.id);
      } else {
        selection.setSingle(track.id);
      }
    },
    [selection],
  );

  const handleToggleLike = useCallback(async (track: PlaylistTrack) => {
    if (track.source === "remote") {
      const serverId = track.remote_id;
      if (!serverId) return;
      const next = !track.remote_starred;
      // Optimistic: the write is durable locally and travels on the next
      // drain, so there is nothing to wait on. Matched on the server id,
      // not the row, so a track the playlist holds twice stays coherent.
      setTracks((prev) =>
        prev.map((row) =>
          row.remote_id === serverId ? { ...row, remote_starred: next } : row,
        ),
      );
      try {
        await remoteSetFavorite("track", serverId, next);
        notifyRemoteChanged();
      } catch (err) {
        console.error("[PlaylistView] remote toggle like failed", err);
        setTracks((prev) =>
          prev.map((row) =>
            row.remote_id === serverId
              ? { ...row, remote_starred: !next }
              : row,
          ),
        );
      }
      return;
    }
    const nowLiked = await toggleLikeTrack(track.id);
    setLikedIds((prev) => {
      const next = new Set(prev);
      if (nowLiked) next.add(track.id);
      else next.delete(track.id);
      return next;
    });
  }, []);

  const handleExportM3u = useCallback(async () => {
    if (!playlist) return;
    const safeName =
      playlist.name.replace(/[\\/:*?"<>|]/g, "_").trim() || "playlist";
    const dest = await pickSaveFile(
      `${safeName}.m3u8`,
      ["m3u8", "m3u"],
      t("playlistView.export.dialogTitle"),
    );
    if (!dest) return;
    try {
      await exportPlaylistM3u(playlist.id, dest);
    } catch (err) {
      console.error("[PlaylistView] export m3u failed", err);
    }
  }, [playlist, t]);

  const handleRemoveFromPlaylist = useCallback(
    async (pid: number, trackId: number) => {
      try {
        await removeTrackFromPlaylist(pid, trackId);
        setTracks((prev) => prev.filter((t) => t.id !== trackId));
      } catch (err) {
        console.error("[PlaylistView] remove track from playlist failed", err);
      }
    },
    [removeTrackFromPlaylist],
  );

  const handleReorder = useCallback(
    (fromIdx: number, toIdx: number) => {
      if (fromIdx === toIdx) return;
      const current = tracksRef.current;
      const moved = current[fromIdx];
      if (!moved) return;
      // Optimistic local reorder so the row settles in place before
      // the round-trip; on failure both sides roll the move back.
      setTracks((prev) => arrayMove(prev, fromIdx, toIdx));
      const rollback = (err: unknown) => {
        console.error("[PlaylistView] reorder failed", err);
        setTracks((prev) => arrayMove(prev, toIdx, fromIdx));
      };
      if (remotePlaylistId != null) {
        // The two sides do not name the moved row the same way. Locally a
        // playlist entry is its track's rowid, so the move travels as
        // (track, destination). The server keys entries by position and
        // may hold the same track twice, so there is no id to name one by
        // — the move travels as (from, to). Same gesture, two writes.
        remoteReorderPlaylistTrack(remotePlaylistId, fromIdx, toIdx)
          .then(() => notifyRemoteChanged())
          .catch(rollback);
        return;
      }
      if (playlistId == null) return;
      reorderPlaylistTrack(playlistId, moved.id, toIdx).catch(rollback);
    },
    [playlistId, remotePlaylistId],
  );

  const isCustomOrder = sortMode === "custom";

  const handlePlayTrackByIndex = useCallback(
    (index: number) => {
      // Play in the order the user is seeing — not the stored order —
      // so a sort-by-Title view enqueues the alphabetical sequence and
      // "next" stays sensible.
      const current = displayTracksRef.current;
      if (index < 0 || index >= current.length) return;
      if (remotePlaylistId != null) {
        // In the curated order the server can build its own queue from
        // the playlist; under a display sort it cannot, so the shown
        // sequence is enqueued by id instead.
        if (isCustomOrder) {
          void remotePlayPlaylist(remotePlaylistId, index);
        } else {
          void remotePlayTracks(
            current.map((row) => row.remote_id ?? ""),
            index,
          );
        }
        return;
      }
      if (playlistId == null) return;
      void playTracks(current, index, { type: "playlist", id: playlistId });
    },
    [playTracks, playlistId, remotePlaylistId, isCustomOrder],
  );

  /** Remote only: drop the entry at `index`. The server keys entries by
   *  position, which is why this is offered in the curated order only. */
  const handleRemoveRemoteAt = useCallback(
    async (index: number) => {
      if (remotePlaylistId == null) return;
      setRemoteBusy(true);
      setRemoteError(null);
      try {
        await remoteRemovePlaylistTrack(remotePlaylistId, index);
        notifyRemoteChanged();
        setRefetchKey((k) => k + 1);
      } catch (err) {
        setRemoteError(String(err));
      } finally {
        setRemoteBusy(false);
      }
    },
    [remotePlaylistId],
  );

  /**
   * Remote only: keep every track of this playlist on disk.
   *
   * Sequential on purpose. Parallel downloads would race the server's own
   * transcode ceiling and each other's bandwidth for no gain — the wait is the
   * bytes, not the round-trips — and one at a time is what lets the count mean
   * something while it runs.
   */
  const keepOffline = useCallback(async () => {
    const missing = tracksRef.current
      .map((row) => row.remote_id)
      .filter((id): id is string => !!id && !offlineIds.has(id));
    if (missing.length === 0) return;
    setRemoteError(null);
    setKeeping({ done: 0, total: missing.length });
    const kept = new Set(offlineIds);
    try {
      for (const [index, id] of missing.entries()) {
        await remoteDownloadTrack(id);
        kept.add(id);
        setKeeping({ done: index + 1, total: missing.length });
      }
    } catch (err) {
      // Stop at the first failure rather than hammering a server that just
      // refused; what was already kept stays kept.
      setRemoteError(String(err));
    } finally {
      setOfflineIds(kept);
      setKeeping(null);
    }
  }, [offlineIds]);

  /** Remote only: append server tracks picked from the catalogue search. */
  const handleAddRemoteTracks = useCallback(
    async (ids: string[]) => {
      if (remotePlaylistId == null || ids.length === 0) return;
      setRemoteBusy(true);
      setRemoteError(null);
      try {
        await remoteAddPlaylistTracks(remotePlaylistId, ids);
        notifyRemoteChanged();
        setRefetchKey((k) => k + 1);
      } catch (err) {
        setRemoteError(String(err));
      } finally {
        setRemoteBusy(false);
      }
    },
    [remotePlaylistId],
  );

  const handleLikedChanged = useCallback(
    (trackId: number, nowLiked: boolean) =>
      setLikedIds((prev) => {
        const next = new Set(prev);
        if (nowLiked) next.add(trackId);
        else next.delete(trackId);
        return next;
      }),
    [],
  );

  const handleOpenCreatePlaylistModal = useCallback(
    () => setIsCreatePlaylistModalOpen(true),
    [],
  );

  const trackContextMenu = useTrackContextMenu({
    likedIds,
    onLikedChanged: handleLikedChanged,
    onCreatePlaylist: handleOpenCreatePlaylistModal,
    onNavigateToAlbum,
    onNavigateToArtist,
    currentPlaylistId: playlistId,
    onRemoveFromPlaylist: handleRemoveFromPlaylist,
    selectedTrackIds: [...selection.selectedIds],
  });
  const onContextMenuRow = trackContextMenu.open;
  const onRowMenuKey = trackContextMenu.openFromKeyboard;

  // Fetch playlist + its tracks whenever the focused id changes. Also
  // re-runs when the playlist list itself updates (e.g. after rename via
  // `updatePlaylist`) so the header reflects the new metadata without a
  // manual refresh.
  const playlistsSignature = playlists
    .map((p) => `${p.id}:${p.updated_at}`)
    .join(",");
  // Local churn says nothing about a server playlist, and refetching one
  // over it would be a network round-trip for a signal that cannot
  // concern it. Its own refreshes travel through `refetchKey`.
  const refreshSignature = remotePlaylistId ?? playlistsSignature;
  useTrackUpdated(useCallback(() => setRefetchKey((k) => k + 1), []));

  /**
   * Which playlist the snapshot below belongs to.
   *
   * `remote` is derived from the props and flips the instant navigation
   * happens, while `playlist` and `tracks` still hold the previous
   * playlist — so a play or a reorder in that window would take the remote
   * branch over local rows and hand the server ids that do not exist.
   * The key is the *identity* only: a refetch of the same playlist (a tag
   * edit, a rename, a cover change) leaves it matching, so the table does
   * not blink back to a skeleton on every one.
   */
  const playlistKey = remote
    ? `remote:${remotePlaylistId}`
    : `local:${playlistId}`;
  const [loadedKey, setLoadedKey] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (playlistId == null && remotePlaylistId == null) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPlaylist(null);
      setTracks([]);
      return;
    }
    (async () => {
      setIsLoading(true);
      try {
        if (remotePlaylistId != null) {
          const [lists, rows] = await Promise.all([
            remoteListPlaylists(),
            remoteListPlaylistTracks(remotePlaylistId),
          ]);
          if (cancelled) return;
          const summary =
            lists.find((entry) => entry.id === remotePlaylistId) ?? null;
          setPlaylist(
            summary ? toPlaylist(summary, remotePlaylistId, rows) : null,
          );
          setTracks(summary ? toPlaylistTracks(rows) : []);
          setRemotePending(summary?.pending_creation ?? false);
        } else if (playlistId != null) {
          const [pl, items] = await Promise.all([
            getPlaylist(playlistId),
            getPlaylistTracks(playlistId),
          ]);
          if (cancelled) return;
          setPlaylist(pl);
          setTracks(items);
          setRemotePending(false);
        }
        if (!cancelled) setLoadedKey(playlistKey);
      } catch (err) {
        if (!cancelled) {
          console.error("[PlaylistView] failed to load playlist", err);
          setPlaylist(null);
          setTracks([]);
          // Stamped on failure too: the not-found state below is a loaded
          // answer about *this* playlist, and leaving the key behind would
          // hold the skeleton forever instead of showing it.
          setLoadedKey(playlistKey);
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    playlistId,
    remotePlaylistId,
    playlistKey,
    refreshSignature,
    getPlaylistTracks,
    refetchKey,
  ]);

  // Which server tracks are already on this disk. Re-read after a run, and
  // when the playlist changes, so the button says what is actually true.
  useEffect(() => {
    if (!remote) return;
    let cancelled = false;
    remoteListDownloads()
      .then((rows) => {
        if (!cancelled) {
          setOfflineIds(new Set(rows.map((row) => row.remote_track_id)));
        }
      })
      .catch(() => {
        // A list we could not read leaves the button offering to download
        // again, which is a no-op server-side rather than a duplicate.
      });
    return () => {
      cancelled = true;
    };
  }, [remote, remotePlaylistId, refetchKey]);

  // Debounced catalogue search while the add panel is open (remote only).
  const searchSeqRef = useRef(0);
  const trimmedQuery = query.trim();
  // An empty box shows nothing and spins for nothing. Derived rather than
  // cleared, so emptying the field needs no write of its own — and a
  // request still in flight over the old text lands on a hidden list.
  const visibleResults = trimmedQuery === "" ? [] : results;
  const showSpinner = trimmedQuery !== "" && isSearching;
  useEffect(() => {
    const trimmed = query.trim();
    if (!isAdding || !trimmed) return;
    const seq = ++searchSeqRef.current;
    const timer = setTimeout(() => {
      // Flip the spinner on only when the request actually fires, not for
      // the whole debounce window on every keystroke.
      setIsSearching(true);
      remoteSearchCatalogue(trimmed)
        .then((rows) => {
          if (seq === searchSeqRef.current) setResults(rows);
        })
        .catch((err) => {
          if (seq === searchSeqRef.current) {
            setRemoteError(String(err));
            setResults([]);
          }
        })
        .finally(() => {
          if (seq === searchSeqRef.current) setIsSearching(false);
        });
    }, 300);
    return () => clearTimeout(timer);
  }, [query, isAdding]);

  // Close the add panel and drop its state when the playlist changes, so
  // a search run against one playlist cannot add to the next, and a
  // failure reported about one is not still on screen over another.
  // The lint disables flag the intentional cross-render reset — the
  // standard "reset state when a prop changes" pattern, the same
  // exception the fetch effect above takes.
  /* eslint-disable react-hooks/set-state-in-effect */
  useEffect(() => {
    setIsAdding(false);
    setQuery("");
    setResults([]);
    setIsRenaming(false);
    setRemoteError(null);
  }, [playlistId, remotePlaylistId]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // Pre-translated table labels — pulled out before any early return so
  // the hook order stays stable across render branches.
  const unknownLabel = t("library.table.unknown");
  const likeLabel = t("liked.like");
  const unlikeLabel = t("liked.unlike");
  const reorderLabel = t("playlistView.actions.reorder");
  const removeLabel = t("playlistView.actions.removeFromPlaylist");
  // A server track can be in a playlist before its metadata has been
  // fetched, so an empty title means "not known yet", not "untitled".
  const awaitingLabel = t("remote.common.awaitingMetadata");
  const headerLabels = useMemo(
    () => ({
      number: t("library.table.number"),
      title: t("library.table.title"),
      artist: t("library.table.artist"),
      album: t("library.table.album"),
      duration: t("library.table.duration"),
    }),
    [t],
  );

  // Remote only: up to four distinct track covers for the header mosaic —
  // the same auto-generated look a local playlist's cover has, composed
  // from the artwork we already hold rather than from a server cover,
  // which does not exist. Deduplicated so a single-album playlist does
  // not show the same tile four times.
  const remoteCoverHashes = useMemo<string[]>(() => {
    if (!remote) return [];
    const seen = new Set<string>();
    const out: string[] = [];
    for (const row of tracks) {
      const hash = row.artwork_hash;
      if (hash && !seen.has(hash)) {
        seen.add(hash);
        out.push(hash);
        if (out.length === 4) break;
      }
    }
    return out;
  }, [remote, tracks]);

  if (playlistId == null && remotePlaylistId == null) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("playlistView.noneTitle")}
        description={t("playlistView.noneDescription")}
        className="py-20"
      />
    );
  }

  // Predicated on the stamp as well as the flag: without it, one render
  // between navigating away from a playlist that failed to load and the
  // next fetch starting would flash "not found" about the new one.
  if (playlist == null && !isLoading && loadedKey === playlistKey) {
    return (
      <EmptyState
        icon={<Music2 size={40} />}
        title={t("playlistView.notFoundTitle")}
        description={t(
          remote
            ? "remote.playlist.notFoundDescription"
            : "playlistView.notFoundDescription",
        )}
        className="py-20"
      />
    );
  }

  // The snapshot still belongs to the playlist we came from. Nothing below
  // may read it — see `playlistKey`.
  if (!playlist || loadedKey !== playlistKey) {
    return <PlaylistSkeleton t={t} />;
  }

  // How many of these tracks are already on this disk. Counted here rather
  // than stored, so removing a copy from Settings is reflected without this
  // view having to hear about it.
  const offlineTrackCount = remote
    ? tracks.reduce(
        (total, row) =>
          total + (row.remote_id && offlineIds.has(row.remote_id) ? 1 : 0),
        0,
      )
    : 0;

  const color = resolvePlaylistColor(playlist.color_id);
  const totalDurationMs = playlist.total_duration_ms;

  const handlePlayAll = async () => {
    if (displayTracks.length === 0) return;
    if (remote) {
      handlePlayTrackByIndex(0);
      return;
    }
    if (playlistId == null) return;
    await playTracks(displayTracks, 0, { type: "playlist", id: playlistId });
  };

  const handleShufflePlay = async () => {
    if (displayTracks.length === 0 || playlistId == null) return;
    await playTracks(displayTracks, 0, { type: "playlist", id: playlistId });
    // Toggle shuffle on if it isn't already; the backend handles the
    // case where it's already shuffled gracefully.
    await toggleShuffle();
  };

  /** Remote only: commit the inline rename in the header. */
  const commitRename = async () => {
    const next = nameDraft.trim();
    if (remotePlaylistId == null || !next || next === playlist.name) {
      setIsRenaming(false);
      return;
    }
    setRemoteBusy(true);
    setRemoteError(null);
    try {
      await remoteUpdatePlaylist(remotePlaylistId, { name: next });
      notifyRemoteChanged();
      setRefetchKey((k) => k + 1);
    } catch (err) {
      setRemoteError(String(err));
    } finally {
      setRemoteBusy(false);
      setIsRenaming(false);
    }
  };

  const handleEditSubmit = async (data: {
    name: string;
    description: string;
    colorId: string;
    iconId: string;
  }) => {
    if (playlistId == null) return;
    try {
      await updatePlaylist(playlistId, {
        name: data.name,
        description: data.description || null,
        color_id: data.colorId,
        icon_id: data.iconId,
      });
    } catch (err) {
      console.error("[PlaylistView] update failed", err);
    }
  };

  /**
   * Two-step delete: first click flips into "confirm?" with a 3 s
   * auto-revert. Second click within the window actually deletes.
   * Mirrors the LibraryView pattern.
   */
  const handleDeleteClick = async () => {
    if ((playlistId == null && remotePlaylistId == null) || isDeleting) return;
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
    if (confirmTimeoutRef.current != null) {
      window.clearTimeout(confirmTimeoutRef.current);
      confirmTimeoutRef.current = null;
    }
    setIsDeleting(true);
    try {
      // Redirect away BEFORE the delete so we don't briefly render a
      // not-found state.
      onAfterDelete();
      if (remotePlaylistId != null) {
        await remoteDeletePlaylist(remotePlaylistId);
        notifyRemoteChanged();
      } else if (playlistId != null) {
        await deletePlaylist(playlistId);
      }
    } catch (err) {
      console.error("[PlaylistView] delete failed", err);
    } finally {
      setIsDeleting(false);
      setConfirmDelete(false);
    }
  };

  const totalDurationLabel =
    totalDurationMs > 0 ? formatDuration(totalDurationMs) : "—";

  return (
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header. Smart playlists (Daily Mix, …) ship a generated cover
          image — render it as a 96×96 tile with a "DAILY MIX" overlay
          label. User-curated playlists fall back to the icon + color
          gradient tile they always had. */}
      {(() => {
        const coverUrl = playlist
          ? resolveRemoteImage(playlist.cover_path, null)
          : null;
        const isSmart = (playlist?.is_smart ?? 0) === 1;
        return (
          <div
            className={`flex items-start justify-between p-6 rounded-2xl ${color.previewBg}`}
          >
            <div className="flex items-center space-x-6 min-w-0">
              {remote && remoteCoverHashes.length >= 4 ? (
                <div className="w-24 h-24 rounded-2xl overflow-hidden shadow-sm shrink-0 grid grid-cols-2">
                  {remoteCoverHashes.map((hash) => (
                    <RemoteArtwork
                      key={hash}
                      hash={hash}
                      className="w-full h-full"
                      iconSize={16}
                    />
                  ))}
                </div>
              ) : remote && remoteCoverHashes.length >= 1 ? (
                <RemoteArtwork
                  hash={remoteCoverHashes[0]}
                  className="w-24 h-24 rounded-2xl shadow-sm shrink-0"
                  iconSize={40}
                />
              ) : (
                <div
                  className={`relative w-24 h-24 rounded-2xl overflow-hidden shadow-sm shrink-0 flex items-center justify-center ${
                    coverUrl ? "" : `${color.tileBg} ${color.tileText}`
                  }`}
                >
                  {coverUrl ? (
                    <>
                      <img
                        src={coverUrl}
                        alt=""
                        className="absolute inset-0 w-full h-full object-cover"
                        loading="lazy"
                      />
                      {isSmart && (
                        <>
                          <div className="absolute inset-x-0 bottom-0 h-1/2 bg-linear-to-t from-black/70 to-transparent" />
                          <div className="absolute bottom-1.5 left-2 right-2 text-[9px] font-bold tracking-widest text-white uppercase truncate">
                            {t("playlistView.smartLabel", "Daily Mix")}
                          </div>
                        </>
                      )}
                    </>
                  ) : remote ? (
                    <ListMusic size={48} />
                  ) : (
                    <PlaylistIcon iconId={playlist.icon_id} size={48} />
                  )}
                </div>
              )}
              <div className="min-w-0">
                <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
                  {t(remote ? "remote.playlist.label" : "playlistView.badge")}
                </div>
                {remote && isRenaming ? (
                  <input
                    autoFocus
                    value={nameDraft}
                    aria-label={t("remote.playlist.nameLabel")}
                    onChange={(e) => setNameDraft(e.target.value)}
                    onBlur={() => void commitRename()}
                    onKeyDown={(e) => {
                      // Ignore the Enter/Escape that closes an IME
                      // composition (Japanese, Chinese, …) — committing or
                      // cancelling on it would fire mid-word, before the
                      // character has been chosen.
                      if (e.nativeEvent.isComposing) return;
                      if (e.key === "Enter") void commitRename();
                      if (e.key === "Escape") setIsRenaming(false);
                    }}
                    className="w-full mb-2 px-2 py-1 text-3xl font-bold rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900"
                  />
                ) : (
                  <h1 className="text-4xl font-bold mb-2 truncate text-zinc-900 dark:text-white">
                    {playlist.name}
                  </h1>
                )}
                {playlist.description && (
                  <p className="text-sm text-zinc-500 mb-2 line-clamp-2">
                    {playlist.description}
                  </p>
                )}
                <div className="flex items-center text-sm text-zinc-500 space-x-2">
                  <Music2 size={16} />
                  <span>
                    {t("playlistView.trackCount", {
                      count: playlist.track_count,
                    })}
                  </span>
                  <span>·</span>
                  <span>{totalDurationLabel}</span>
                  {remote && keeping && (
                    <>
                      <span>·</span>
                      <span>
                        {t("remote.playlist.keepingProgress", {
                          done: keeping.done,
                          total: keeping.total,
                        })}
                      </span>
                    </>
                  )}
                  {remote && !keeping && offlineTrackCount > 0 && (
                    <>
                      <span>·</span>
                      <span>
                        {t("remote.playlist.keptOffline", {
                          count: offlineTrackCount,
                        })}
                      </span>
                    </>
                  )}
                  {remote && remotePending && (
                    <>
                      <span>·</span>
                      <span>{t("remote.playlist.notSent")}</span>
                    </>
                  )}
                </div>
              </div>
            </div>

            <div className="flex items-center space-x-3 shrink-0">
              <button
                type="button"
                onClick={handlePlayAll}
                disabled={tracks.length === 0 || remoteBusy}
                className={`text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm ${color.button} disabled:opacity-50 disabled:cursor-not-allowed`}
              >
                <Play size={16} className="fill-current" />
                <span>{t("playlistView.actions.play")}</span>
              </button>

              <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
                {/* Shuffle is a mode of the local queue; the remote one has
                    none. Hidden rather than disabled — a control that can
                    never be enabled reads as a broken page. */}
                {!remote && (
                  <Tooltip label={t("playlistView.actions.shuffle")}>
                    <button
                      type="button"
                      onClick={handleShufflePlay}
                      disabled={tracks.length === 0}
                      aria-label={t("playlistView.actions.shuffle")}
                      className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Shuffle size={18} />
                    </button>
                  </Tooltip>
                )}

                {/* The twin's own affordance: append tracks picked from a
                    live search of the server's catalogue. The local side
                    adds tracks from the library instead. */}
                {remote && (
                  <Tooltip label={t("remote.playlist.addTracks")}>
                    <button
                      type="button"
                      onClick={() => setIsAdding((v) => !v)}
                      disabled={remoteBusy}
                      aria-label={t("remote.playlist.addTracks")}
                      aria-pressed={isAdding}
                      className={`p-2 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
                        isAdding
                          ? "bg-emerald-50 text-emerald-600 dark:bg-emerald-900/20 dark:text-emerald-400"
                          : "hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white"
                      }`}
                    >
                      <ListPlus size={18} />
                    </button>
                  </Tooltip>
                )}

                {/* Keep every track on this disk. Absent once they all are:
                    a control whose only outcome is "nothing to do" is noise,
                    and the settings card is where copies are reviewed and
                    dropped. */}
                {remote && offlineTrackCount < tracks.length && (
                  <Tooltip label={t("remote.playlist.keepOffline")}>
                    <button
                      type="button"
                      onClick={() => void keepOffline()}
                      disabled={remoteBusy || keeping !== null}
                      aria-label={t("remote.playlist.keepOffline")}
                      className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      {keeping ? (
                        <Loader2 size={18} className="animate-spin" />
                      ) : (
                        <ArrowDownToLine size={18} />
                      )}
                    </button>
                  </Tooltip>
                )}

                {/* A server playlist has a name and a comment and nothing
                    else the modal edits — no colour, no icon, no cover —
                    so it renames in place instead. */}
                {remote ? (
                  <Tooltip label={t("remote.playlist.rename")}>
                    <button
                      type="button"
                      onClick={() => {
                        setNameDraft(playlist.name);
                        setIsRenaming(true);
                      }}
                      disabled={remoteBusy}
                      aria-label={t("remote.playlist.rename")}
                      className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Pencil size={18} />
                    </button>
                  </Tooltip>
                ) : (
                  <Tooltip label={t("playlistView.actions.edit")}>
                    <button
                      type="button"
                      onClick={() => setIsEditOpen(true)}
                      aria-label={t("playlistView.actions.edit")}
                      className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white"
                    >
                      <Edit2 size={18} />
                    </button>
                  </Tooltip>
                )}

                {/* An M3U is a list of file paths; a server track has none. */}
                {!remote && (
                  <Tooltip label={t("playlistView.actions.exportM3u")}>
                    <button
                      type="button"
                      onClick={handleExportM3u}
                      disabled={tracks.length === 0}
                      aria-label={t("playlistView.actions.exportM3u")}
                      className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
                    >
                      <Download size={18} />
                    </button>
                  </Tooltip>
                )}

                <Tooltip
                  label={
                    confirmDelete
                      ? t("playlistView.actions.deleteConfirm")
                      : t("playlistView.actions.delete")
                  }
                >
                  <button
                    type="button"
                    onClick={handleDeleteClick}
                    disabled={isDeleting || remoteBusy}
                    aria-label={t("playlistView.actions.delete")}
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
        );
      })()}

      {/* Add-tracks panel (remote only): a debounced live search of the
          server's catalogue with a "+" per hit. */}
      {remote && isAdding && (
        <div className="rounded-xl border border-zinc-200 dark:border-zinc-700 p-3 space-y-2">
          <div className="relative">
            <Search
              size={15}
              className="absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
            />
            <input
              autoFocus
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label={t("remote.playlist.searchPlaceholder")}
              placeholder={t("remote.playlist.searchPlaceholder")}
              className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
            />
            {showSpinner && (
              <Loader2
                size={15}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-zinc-400 animate-spin"
              />
            )}
          </div>
          {trimmedQuery !== "" &&
            !isSearching &&
            visibleResults.length === 0 && (
              <p className="px-1 py-2 text-xs text-zinc-500">
                {t("remote.playlist.noMatches")}
              </p>
            )}
          {visibleResults.length > 0 && (
            <ul className="max-h-72 overflow-y-auto scrollbar-hide space-y-0.5">
              {visibleResults.map((row) => (
                <li
                  key={row.id}
                  className="group flex items-center gap-3 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
                >
                  <RemoteArtwork hash={row.artwork_hash} />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm truncate text-zinc-800 dark:text-zinc-100">
                      {row.title ?? t("remote.common.untitled")}
                    </div>
                    <div className="text-xs text-zinc-500 truncate">
                      {[row.artist, row.album].filter(Boolean).join(" — ")}
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void handleAddRemoteTracks([row.id])}
                    disabled={remoteBusy}
                    className="shrink-0 p-1.5 rounded-lg text-emerald-600 dark:text-emerald-400 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 disabled:opacity-50"
                    aria-label={t("remote.playlist.addTrack", {
                      title: row.title ?? t("remote.common.trackFallback"),
                    })}
                    title={t("remote.playlist.addToPlaylist")}
                  >
                    <Plus size={16} />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* A queued server write that never leaves has no other surface. */}
      {remoteError && (
        <p className="text-xs text-red-600 dark:text-red-400 break-words">
          {remoteError}
        </p>
      )}

      {/* Sort selector. The dropdown is omitted on empty playlists to
          keep the empty-state visually clean. Only mounted once the
          per-playlist sort memory has finished hydrating so the active
          option doesn't flash from "Custom" to the persisted value on
          first paint. */}
      {tracks.length > 0 && playlistSort.isLoaded && (
        <div className="flex items-center justify-end -mt-4">
          <PlaylistSortMenu
            current={sortMode}
            modes={sortModes}
            onChange={setSortMode}
            t={t}
          />
        </div>
      )}

      {/* Tracks list */}
      {tracks.length === 0 && isLoading ? (
        <PlaylistSkeleton t={t} />
      ) : tracks.length > 0 ? (
        <PlaylistTrackTable
          tracks={displayTracks}
          isLoading={isLoading}
          remote={remote}
          currentTrackId={currentTrack?.id ?? null}
          currentRemoteId={currentRemoteId}
          isPlaying={isPlaying}
          onPlayTrack={handlePlayTrackByIndex}
          likedIds={likedIds}
          onToggleLike={handleToggleLike}
          onNavigateToAlbum={onNavigateToAlbum}
          onNavigateToArtist={onNavigateToArtist}
          onNavigateToRemoteAlbum={onNavigateToRemoteAlbum}
          onNavigateToRemoteArtist={onNavigateToRemoteArtist}
          unknownLabel={unknownLabel}
          headerLabels={headerLabels}
          likeLabel={likeLabel}
          unlikeLabel={unlikeLabel}
          reorderLabel={reorderLabel}
          removeLabel={removeLabel}
          awaitingLabel={awaitingLabel}
          onContextMenuRow={onContextMenuRow}
          onRowMenuKey={onRowMenuKey}
          onReorder={handleReorder}
          onRemoveAt={remote ? handleRemoveRemoteAt : undefined}
          busy={remoteBusy}
          isSelected={selection.isSelected}
          onRowSelect={handleRowSelect}
          dragEnabled={isCustomOrder}
        />
      ) : (
        <EmptyState
          icon={<Music2 size={40} />}
          title={t("playlistView.emptyTitle")}
          description={t(
            remote
              ? "remote.playlist.emptyDescription"
              : "playlistView.emptyDescription",
          )}
          className="py-20"
        />
      )}

      {/* Both modals write through a rowid — the edit one against this
          playlist, the create one against the local library. Mounted only
          on the local side: with the remote sentinel in `playlist.id` they
          would be addressing row -1. */}
      <CreatePlaylistModal
        isOpen={!remote && isEditOpen}
        onClose={() => setIsEditOpen(false)}
        existing={playlist}
        onCreate={handleEditSubmit}
        onCoverChanged={async () => {
          // Cover backend command already wrote the new hash; pull the
          // fresh row so `cover_path` updates without waiting for the
          // next user navigation, AND refresh PlaylistContext so the
          // sidebar tile re-renders (it reads from the context, not
          // from this view's local state).
          //
          // Decoupled on purpose: a context-refresh failure must not
          // block the local-state update. `PlaylistContext.refresh`
          // swallows its own errors (logs to console), so the bare
          // fire-and-forget is safe.
          if (playlistId == null) return;
          void refreshPlaylists();
          try {
            const fresh = await getPlaylist(playlistId);
            setPlaylist(fresh);
          } catch (err) {
            console.error("[PlaylistView] refresh after cover change", err);
          }
        }}
      />

      <CreatePlaylistModal
        isOpen={!remote && isCreatePlaylistModalOpen}
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
            console.error("[PlaylistView] create playlist failed", err);
          }
        }}
      />

      {!remote && trackContextMenu.render()}

      {playlistId != null && (
        <SelectionActionBar
          trackIds={[...selection.selectedIds]}
          context={{ type: "playlist", playlistId }}
          onClear={selection.clear}
          onCreatePlaylist={() => setIsCreatePlaylistModalOpen(true)}
          onAfterRemoveFromPlaylist={(removedIds) => {
            const removed = new Set(removedIds);
            setTracks((prev) => prev.filter((t) => !removed.has(t.id)));
          }}
        />
      )}
    </div>
  );
}

interface PlaylistTrackTableProps {
  tracks: PlaylistTrack[];
  isLoading: boolean;
  /** The whole table comes from one source — a playlist is one or the
   *  other, never a mix — so the branch is decided once here rather than
   *  per row. */
  remote: boolean;
  currentTrackId: number | null;
  /** Server id of the track playing now, read off the live remote queue.
   *  A remote row's own `id` is a negative sentinel, so matching on it
   *  would highlight nothing, quietly. */
  currentRemoteId: string | null;
  isPlaying: boolean;
  onPlayTrack: (index: number) => void;
  likedIds: Set<number>;
  onToggleLike: (track: PlaylistTrack) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
  unknownLabel: string;
  headerLabels: {
    number: string;
    title: string;
    artist: string;
    album: string;
    duration: string;
  };
  likeLabel: string;
  unlikeLabel: string;
  reorderLabel: string;
  removeLabel: string;
  awaitingLabel: string;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  onReorder: (fromIndex: number, toIndex: number) => void;
  /** Remote only: drop the entry at this index. The local side removes
   *  from the context menu, which a server row has no equivalent of. */
  onRemoveAt?: (index: number) => void;
  /** Remote only: a server write is in flight. */
  busy: boolean;
  isSelected: (id: number) => boolean;
  onRowSelect: (track: PlaylistTrack, e: React.MouseEvent) => void;
  /**
   * When `false` the rows render without grip handles and `onReorder`
   * is never invoked — the playlist is in a non-custom sort mode and
   * drag-to-reorder would mutate the stored order in ways the user
   * isn't asking for.
   */
  dragEnabled: boolean;
}

const PLAYLIST_ROW_HEIGHT = 56;

function PlaylistTrackTable({
  tracks,
  isLoading,
  remote,
  currentTrackId,
  currentRemoteId,
  isPlaying,
  onPlayTrack,
  likedIds,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  unknownLabel,
  headerLabels,
  likeLabel,
  unlikeLabel,
  reorderLabel,
  removeLabel,
  awaitingLabel,
  onContextMenuRow,
  onRowMenuKey,
  onReorder,
  onRemoveAt,
  busy,
  isSelected,
  onRowSelect,
  dragEnabled,
}: PlaylistTrackTableProps) {
  "use no memo";
  // A remote table carries a trailing remove column; the local one puts
  // that in the context menu. Same template otherwise, so the two read
  // alike column for column.
  const gridCols = remote
    ? "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem_2rem]"
    : "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem]";
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
  );
  // Per-row stable IDs. Locally `track.id` is the playlist's PRIMARY KEY
  // and can't repeat (a track is in a playlist either zero or one times).
  // A remote row's id is the projection's negative sentinel, handed out
  // once per position at load time — distinct even when the server holds
  // the same track twice, and stable across an optimistic reorder because
  // the rows move rather than being re-projected.
  const ids = useMemo(() => tracks.map((t) => String(t.id)), [tracks]);

  const [activeId, setActiveId] = useState<string | null>(null);

  const pageScrollRef = usePageScroll();
  const parentRef = useRef<HTMLDivElement>(null);
  // Re-anchor the virtualizer whenever the row container moves within the
  // page scroller (e.g. header expands/collapses). `scrollMargin` tells
  // tanstack-virtual how far down the scroller our rows actually start.
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

  // Virtualize the row list. SortableContext keeps the *full* id array
  // so dnd-kit knows the abstract ordering, even for items that aren't
  // currently mounted — only the on-screen window pays the useSortable
  // cost. This is what makes grab-on-300+-tracks feel instant.
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: tracks.length,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => PLAYLIST_ROW_HEIGHT,
    overscan: 8,
    scrollMargin,
  });

  const handleDragStart = useCallback((e: DragStartEvent) => {
    setActiveId(String(e.active.id));
  }, []);

  const handleDragEnd = useCallback(
    (e: DragEndEvent) => {
      setActiveId(null);
      const { active, over } = e;
      if (!over || active.id === over.id) return;
      const fromId = String(active.id);
      const toId = String(over.id);
      const from = tracks.findIndex((t) => String(t.id) === fromId);
      const to = tracks.findIndex((t) => String(t.id) === toId);
      if (from === -1 || to === -1) return;
      onReorder(from, to);
    },
    [tracks, onReorder],
  );

  const handleDragCancel = useCallback(() => setActiveId(null), []);

  const activeTrack = activeId
    ? tracks.find((t) => String(t.id) === activeId)
    : null;
  return (
    // Borderless wrapper so rows span the full content width Spotify-style
    // — the page-level scroller already provides the visual frame, and a
    // contained card here just shrunk every row by ~40 px on each side.
    // The column header keeps its bottom border for the visual separator.
    <div>
      <div
        className={`grid ${gridCols} gap-4 px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-200 dark:border-zinc-800`}
      >
        <span aria-hidden="true" />
        <span className="text-right">{headerLabels.number}</span>
        <span aria-hidden="true" />
        <span>{headerLabels.title}</span>
        <span>{headerLabels.artist}</span>
        <span>{headerLabels.album}</span>
        <span className="flex justify-end" aria-label={headerLabels.duration}>
          <Clock size={14} />
        </span>
        <span aria-hidden="true" />
        {remote && <span aria-hidden="true" />}
      </div>
      <DndContext
        sensors={sensors}
        collisionDetection={closestCenter}
        modifiers={[restrictToVerticalAxis]}
        // Always-measure works best with virtualization: rows entering the
        // window during a drag scroll get measured on the fly instead of
        // a single synchronous burst on the first dragmove.
        measuring={{ droppable: { strategy: MeasuringStrategy.Always } }}
        onDragStart={handleDragStart}
        onDragEnd={handleDragEnd}
        onDragCancel={handleDragCancel}
      >
        <SortableContext items={ids} strategy={verticalListSortingStrategy}>
          <div
            ref={parentRef}
            className={isLoading ? "opacity-50" : ""}
            style={{
              height: `${rowVirtualizer.getTotalSize()}px`,
              position: "relative",
              width: "100%",
            }}
          >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const track = tracks[virtualRow.index];
              if (!track) return null;
              return (
                <SortablePlaylistRow
                  key={track.id}
                  track={track}
                  index={virtualRow.index}
                  rowHeight={PLAYLIST_ROW_HEIGHT}
                  top={virtualRow.start - scrollMargin}
                  gridCols={gridCols}
                  remote={remote}
                  isCurrent={
                    remote
                      ? currentRemoteId != null &&
                        track.remote_id === currentRemoteId
                      : track.id === currentTrackId
                  }
                  isPlaying={isPlaying}
                  isLiked={
                    remote
                      ? track.remote_starred === true
                      : likedIds.has(track.id)
                  }
                  isRowSelected={!remote && isSelected(track.id)}
                  likeLabel={likeLabel}
                  unlikeLabel={unlikeLabel}
                  unknownLabel={unknownLabel}
                  reorderLabel={reorderLabel}
                  removeLabel={removeLabel}
                  awaitingLabel={awaitingLabel}
                  busy={busy}
                  onPlayTrack={onPlayTrack}
                  onContextMenuRow={onContextMenuRow}
                  onRowMenuKey={onRowMenuKey}
                  onToggleLike={onToggleLike}
                  onNavigateToAlbum={onNavigateToAlbum}
                  onNavigateToArtist={onNavigateToArtist}
                  onNavigateToRemoteAlbum={onNavigateToRemoteAlbum}
                  onNavigateToRemoteArtist={onNavigateToRemoteArtist}
                  onRemoveAt={dragEnabled ? onRemoveAt : undefined}
                  onRowSelect={onRowSelect}
                  dragEnabled={dragEnabled}
                />
              );
            })}
          </div>
        </SortableContext>
        {/* Portal the overlay to <body> so it stays positioned relative
            to the viewport even if a future ancestor introduces a
            `transform` (which would make it the containing block for
            `position: fixed` and pin the overlay off-screen). */}
        {createPortal(
          <DragOverlay dropAnimation={null}>
            {activeTrack ? (
              <PlaylistRowPreview
                track={activeTrack}
                remote={remote}
                unknownLabel={unknownLabel}
              />
            ) : null}
          </DragOverlay>,
          document.body,
        )}
      </DndContext>
    </div>
  );
}

function PlaylistRowPreview({
  track,
  remote,
  unknownLabel,
}: {
  track: PlaylistTrack;
  remote: boolean;
  unknownLabel: string;
}) {
  return (
    <div className="flex items-center space-x-3 p-2 rounded-lg bg-white dark:bg-zinc-800 shadow-lg border border-zinc-200 dark:border-zinc-700 select-none">
      <div className="shrink-0 p-1 -ml-1 text-zinc-400">
        <GripVertical size={14} />
      </div>
      {remote ? (
        <RemoteArtwork
          hash={track.artwork_hash ?? null}
          className="w-10 h-10 rounded-md"
          iconSize={18}
        />
      ) : (
        <Artwork
          path={track.artwork_path}
          className="w-10 h-10"
          iconSize={18}
          alt={track.album_title ?? track.title}
          rounded="md"
        />
      )}
      <div className="flex-1 min-w-0">
        <div className="text-sm truncate text-zinc-800 dark:text-zinc-200">
          {track.title}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {track.artist_name ?? unknownLabel}
        </div>
      </div>
    </div>
  );
}

interface SortablePlaylistRowProps {
  track: PlaylistTrack;
  index: number;
  /** Pixel offset from the virtualizer for this row's slot. */
  top: number;
  rowHeight: number;
  gridCols: string;
  /** From the server rather than the device. Gates the artwork source,
   *  the artist / album links, what the heart writes, and the local-only
   *  selection and context menu. */
  remote: boolean;
  isCurrent: boolean;
  isPlaying: boolean;
  isLiked: boolean;
  isRowSelected: boolean;
  likeLabel: string;
  unlikeLabel: string;
  unknownLabel: string;
  reorderLabel: string;
  removeLabel: string;
  awaitingLabel: string;
  /** Remote only: a server write is in flight. */
  busy: boolean;
  onPlayTrack: (index: number) => void;
  onContextMenuRow: (event: React.MouseEvent, track: Track) => void;
  /** Keyboard equivalent (Menu / Shift+F10). Returns `true` when it
   *  opened the menu, so the row's own key handling can stand down. */
  onRowMenuKey: (event: React.KeyboardEvent, track: Track) => boolean;
  onToggleLike: (track: PlaylistTrack) => void;
  onNavigateToAlbum: (albumId: number) => void;
  onNavigateToArtist: (artistId: number) => void;
  onNavigateToRemoteAlbum?: (remoteAlbumId: string) => void;
  onNavigateToRemoteArtist?: (remoteArtistId: string) => void;
  /** Remote only, and only in the curated order: drop this entry. */
  onRemoveAt?: (index: number) => void;
  onRowSelect: (track: PlaylistTrack, e: React.MouseEvent) => void;
  /** Hide the grip handle when the playlist is in a non-custom sort
   *  mode. dnd-kit's listeners are also detached so a desperate
   *  click-drag on the row body can't trigger a backend reorder.
   */
  dragEnabled: boolean;
}

const SortablePlaylistRow = memo(function SortablePlaylistRow({
  track,
  index,
  top,
  rowHeight,
  gridCols,
  remote,
  isCurrent,
  isPlaying,
  isLiked,
  isRowSelected,
  likeLabel,
  unlikeLabel,
  unknownLabel,
  reorderLabel,
  removeLabel,
  awaitingLabel,
  busy,
  onPlayTrack,
  onContextMenuRow,
  onRowMenuKey,
  onToggleLike,
  onNavigateToAlbum,
  onNavigateToArtist,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  onRemoveAt,
  onRowSelect,
  dragEnabled,
}: SortablePlaylistRowProps) {
  // Disable dnd-kit's per-item layout animations: they trigger CSS
  // transitions on every neighbour the drag crosses, which is what
  // makes the row feel sluggish on long playlists. The visual jump
  // when items snap into their new slot is barely noticeable, and
  // the drag itself is now silky-smooth.
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useSortable({
      id: String(track.id),
      animateLayoutChanges: () => false,
      disabled: !dragEnabled,
    });
  // Place the row's slot via CSS `top` (not via a translateY
  // transform): dnd-kit anchors the drag overlay and resolves drop
  // targets from `offsetTop`, which doesn't see CSS transforms. With
  // `transform: translateY(start)` every row reports `offsetTop = 0`
  // and dnd-kit thinks they're all stacked at the parent's top edge,
  // making the overlay snap to viewport top and collisions resolve to
  // whichever row is first in the DOM. Using `top` keeps offsetTop
  // honest. useSortable's own transform (intra-drag displacement) is
  // kept as the only `transform` on the element so it composes
  // cleanly with `top` instead of fighting it.
  const sortableTransform = CSS.Transform.toString(transform);
  const style: React.CSSProperties = {
    position: "absolute",
    top: `${top}px`,
    left: 0,
    width: "100%",
    height: `${rowHeight}px`,
    transform: sortableTransform || undefined,
    // While this row is the drag source, `<DragOverlay>` shows the
    // visible copy that follows the cursor. We hide the in-place
    // copy but keep it mounted to preserve its slot for neighbour
    // layout calculations.
    opacity: isDragging ? 0 : 1,
  };
  return (
    // Row can't be a <button> because it contains action buttons
    // (heart, more-options) and a drag handle; nested buttons are
    // invalid HTML. Keyboard activation still works via tabIndex +
    // onKeyDown.
    <div
      ref={setNodeRef}
      style={style}
      tabIndex={0}
      role="button"
      onClick={(e) => onRowSelect(track, e)}
      onDoubleClick={() => onPlayTrack(index)}
      onKeyDown={(e) => {
        // Only play when the row itself is focused — see LibraryView.
        if (e.target !== e.currentTarget) return;
        // The context menu acts on a rowid throughout; a server row has
        // none, so it is not offered rather than opened onto dead items.
        if (!remote && onRowMenuKey(e, track)) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onPlayTrack(index);
        }
      }}
      onKeyUp={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === " ") e.preventDefault();
      }}
      onContextMenu={(e) => {
        if (remote) return;
        onContextMenuRow(e, track);
      }}
      className={`group grid ${gridCols} gap-4 px-5 items-center select-none transition-colors cursor-pointer border-b border-zinc-100 dark:border-zinc-800/60 focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
        isRowSelected
          ? "bg-blue-500/15 ring-1 ring-inset ring-blue-500/40 dark:bg-blue-500/20"
          : isCurrent
            ? "bg-emerald-50 dark:bg-emerald-900/20"
            : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      {dragEnabled ? (
        <button
          type="button"
          {...attributes}
          {...listeners}
          aria-label={reorderLabel}
          className="shrink-0 p-1 -ml-1 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 group-hover:opacity-100 transition-opacity"
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          <GripVertical size={14} />
        </button>
      ) : (
        // Empty slot so the row grid columns don't reshuffle when the
        // sort mode flips. Keeps the title / artist / album lined up
        // with the column header.
        <span aria-hidden="true" />
      )}
      <span
        className={`text-right text-sm tabular-nums flex items-center justify-end ${
          isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
        }`}
      >
        {isCurrent ? <PlayingIndicator isPlaying={isPlaying} /> : index + 1}
      </span>
      {remote ? (
        <RemoteArtwork
          hash={track.artwork_hash ?? null}
          className="w-10 h-10 rounded-md"
          iconSize={18}
        />
      ) : (
        <Artwork
          path={track.artwork_path}
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
        <span className="truncate">
          {track.title || (remote ? awaitingLabel : "")}
        </span>
        <HiResBadge
          bitDepth={track.bit_depth}
          sampleRate={track.sample_rate}
          codec={track.codec}
          variant="inline"
        />
      </span>
      {/* The server's ids are strings, so they cannot ride in the numeric
          `artist_id` / `album_id` the shared links navigate by. */}
      {remote ? (
        <RemoteEntityLink
          name={track.artist_name}
          id={track.remote_artist_id}
          onNavigate={onNavigateToRemoteArtist}
        />
      ) : (
        <ArtistLink
          name={track.artist_name}
          artistIds={track.artist_ids}
          onNavigate={onNavigateToArtist}
          fallback={unknownLabel}
          className="text-sm text-zinc-500 truncate"
        />
      )}
      {remote ? (
        <RemoteEntityLink
          name={track.album_title}
          id={track.remote_album_id}
          onNavigate={onNavigateToRemoteAlbum}
        />
      ) : (
        <AlbumLink
          title={track.album_title}
          albumId={track.album_id}
          onNavigate={onNavigateToAlbum}
          fallback={unknownLabel}
          className="text-sm text-zinc-500 truncate"
        />
      )}
      <span className="text-sm tabular-nums text-zinc-400 text-right">
        {formatDuration(track.duration_ms)}
      </span>
      <div className="flex justify-center">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            void onToggleLike(track);
          }}
          aria-label={isLiked ? unlikeLabel : likeLabel}
          aria-pressed={isLiked}
          className={`p-1 rounded-full transition-colors ${
            isLiked
              ? "text-pink-500"
              : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
          }`}
        >
          <Heart size={14} className={isLiked ? "fill-current" : ""} />
        </button>
      </div>
      {/* Remote only. Removing acts on the entry's position, which is why
          the column is empty under a display sort — the same reason the
          grip is. An empty slot keeps the columns lined up with the
          header. */}
      {remote &&
        (onRemoveAt ? (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onRemoveAt(index);
            }}
            disabled={busy}
            className="p-1 rounded text-zinc-300 dark:text-zinc-600 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:text-red-500 dark:hover:text-red-400 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
            aria-label={removeLabel}
            title={removeLabel}
          >
            <X size={15} />
          </button>
        ) : (
          <span aria-hidden="true" />
        ))}
    </div>
  );
});

/**
 * An artist or album cell for a server track: a link when the server gave
 * us an id to navigate by, plain text when it did not.
 */
function RemoteEntityLink({
  name,
  id,
  onNavigate,
}: {
  name: string | null;
  id: string | null | undefined;
  onNavigate?: (id: string) => void;
}) {
  if (name && id && onNavigate) {
    return (
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onNavigate(id);
          }}
          className="truncate max-w-full text-left hover:text-emerald-600 dark:hover:text-emerald-400 hover:underline"
          title={name}
        >
          {name}
        </button>
      </div>
    );
  }
  return (
    <div className="min-w-0 text-sm text-zinc-500 truncate">{name ?? "—"}</div>
  );
}

// =============================================================================
// Playlist sort menu (Spotify-style: list of modes + check, no direction)
// =============================================================================

interface PlaylistSortMenuProps {
  current: PlaylistSortMode;
  /** The modes this playlist's source can actually back. */
  modes: ReadonlyArray<PlaylistSortMode>;
  onChange: (next: PlaylistSortMode) => void;
  // i18next's `t` is heavily overloaded — accept it whole rather than
  // re-declaring a subset that the type checker would reject.
  t: ReturnType<typeof useTranslation>["t"];
}

function PlaylistSortMenu({
  current,
  modes,
  onChange,
  t,
}: PlaylistSortMenuProps) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onClickOutside = (event: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsOpen(false);
    };
    document.addEventListener("mousedown", onClickOutside);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClickOutside);
      document.removeEventListener("keydown", onKey);
    };
  }, [isOpen]);

  // i18n labels with inline fallbacks so the menu works in every locale
  // without forcing a 17-file translation pass for keys most users
  // never see. The fallback strings stay in English so the option set
  // is at least intelligible if a translation drops a key.
  const labels: Record<PlaylistSortMode, string> = {
    custom: t("sort.customOrder", "Custom order"),
    title: t("sort.title"),
    artist: t("sort.artist"),
    album: t("sort.album"),
    added_at: t("sort.recentlyAdded", "Recently added"),
    duration_ms: t("sort.duration"),
    filename: t("sort.filename"),
  };

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
        <span>{labels[current]}</span>
      </button>
      {isOpen && (
        <ul
          role="listbox"
          className="absolute top-full right-0 mt-2 min-w-56 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated overflow-hidden z-50 animate-fade-in py-1"
        >
          <li
            className="px-4 pt-1 pb-2 text-[10px] font-bold tracking-widest text-zinc-400 uppercase"
            aria-hidden="true"
          >
            {t("sort.menuTitle", "Sort by")}
          </li>
          {modes.map((mode) => {
            const isSelected = mode === current;
            return (
              <li key={mode} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={isSelected}
                  onClick={() => {
                    onChange(mode);
                    setIsOpen(false);
                  }}
                  className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors ${
                    isSelected
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                      : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30"
                  }`}
                >
                  <span>{labels[mode]}</span>
                  {isSelected && <Check size={14} />}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

function PlaylistSkeleton({
  t,
}: {
  t: (key: string, options?: Record<string, unknown>) => string;
}) {
  const tile = "bg-zinc-200/70 dark:bg-zinc-700/40";
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={t("playlistView.emptyTitle")}
      className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-800 dark:bg-zinc-800/40 overflow-hidden animate-pulse"
    >
      {Array.from({ length: 10 }).map((_, i) => (
        <div
          key={i}
          className="grid grid-cols-[3rem_2.75rem_1fr_1fr_1fr_5rem] gap-4 px-5 py-2 h-14 items-center border-b border-zinc-100 dark:border-zinc-800/60"
        >
          <div className={`h-3 w-4 rounded ${tile} justify-self-end`} />
          <div className={`w-10 h-10 rounded-md ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 rounded ${tile}`} />
          <div className={`h-3 w-10 rounded ${tile} justify-self-end`} />
        </div>
      ))}
    </div>
  );
}
