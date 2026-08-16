import { useCallback, useEffect, useRef, useState } from "react";
import {
  Clock,
  GripVertical,
  Loader2,
  ListMusic,
  ListPlus,
  Pencil,
  Play,
  Plus,
  Search,
  Trash2,
  X,
} from "lucide-react";
import {
  DndContext,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";
import { CSS } from "@dnd-kit/utilities";
import {
  remoteDeletePlaylist,
  remoteListPlaylists,
  remoteAddPlaylistTracks,
  remoteListPlaylistTracks,
  remotePlayPlaylist,
  remoteRemovePlaylistTrack,
  remoteReorderPlaylistTrack,
  remoteSearchCatalogue,
  remoteUpdatePlaylist,
  type RemotePlaylistSummary,
  type RemoteTrack,
} from "../../lib/tauri/remoteServer";
import { formatDuration } from "../../lib/tauri/track";
import { notifyRemoteChanged } from "../../hooks/useRemoteSource";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * A single remote-server playlist, managed like a local one but from the
 * projected `remote_*` cache (RFC-005 sync_v2).
 *
 * ## Not localized — deliberately
 *
 * This mounts only in a `sync_v2` build (off by default), so no shipped
 * build renders it. It shares that decision with {@link RemoteServerCard}:
 * translating provisional copy into seventeen native-reviewed locale
 * files would be churn for strings no user can reach yet. The keys land
 * with the feature.
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
}: {
  remotePlaylistId: string | null;
  onAfterDelete: () => void;
}) {
  const [summary, setSummary] = useState<RemotePlaylistSummary | null>(null);
  const [tracks, setTracks] = useState<RemoteTrack[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const [busy, setBusy] = useState(false);
  // Add-tracks panel: a live catalogue search with a "+" per hit.
  const [adding, setAdding] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<RemoteTrack[]>([]);
  const [searching, setSearching] = useState(false);

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
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load();
  }, [load]);

  // Play the playlist as a native remote queue from `index`. The tracks
  // after it auto-advance, and next / previous (PlayerBar, media keys)
  // drive the remote queue while it plays — the backend owns it.
  const playFrom = useCallback(
    async (index: number) => {
      if (!remotePlaylistId) return;
      setBusy(true);
      setError(null);
      try {
        await remotePlayPlaylist(remotePlaylistId, index);
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [remotePlaylistId],
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
  const onDragEnd = useCallback(
    (event: DragEndEvent) => {
      const { active, over } = event;
      if (!over || active.id === over.id) return;
      handleReorder(Number(active.id), Number(over.id));
    },
    [handleReorder],
  );

  // Debounced catalogue search while the add panel is open.
  const searchSeqRef = useRef(0);
  useEffect(() => {
    if (!adding) return;
    const q = query.trim();
    if (!q) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setResults([]);
      return;
    }
    const seq = ++searchSeqRef.current;
    setSearching(true);
    const timer = setTimeout(() => {
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

  const remove = useCallback(async () => {
    if (!remotePlaylistId) return;
    setBusy(true);
    try {
      await remoteDeletePlaylist(remotePlaylistId);
      notifyRemoteChanged();
      onAfterDelete();
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }, [remotePlaylistId, onAfterDelete]);

  if (!remotePlaylistId) return null;

  const totalMs = tracks.reduce((sum, t) => sum + (t.duration_ms ?? 0), 0);

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <header className="flex items-end gap-5">
        <div className="w-28 h-28 rounded-2xl bg-gradient-to-br from-emerald-500/80 to-teal-600/80 flex items-center justify-center shrink-0 shadow-lg">
          <ListMusic size={44} className="text-white/90" />
        </div>
        <div className="flex-1 min-w-0">
          <p className="text-[11px] font-bold uppercase tracking-widest text-zinc-400">
            Remote playlist
          </p>
          {renaming ? (
            <input
              autoFocus
              value={nameDraft}
              onChange={(e) => setNameDraft(e.target.value)}
              onBlur={() => void commitRename()}
              onKeyDown={(e) => {
                if (e.key === "Enter") void commitRename();
                if (e.key === "Escape") setRenaming(false);
              }}
              className="w-full mt-1 px-2 py-1 text-2xl font-bold rounded-lg border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900"
            />
          ) : (
            <h1 className="text-3xl font-bold truncate text-zinc-900 dark:text-white">
              {summary?.name ?? "…"}
            </h1>
          )}
          {summary?.comment && (
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1 line-clamp-2">
              {summary.comment}
            </p>
          )}
          <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
            {tracks.length} tracks · {formatDuration(totalMs)}
            {summary?.pending_creation && " · not sent to the server yet"}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <button
            type="button"
            onClick={() => void playFrom(0)}
            disabled={busy || tracks.length === 0}
            className="inline-flex items-center gap-1.5 px-4 py-2 rounded-full bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold disabled:opacity-50"
          >
            <Play size={16} className="fill-current" />
            Play
          </button>
          <button
            type="button"
            onClick={() => setAdding((v) => !v)}
            disabled={busy || !summary}
            className={`p-2 rounded-lg border disabled:opacity-50 ${
              adding
                ? "border-emerald-300 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/20 text-emerald-600 dark:text-emerald-400"
                : "border-zinc-200 dark:border-zinc-700 hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
            }`}
            aria-label="Add tracks"
            aria-pressed={adding}
            title="Add tracks"
          >
            <ListPlus size={16} />
          </button>
          <button
            type="button"
            onClick={() => {
              setNameDraft(summary?.name ?? "");
              setRenaming(true);
            }}
            disabled={busy || !summary}
            className="p-2 rounded-lg border border-zinc-200 dark:border-zinc-700 hover:bg-zinc-50 dark:hover:bg-zinc-800/40 disabled:opacity-50"
            aria-label="Rename"
          >
            <Pencil size={16} />
          </button>
          <button
            type="button"
            onClick={() => void remove()}
            disabled={busy}
            className="p-2 rounded-lg border border-red-200 dark:border-red-900/50 text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/30 disabled:opacity-50"
            aria-label="Delete"
          >
            <Trash2 size={16} />
          </button>
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
              placeholder="Search the server's catalogue…"
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
            <p className="px-1 py-2 text-xs text-zinc-500">No matches.</p>
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
                      {track.title ?? "Untitled"}
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
                    aria-label={`Add ${track.title ?? "track"}`}
                    title="Add to playlist"
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

      {loading ? (
        <div className="flex justify-center py-16">
          <Loader2 size={24} className="animate-spin text-zinc-400" />
        </div>
      ) : tracks.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400 py-8 text-center">
          This playlist is empty.
        </p>
      ) : (
        <div>
          {/* Column header — mirrors the local playlist table so the two
              read the same. Drag + remove columns are unlabeled. */}
          <div
            className={`grid ${GRID_COLS} gap-3 items-center px-3 pb-2 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-200 dark:border-zinc-800`}
          >
            <span />
            <span className="text-right">#</span>
            <span />
            <span>Title</span>
            <span>Artist</span>
            <span>Album</span>
            <span className="flex justify-end">
              <Clock size={13} />
            </span>
            <span />
          </div>
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            modifiers={[restrictToVerticalAxis]}
            onDragEnd={onDragEnd}
          >
            <SortableContext
              items={tracks.map((_, i) => String(i))}
              strategy={verticalListSortingStrategy}
            >
              <ul className="mt-1">
                {tracks.map((track, index) => (
                  <RemoteTrackRow
                    key={`${track.id}-${index}`}
                    id={String(index)}
                    track={track}
                    index={index + 1}
                    busy={busy}
                    onPlay={() => void playFrom(index)}
                    onRemove={() => void removeTrack(index)}
                  />
                ))}
              </ul>
            </SortableContext>
          </DndContext>
        </div>
      )}
    </div>
  );
}

/** Column template shared by the header and the rows, mirroring the local
 *  playlist table: drag · # · cover · title · artist · album · time · remove. */
const GRID_COLS =
  "grid-cols-[1.25rem_1.5rem_2.5rem_minmax(0,2fr)_minmax(0,1.4fr)_minmax(0,1.4fr)_3.5rem_1.5rem]";

function RemoteTrackRow({
  id,
  track,
  index,
  busy,
  onPlay,
  onRemove,
}: {
  id: string;
  track: RemoteTrack;
  index: number;
  busy: boolean;
  onPlay: () => void;
  onRemove: () => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } =
    useSortable({ id });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.6 : 1,
  };
  return (
    <li
      ref={setNodeRef}
      style={style}
      className={`group grid ${GRID_COLS} gap-3 items-center px-3 h-12 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/40`}
    >
      {/* Drag handle — appears on hover; PointerSensor only starts a sort
          past a 4px drag, so the row's other clicks still work. */}
      <button
        type="button"
        {...attributes}
        {...listeners}
        aria-label="Reorder"
        className="text-zinc-300 dark:text-zinc-600 hover:text-zinc-500 dark:hover:text-zinc-400 cursor-grab active:cursor-grabbing opacity-0 group-hover:opacity-100 transition-opacity"
      >
        <GripVertical size={14} />
      </button>
      <div className="text-right text-xs text-zinc-400 tabular-nums">
        <span className="group-hover:hidden">{index}</span>
        {/* Playable even while awaiting metadata: the server streams by
            id, so a missing title only means we can't label it yet. */}
        <button
          type="button"
          onClick={onPlay}
          disabled={busy}
          className="hidden group-hover:inline-flex text-emerald-600 dark:text-emerald-400 disabled:opacity-40 disabled:cursor-not-allowed"
          aria-label="Play"
        >
          <Play size={14} className="fill-current" />
        </button>
      </div>
      <RemoteArtwork hash={track.artwork_hash} className="w-9 h-9" />
      <div className="min-w-0 text-sm font-medium truncate text-zinc-800 dark:text-zinc-100">
        {track.title ?? "Awaiting metadata…"}
      </div>
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        {track.artist ?? "—"}
      </div>
      <div className="min-w-0 text-sm text-zinc-500 truncate">
        {track.album ?? "—"}
      </div>
      <div className="text-right text-xs text-zinc-400 tabular-nums">
        {track.duration_ms != null ? formatDuration(track.duration_ms) : "—"}
      </div>
      <button
        type="button"
        onClick={onRemove}
        disabled={busy}
        className="p-1 rounded text-zinc-300 dark:text-zinc-600 opacity-0 group-hover:opacity-100 hover:text-red-500 dark:hover:text-red-400 transition-opacity disabled:opacity-40 disabled:cursor-not-allowed"
        aria-label="Remove from playlist"
        title="Remove from playlist"
      >
        <X size={15} />
      </button>
    </li>
  );
}
