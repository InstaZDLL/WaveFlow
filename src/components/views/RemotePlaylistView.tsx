import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { createPortal } from "react-dom";
import {
  ArrowUpDown,
  Check,
  Clock,
  GripVertical,
  Heart,
  Loader2,
  ListMusic,
  ListPlus,
  Music2,
  Pencil,
  Play,
  Plus,
  Search,
  Trash2,
  X,
} from "lucide-react";
import {
  DndContext,
  DragOverlay,
  MeasuringStrategy,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
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
import {
  remoteDeletePlaylist,
  remoteListPlaylists,
  remoteAddPlaylistTracks,
  remoteListPlaylistTracks,
  remotePlayPlaylist,
  remoteRemovePlaylistTrack,
  remoteReorderPlaylistTrack,
  remoteSearchCatalogue,
  remoteSetFavorite,
  remoteUpdatePlaylist,
  remoteGetPlayQueue,
  remotePlayTracks,
  type RemotePlaylistSummary,
  type RemoteTrack,
} from "../../lib/tauri/remoteServer";
import { formatDuration } from "../../lib/tauri/track";
import { notifyRemoteChanged } from "../../hooks/useRemoteSource";
import { usePlayer } from "../../hooks/usePlayer";
import { isRemoteTrack } from "../../lib/playerSources";
import { PlayingIndicator } from "../common/PlayingIndicator";
import { colorForPlaylistId } from "../../lib/playlistVisuals";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * Display-only sort for the remote track table, mirroring the local
 * PlaylistView menu. "custom" preserves the server-curated order (the
 * only one the drag handle and the ×-remove act on, since the server
 * keys tracks by position and may hold duplicates); any other mode is a
 * client-side sort that never touches the projection. `added_at` /
 * `filename` are dropped — a remote track carries neither field.
 */
type RemoteSortMode = "custom" | "title" | "artist" | "album" | "duration";

const REMOTE_SORT_MODES: ReadonlyArray<RemoteSortMode> = [
  "custom",
  "title",
  "artist",
  "album",
  "duration",
];

const REMOTE_SORT_LABEL_KEYS: Record<RemoteSortMode, string> = {
  custom: "remote.playlist.sort.custom",
  title: "remote.playlist.sort.title",
  artist: "remote.playlist.sort.artist",
  album: "remote.playlist.sort.album",
  duration: "remote.playlist.sort.duration",
};

/**
 * A single remote-server playlist, managed like a local one but from the
 * projected `remote_*` cache (RFC-005 sync_v2).
 *
 * ## Localized under `remote.*`
 *
 * `sync_v2` now ships in the default feature set, so this view is
 * reachable in a released build and is translated across every locale
 * through the shared `remote.*` i18n namespace.
 *
 * ## Managed like a local playlist
 *
 * Play (as a native remote queue), rename, delete, remove a track, drag to
 * reorder, and add tracks via a live catalogue search. Every track edit
 * applies to the projection at once and queues an `UpdatePlaylist` for the
 * server, so it survives offline.
 */
export function RemotePlaylistView({
  remotePlaylistId,
  onAfterDelete,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
}: {
  remotePlaylistId: string | null;
  onAfterDelete: () => void;
  onNavigateToRemoteAlbum: (albumId: string) => void;
  onNavigateToRemoteArtist: (artistId: string) => void;
}) {
  const { t } = useTranslation();
  const { currentTrack, isPlaying } = usePlayer();
  const [summary, setSummary] = useState<RemotePlaylistSummary | null>(null);
  const [tracks, setTracks] = useState<RemoteTrack[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [sortMode, setSortMode] = useState<RemoteSortMode>("custom");
  // Two-step delete, mirroring the local PlaylistView: first click arms
  // it, a second within 3 s confirms, otherwise it auto-reverts.
  const [confirmDelete, setConfirmDelete] = useState(false);
  const confirmTimeoutRef = useRef<number | null>(null);
  // Server id of the remote track currently playing, so the matching row
  // lights up like a local playlist. Null unless a remote queue is live.
  const [playingRemoteId, setPlayingRemoteId] = useState<string | null>(null);
  // Add-tracks panel: a live catalogue search with a "+" per hit.
  const [adding, setAdding] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<RemoteTrack[]>([]);
  const [searching, setSearching] = useState(false);
  // Index (as a string) of the row currently being dragged, for the
  // DragOverlay copy — the in-place row can scroll out of the virtual
  // window mid-drag, so the overlay is what the user actually follows.
  const [activeId, setActiveId] = useState<string | null>(null);

  const seqRef = useRef(0);
  const load = useCallback(async () => {
    if (!remotePlaylistId) return;
    const seq = ++seqRef.current;
    setLoading(true);
    setError(null);
    try {
      const [lists, rows] = await Promise.all([
        remoteListPlaylists(),
        remoteListPlaylistTracks(remotePlaylistId),
      ]);
      if (seq !== seqRef.current) return;
      setSummary(lists.find((p) => p.id === remotePlaylistId) ?? null);
      setTracks(rows);
    } catch (err) {
      if (seq === seqRef.current) setError(String(err));
    } finally {
      if (seq === seqRef.current) setLoading(false);
    }
  }, [remotePlaylistId]);

  useEffect(() => {
    // `load` flips `loading` on synchronously — the intended initial
    // state for a freshly-opened playlist, not a cascading-render smell.
    void load();
  }, [load]);

  // Cleanup the delete-confirm timer on unmount.
  useEffect(
    () => () => {
      if (confirmTimeoutRef.current != null) {
        window.clearTimeout(confirmTimeoutRef.current);
      }
    },
    [],
  );

  // Resolve which of our rows (if any) is the remote track playing right
  // now, so it highlights like a local one. The synthesized remote Track
  // carries a negative sentinel id, not the server id — the server id
  // lives on the live remote queue, so read it from there whenever the
  // current track is a remote stream. Re-runs on every track change.
  const currentIsRemote = isRemoteTrack(currentTrack);
  const currentSentinelId = currentTrack?.id ?? null;
  useEffect(() => {
    if (!currentIsRemote) {
      setPlayingRemoteId(null);
      return;
    }
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

  // Client-side display order. "custom" keeps the server order verbatim;
  // every other mode is a stable copy-sort that never mutates the
  // projection — the drag handle and ×-remove are hidden while it's
  // active (see the row), so the two can't disagree on positions.
  // Up to four distinct track covers for the header mosaic — the same
  // auto-generated look the local "Liked Songs" tile has, composed here
  // from the per-track artwork we already hold (no server cover needed).
  // Dedup so a single-artist playlist doesn't show four identical tiles.
  const coverHashes = useMemo<string[]>(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const t of tracks) {
      if (t.artwork_hash && !seen.has(t.artwork_hash)) {
        seen.add(t.artwork_hash);
        out.push(t.artwork_hash);
        if (out.length === 4) break;
      }
    }
    return out;
  }, [tracks]);

  const isCustomOrder = sortMode === "custom";
  const displayTracks = useMemo<RemoteTrack[]>(() => {
    if (isCustomOrder) return tracks;
    const collator = new Intl.Collator(undefined, {
      numeric: true,
      sensitivity: "base",
    });
    const sorted = [...tracks];
    switch (sortMode) {
      case "title":
        sorted.sort((a, b) =>
          collator.compare(a.title ?? "", b.title ?? ""),
        );
        break;
      case "artist":
        sorted.sort((a, b) =>
          collator.compare(a.artist ?? "", b.artist ?? ""),
        );
        break;
      case "album":
        sorted.sort((a, b) =>
          collator.compare(a.album ?? "", b.album ?? ""),
        );
        break;
      case "duration":
        sorted.sort((a, b) => (b.duration_ms ?? 0) - (a.duration_ms ?? 0));
        break;
    }
    return sorted;
  }, [tracks, sortMode, isCustomOrder]);

  // Play from `displayIndex` in the order the user is seeing. In custom
  // order that's the server order, so `remotePlayPlaylist` builds the
  // native queue directly; under a display sort we enqueue the shown
  // sequence by id via `remotePlayTracks` so "next" stays sensible.
  const playFrom = useCallback(
    async (displayIndex: number) => {
      if (!remotePlaylistId) return;
      setBusy(true);
      setError(null);
      try {
        if (isCustomOrder) {
          await remotePlayPlaylist(remotePlaylistId, displayIndex);
        } else {
          await remotePlayTracks(
            displayTracks.map((t) => t.id),
            displayIndex,
          );
        }
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [remotePlaylistId, isCustomOrder, displayTracks],
  );

  const removeTrack = useCallback(
    async (index: number) => {
      if (!remotePlaylistId) return;
      setBusy(true);
      setError(null);
      try {
        await remoteRemovePlaylistTrack(remotePlaylistId, index);
        notifyRemoteChanged();
        await load();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [remotePlaylistId, load],
  );

  const toggleLike = useCallback((track: RemoteTrack) => {
    const next = !track.starred;
    // Optimistic: flip the heart at once; the write is durable locally and
    // travels on the next drain, so there's nothing to wait on.
    setTracks((prev) =>
      prev.map((t) => (t.id === track.id ? { ...t, starred: next } : t)),
    );
    remoteSetFavorite("track", track.id, next)
      .then(() => notifyRemoteChanged())
      .catch((err) => {
        console.error("[RemotePlaylistView] toggle like failed", err);
        setTracks((prev) =>
          prev.map((t) =>
            t.id === track.id ? { ...t, starred: track.starred } : t,
          ),
        );
      });
  }, []);

  const handleReorder = useCallback(
    (from: number, to: number) => {
      if (!remotePlaylistId || from === to) return;
      // Optimistic local move so the row settles before the server ack;
      // resync from the backend only if the write fails.
      setTracks((prev) => arrayMove(prev, from, to));
      remoteReorderPlaylistTrack(remotePlaylistId, from, to)
        .then(() => notifyRemoteChanged())
        .catch((err) => {
          setError(String(err));
          void load();
        });
    },
    [remotePlaylistId, load],
  );

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
  );
  const onDragStart = useCallback((event: DragStartEvent) => {
    setActiveId(String(event.active.id));
  }, []);
  const onDragCancel = useCallback(() => setActiveId(null), []);
  const onDragEnd = useCallback(
    (event: DragEndEvent) => {
      setActiveId(null);
      const { active, over } = event;
      if (!over || active.id === over.id) return;
      handleReorder(Number(active.id), Number(over.id));
    },
    [handleReorder],
  );

  // Virtualize the track list against the page scroller (same contract as
  // the local PlaylistView, and the repo's "virtual scroll everywhere"
  // invariant) so a large remote playlist doesn't mount thousands of rows.
  // SortableContext still holds the full index array, so dnd-kit knows the
  // ordering even for rows outside the window.
  const pageScrollRef = usePageScroll();
  const listParentRef = useRef<HTMLDivElement>(null);
  const [scrollMargin, setScrollMargin] = useState(0);
  useLayoutEffect(() => {
    const parent = listParentRef.current;
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
    // Also recompute when content *above* the list changes height without
    // resizing the list itself — the add-tracks panel opening, an error or
    // the playlist comment appearing, the sort bar toggling — since that
    // shifts the list's top within the scroller (which a ResizeObserver on
    // the list alone wouldn't catch).
  }, [
    pageScrollRef,
    displayTracks.length,
    adding,
    error,
    sortMode,
    summary?.comment,
  ]);

  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: displayTracks.length,
    getScrollElement: () => pageScrollRef?.current ?? null,
    estimateSize: () => REMOTE_ROW_HEIGHT,
    overscan: 8,
    scrollMargin,
  });

  // Debounced catalogue search while the add panel is open.
  const searchSeqRef = useRef(0);
  useEffect(() => {
    if (!adding) return;
    const q = query.trim();
    if (!q) {
      setResults([]);
      setSearching(false);
      return;
    }
    const seq = ++searchSeqRef.current;
    const timer = setTimeout(() => {
      // Flip the spinner on only when the request actually fires, not for
      // the whole debounce window on every keystroke.
      setSearching(true);
      remoteSearchCatalogue(q)
        .then((rows) => {
          if (seq === searchSeqRef.current) setResults(rows);
        })
        .catch((err) => {
          if (seq === searchSeqRef.current) {
            setError(String(err));
            setResults([]);
          }
        })
        .finally(() => {
          if (seq === searchSeqRef.current) setSearching(false);
        });
    }, 300);
    return () => clearTimeout(timer);
  }, [query, adding]);

  const addTracks = useCallback(
    async (ids: string[]) => {
      if (!remotePlaylistId || ids.length === 0) return;
      setBusy(true);
      setError(null);
      try {
        await remoteAddPlaylistTracks(remotePlaylistId, ids);
        notifyRemoteChanged();
        await load();
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [remotePlaylistId, load],
  );

  const commitRename = useCallback(async () => {
    const next = nameDraft.trim();
    if (!remotePlaylistId || !next || next === summary?.name) {
      setRenaming(false);
      return;
    }
    setBusy(true);
    try {
      await remoteUpdatePlaylist(remotePlaylistId, { name: next });
      notifyRemoteChanged();
      await load();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
      setRenaming(false);
    }
  }, [nameDraft, remotePlaylistId, summary?.name, load]);

  const handleDeleteClick = useCallback(async () => {
    if (!remotePlaylistId || busy) return;
    // First click arms the confirm, with a 3 s auto-revert — matches the
    // local PlaylistView so the two delete the same way.
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
    setBusy(true);
    try {
      // Redirect away before the delete so no not-found frame flashes.
      onAfterDelete();
      await remoteDeletePlaylist(remotePlaylistId);
      notifyRemoteChanged();
    } catch (err) {
      setError(String(err));
      setBusy(false);
      setConfirmDelete(false);
    }
  }, [remotePlaylistId, busy, confirmDelete, onAfterDelete]);

  if (!remotePlaylistId) return null;

  const totalMs = tracks.reduce((sum, t) => sum + (t.duration_ms ?? 0), 0);
  const color = colorForPlaylistId(remotePlaylistId);

  return (
    <div className="space-y-8 animate-fade-in pb-20">
      {/* Header — mirrors the local PlaylistView: a per-playlist colour
          band, a flat colour tile (no cover_path exists on a remote
          playlist, exactly like a local one without a custom cover), a
          4xl title, and a colour Play button beside a bordered action
          pill. */}
      <header
        className={`flex items-start justify-between p-6 rounded-2xl ${color.previewBg}`}
      >
        <div className="flex items-center space-x-6 min-w-0">
          {coverHashes.length >= 4 ? (
            <div className="w-24 h-24 rounded-2xl overflow-hidden shadow-sm shrink-0 grid grid-cols-2">
              {coverHashes.map((hash) => (
                <RemoteArtwork
                  key={hash}
                  hash={hash}
                  className="w-full h-full"
                  iconSize={16}
                />
              ))}
            </div>
          ) : coverHashes.length >= 1 ? (
            <RemoteArtwork
              hash={coverHashes[0]}
              className="w-24 h-24 rounded-2xl shadow-sm shrink-0"
              iconSize={40}
            />
          ) : (
            <div
              className={`w-24 h-24 rounded-2xl shadow-sm flex items-center justify-center shrink-0 ${color.tileBg} ${color.tileText}`}
            >
              <ListMusic size={48} />
            </div>
          )}
          <div className="min-w-0">
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("remote.playlist.label")}
            </div>
            {renaming ? (
              <input
                autoFocus
                value={nameDraft}
                aria-label={t("remote.playlist.nameLabel")}
                onChange={(e) => setNameDraft(e.target.value)}
                onBlur={() => void commitRename()}
                onKeyDown={(e) => {
                  // Ignore the Enter/Escape that closes an IME composition
                  // (Japanese, Chinese, …) — committing or cancelling on it
                  // would fire mid-word, before the character is chosen.
                  if (e.nativeEvent.isComposing) return;
                  if (e.key === "Enter") void commitRename();
                  if (e.key === "Escape") setRenaming(false);
                }}
                className="w-full mb-2 px-2 py-1 text-3xl font-bold rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900"
              />
            ) : (
              <h1 className="text-4xl font-bold mb-2 truncate text-zinc-900 dark:text-white">
                {summary?.name ?? "…"}
              </h1>
            )}
            {summary?.comment && (
              <p className="text-sm text-zinc-500 mb-2 line-clamp-2">
                {summary.comment}
              </p>
            )}
            <div className="flex items-center text-sm text-zinc-500 space-x-2">
              <Music2 size={16} />
              <span>
                {t("remote.playlist.trackCount", { count: tracks.length })}
              </span>
              <span>·</span>
              <span>{formatDuration(totalMs)}</span>
              {summary?.pending_creation && (
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
            onClick={() => void playFrom(0)}
            disabled={busy || tracks.length === 0}
            className={`text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm ${color.button} disabled:opacity-50 disabled:cursor-not-allowed`}
          >
            <Play size={16} className="fill-current" />
            <span>{t("remote.common.play")}</span>
          </button>

          <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
            <button
              type="button"
              onClick={() => setAdding((v) => !v)}
              disabled={busy || !summary}
              aria-label={t("remote.playlist.addTracks")}
              aria-pressed={adding}
              title={t("remote.playlist.addTracks")}
              className={`p-2 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
                adding
                  ? "bg-emerald-50 text-emerald-600 dark:bg-emerald-900/20 dark:text-emerald-400"
                  : "hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white"
              }`}
            >
              <ListPlus size={18} />
            </button>
            <button
              type="button"
              onClick={() => {
                setNameDraft(summary?.name ?? "");
                setRenaming(true);
              }}
              disabled={busy || !summary}
              aria-label={t("remote.playlist.rename")}
              className="p-2 rounded-lg transition-colors hover:bg-zinc-100 text-zinc-500 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-400 dark:hover:text-white disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Pencil size={18} />
            </button>
            <button
              type="button"
              onClick={() => void handleDeleteClick()}
              disabled={busy}
              aria-label={t("remote.playlist.delete")}
              title={
                confirmDelete
                  ? t("remote.playlist.confirmDelete")
                  : t("remote.playlist.delete")
              }
              className={`p-2 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
                confirmDelete
                  ? "bg-red-500 text-white hover:bg-red-600"
                  : "text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-500/10"
              }`}
            >
              <Trash2 size={18} />
            </button>
          </div>
        </div>
      </header>

      {adding && (
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
              placeholder={t("remote.playlist.searchPlaceholder")}
              className="w-full pl-9 pr-3 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900"
            />
            {searching && (
              <Loader2
                size={15}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-zinc-400 animate-spin"
              />
            )}
          </div>
          {query.trim() !== "" && !searching && results.length === 0 && (
            <p className="px-1 py-2 text-xs text-zinc-500">
              {t("remote.playlist.noMatches")}
            </p>
          )}
          {results.length > 0 && (
            <ul className="max-h-72 overflow-y-auto scrollbar-hide space-y-0.5">
              {results.map((track) => (
                <li
                  key={track.id}
                  className="group flex items-center gap-3 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
                >
                  <RemoteArtwork hash={track.artwork_hash} />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm truncate text-zinc-800 dark:text-zinc-100">
                      {track.title ?? t("remote.common.untitled")}
                    </div>
                    <div className="text-xs text-zinc-500 truncate">
                      {[track.artist, track.album].filter(Boolean).join(" — ")}
                    </div>
                  </div>
                  <button
                    type="button"
                    onClick={() => void addTracks([track.id])}
                    disabled={busy}
                    className="shrink-0 p-1.5 rounded-lg text-emerald-600 dark:text-emerald-400 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 disabled:opacity-50"
                    aria-label={t("remote.playlist.addTrack", {
                      title: track.title ?? t("remote.common.trackFallback"),
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

      {error && (
        <p className="text-xs text-red-600 dark:text-red-400 break-words">
          {error}
        </p>
      )}

      {!loading && tracks.length > 0 && (
        <div className="flex items-center justify-end -mt-4">
          <RemoteSortMenu current={sortMode} onChange={setSortMode} />
        </div>
      )}

      {loading ? (
        <div className="flex justify-center py-16">
          <Loader2 size={24} className="animate-spin text-zinc-400" />
        </div>
      ) : tracks.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400 py-8 text-center">
          {t("remote.playlist.empty")}
        </p>
      ) : (
        <div>
          {/* Column header — mirrors the local playlist table so the two
              read the same. Drag + remove columns are unlabeled. */}
          <div
            className={`grid ${GRID_COLS} gap-4 items-center px-5 py-3 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-200 dark:border-zinc-800`}
          >
            <span />
            <span className="text-right">#</span>
            <span />
            <span>{t("remote.common.colTitle")}</span>
            <span>{t("remote.common.colArtist")}</span>
            <span>{t("remote.common.colAlbum")}</span>
            <span className="flex justify-end">
              <Clock size={14} />
            </span>
            <span />
            <span />
          </div>
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            modifiers={[restrictToVerticalAxis]}
            // Always-measure pairs with virtualization: rows entering the
            // window during a drag-scroll get measured on the fly.
            measuring={{ droppable: { strategy: MeasuringStrategy.Always } }}
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
            onDragCancel={onDragCancel}
          >
            <SortableContext
              items={displayTracks.map((_, i) => String(i))}
              strategy={verticalListSortingStrategy}
            >
              <div
                ref={listParentRef}
                style={{
                  height: `${rowVirtualizer.getTotalSize()}px`,
                  position: "relative",
                  width: "100%",
                }}
              >
                {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                  const index = virtualRow.index;
                  const track = displayTracks[index];
                  if (!track) return null;
                  return (
                    <RemoteTrackRow
                      key={`${track.id}-${index}`}
                      id={String(index)}
                      track={track}
                      index={index + 1}
                      rowHeight={REMOTE_ROW_HEIGHT}
                      top={virtualRow.start - scrollMargin}
                      busy={busy}
                      dragEnabled={isCustomOrder && !busy}
                      isCurrent={
                        playingRemoteId != null && track.id === playingRemoteId
                      }
                      isPlaying={isPlaying}
                      onPlay={() => void playFrom(index)}
                      onRemove={() => void removeTrack(index)}
                      onNavigateToRemoteAlbum={onNavigateToRemoteAlbum}
                      onNavigateToRemoteArtist={onNavigateToRemoteArtist}
                      onToggleLike={() => toggleLike(track)}
                    />
                  );
                })}
              </div>
            </SortableContext>
            {/* Portal the overlay to <body> so a future `transform`/
                `backdrop-filter` ancestor can't become its containing block
                and pin it off-screen (repo overlay invariant). */}
            {createPortal(
              <DragOverlay dropAnimation={null}>
                {activeId != null && displayTracks[Number(activeId)] ? (
                  <RemoteRowPreview track={displayTracks[Number(activeId)]} />
                ) : null}
              </DragOverlay>,
              document.body,
            )}
          </DndContext>
        </div>
      )}
    </div>
  );
}

/** Column template shared by the header and the rows, mirroring the local
 *  playlist table: drag · # · cover · title · artist · album · time · like ·
 *  remove. Widths match `PlaylistView` verbatim (plus the trailing remove
 *  column, which the local view puts in a context menu we don't have). */
const GRID_COLS =
  "grid-cols-[1.5rem_3rem_2.75rem_1fr_1fr_1fr_5rem_2rem_2rem]";

/** Row height (px) the virtualizer estimates with — matches the old
 *  `h-14` (3.5rem = 56px), now set via inline style for absolute layout. */
const REMOTE_ROW_HEIGHT = 56;

const RemoteTrackRow = memo(function RemoteTrackRow({
  id,
  track,
  index,
  rowHeight,
  top,
  busy,
  dragEnabled,
  isCurrent,
  isPlaying,
  onPlay,
  onRemove,
  onNavigateToRemoteAlbum,
  onNavigateToRemoteArtist,
  onToggleLike,
}: {
  id: string;
  track: RemoteTrack;
  index: number;
  /** Slot height, driven by the virtualizer's estimate. */
  rowHeight: number;
  /** Absolute offset of this row's slot within the virtualized container,
   *  already corrected for the container's `scrollMargin`. */
  top: number;
  busy: boolean;
  /** False while a display sort is active — the grip and ×-remove are
   *  hidden and dnd-kit is detached so neither can act on a position that
   *  no longer matches the server's order. */
  dragEnabled: boolean;
  isCurrent: boolean;
  isPlaying: boolean;
  onPlay: () => void;
  onRemove: () => void;
  onNavigateToRemoteAlbum: (albumId: string) => void;
  onNavigateToRemoteArtist: (artistId: string) => void;
  onToggleLike: () => void;
}) {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useSortable({
      id,
      // Skip layout-shift animations: on a long virtualized list the
      // per-neighbour transitions are what make a drag feel sluggish.
      animateLayoutChanges: () => false,
      disabled: !dragEnabled,
    });
  // Position via CSS `top`, not a translateY transform: dnd-kit resolves
  // drop targets from `offsetTop`, which ignores transforms — anchoring
  // every row at the container top would collapse all collisions to the
  // first DOM row. useSortable's own transform (intra-drag displacement)
  // stays the only `transform` so it composes with `top` cleanly.
  const style: React.CSSProperties = {
    position: "absolute",
    top: `${top}px`,
    left: 0,
    width: "100%",
    height: `${rowHeight}px`,
    transform: CSS.Transform.toString(transform) || undefined,
    opacity: isDragging ? 0 : 1,
  };
  return (
    <div
      ref={setNodeRef}
      style={style}
      onDoubleClick={onPlay}
      className={`group grid ${GRID_COLS} gap-4 items-center px-5 border-b border-zinc-100 dark:border-zinc-800/60 transition-colors ${
        isCurrent
          ? "bg-emerald-50 dark:bg-emerald-900/20"
          : "hover:bg-zinc-50 dark:hover:bg-zinc-800/60"
      }`}
    >
      {/* Drag handle — appears on hover; PointerSensor only starts a sort
          past a 4px drag, so the row's other clicks still work. Hidden
          under a display sort (an empty slot keeps the columns aligned). */}
      {dragEnabled ? (
        <button
          type="button"
          {...attributes}
          {...listeners}
          aria-label={t("remote.playlist.reorder")}
          className="shrink-0 p-1 -ml-1 text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 group-hover:opacity-100 transition-opacity"
        >
          <GripVertical size={14} />
        </button>
      ) : (
        <span aria-hidden="true" />
      )}
      <span
        className={`text-right text-sm tabular-nums flex items-center justify-end ${
          isCurrent ? "text-emerald-500 font-semibold" : "text-zinc-400"
        }`}
      >
        {isCurrent ? (
          <PlayingIndicator isPlaying={isPlaying} />
        ) : (
          <>
            <span className="group-hover:hidden">{index}</span>
            {/* Playable even while awaiting metadata: the server streams by
                id, so a missing title only means we can't label it yet. */}
            <button
              type="button"
              onClick={onPlay}
              disabled={busy}
              className="hidden group-hover:inline-flex text-emerald-600 dark:text-emerald-400 disabled:opacity-40 disabled:cursor-not-allowed"
              aria-label={t("remote.common.play")}
            >
              <Play size={14} className="fill-current" />
            </button>
          </>
        )}
      </span>
      <RemoteArtwork hash={track.artwork_hash} className="w-10 h-10 rounded-md" />
      <span
        className={`min-w-0 text-sm truncate ${
          isCurrent
            ? "text-emerald-600 dark:text-emerald-400 font-semibold"
            : "text-zinc-800 dark:text-zinc-200"
        }`}
      >
        {track.title ?? t("remote.common.awaitingMetadata")}
      </span>
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        {track.artist && track.artist_id ? (
          <button
            type="button"
            onClick={() => onNavigateToRemoteArtist(track.artist_id!)}
            className="truncate max-w-full text-left hover:text-emerald-600 dark:hover:text-emerald-400 hover:underline"
            title={track.artist}
          >
            {track.artist}
          </button>
        ) : (
          (track.artist ?? "—")
        )}
      </div>
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        {track.album && track.album_id ? (
          <button
            type="button"
            onClick={() => onNavigateToRemoteAlbum(track.album_id!)}
            className="truncate max-w-full text-left hover:text-emerald-600 dark:hover:text-emerald-400 hover:underline"
            title={track.album}
          >
            {track.album}
          </button>
        ) : (
          (track.album ?? "—")
        )}
      </div>
      <span className="text-sm tabular-nums text-zinc-400 text-right">
        {track.duration_ms != null ? formatDuration(track.duration_ms) : "—"}
      </span>
      <div className="flex justify-center">
        <button
          type="button"
          onClick={onToggleLike}
          className={`p-1 rounded-full transition-colors ${
            track.starred
              ? "text-pink-500"
              : "text-zinc-300 dark:text-zinc-600 hover:text-pink-500"
          }`}
          aria-label={
            track.starred ? t("remote.common.unlike") : t("remote.common.like")
          }
          aria-pressed={track.starred}
        >
          <Heart size={14} className={track.starred ? "fill-current" : ""} />
        </button>
      </div>
      {/* Remove acts on the server position, so it's only offered in the
          curated order — hidden (empty slot) under a display sort. */}
      {dragEnabled ? (
        <button
          type="button"
          onClick={onRemove}
          disabled={busy}
          className="p-1 rounded text-zinc-300 dark:text-zinc-600 opacity-0 group-hover:opacity-100 hover:text-red-500 dark:hover:text-red-400 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
          aria-label={t("remote.playlist.remove")}
          title={t("remote.playlist.remove")}
        >
          <X size={15} />
        </button>
      ) : (
        <span aria-hidden="true" />
      )}
    </div>
  );
});

/** A lightweight copy of a row for the drag overlay: cover + title +
 *  artist, no controls. It follows the cursor while the in-place row (which
 *  can scroll out of the virtual window) stays hidden. */
function RemoteRowPreview({ track }: { track: RemoteTrack }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-3 px-5 h-14 rounded-lg bg-white dark:bg-zinc-800 shadow-lg ring-1 ring-black/5 dark:ring-white/10">
      <RemoteArtwork hash={track.artwork_hash} className="w-10 h-10 rounded-md" />
      <div className="min-w-0">
        <div className="text-sm truncate text-zinc-800 dark:text-zinc-200">
          {track.title ?? t("remote.common.awaitingMetadata")}
        </div>
        {track.artist && (
          <div className="text-xs text-zinc-500 truncate">{track.artist}</div>
        )}
      </div>
    </div>
  );
}

/**
 * Sort selector for the remote track table — the same Spotify-style
 * "list of modes + check, no direction toggle" menu as the local
 * PlaylistView, minus the modes a remote track can't back.
 */
function RemoteSortMenu({
  current,
  onChange,
}: {
  current: RemoteSortMode;
  onChange: (next: RemoteSortMode) => void;
}) {
  const { t } = useTranslation();
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
        <span>{t(REMOTE_SORT_LABEL_KEYS[current])}</span>
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
            {t("remote.playlist.sortBy")}
          </li>
          {REMOTE_SORT_MODES.map((mode) => {
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
                  <span>{t(REMOTE_SORT_LABEL_KEYS[mode])}</span>
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
