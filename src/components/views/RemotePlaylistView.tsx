import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, ListMusic, Pencil, Play, Trash2 } from "lucide-react";
import {
  remoteArtwork,
  remoteDeletePlaylist,
  remoteListPlaylists,
  remoteListPlaylistTracks,
  remoteStreamUrl,
  remoteUpdatePlaylist,
  type RemotePlaylistSummary,
  type RemoteTrack,
} from "../../lib/tauri/remoteServer";
import { playerPlayUrl } from "../../lib/tauri/player";
import { formatDuration } from "../../lib/tauri/track";
import { notifyRemoteChanged } from "../../hooks/useRemoteSource";

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
 * ## Playback is per-track for now
 *
 * Step 1 plays each track through the single-URL radio path
 * ({@link playerPlayUrl}) — proven end to end in Phase A. Queue-aware
 * native playback (play-all, next/prev, auto-advance) is Step 2, which
 * teaches the engine a finite "remote track" queue entry.
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

  const playTrack = useCallback(async (track: RemoteTrack) => {
    if (track.title == null) return; // metadata not cached yet
    setBusy(true);
    setError(null);
    try {
      const url = await remoteStreamUrl(track.id);
      await playerPlayUrl({
        url,
        title: track.title ?? undefined,
        artist: track.artist ?? undefined,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }, []);

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
          <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
            {tracks.length} tracks · {formatDuration(totalMs)}
            {summary?.pending_creation && " · not sent to the server yet"}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
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
        <ul className="space-y-0.5">
          {tracks.map((track, index) => (
            <RemoteTrackRow
              key={`${track.id}-${index}`}
              track={track}
              index={index + 1}
              busy={busy}
              onPlay={() => void playTrack(track)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function RemoteTrackRow({
  track,
  index,
  busy,
  onPlay,
}: {
  track: RemoteTrack;
  index: number;
  busy: boolean;
  onPlay: () => void;
}) {
  const pending = track.title == null;
  return (
    <li className="group flex items-center gap-3 px-2 py-1.5 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/40">
      <div className="w-6 text-right text-xs text-zinc-400 tabular-nums shrink-0">
        <span className="group-hover:hidden">{index}</span>
        <button
          type="button"
          onClick={onPlay}
          disabled={busy || pending}
          className="hidden group-hover:inline-flex text-emerald-600 dark:text-emerald-400 disabled:opacity-40 disabled:cursor-not-allowed"
          aria-label="Play"
        >
          <Play size={14} className="fill-current" />
        </button>
      </div>
      <RemoteArtwork hash={track.artwork_hash} />
      <div className="flex-1 min-w-0">
        <div className="text-sm font-medium truncate text-zinc-800 dark:text-zinc-100">
          {track.title ?? "Awaiting metadata…"}
        </div>
        <div className="text-xs text-zinc-500 truncate">
          {[track.artist, track.album].filter(Boolean).join(" — ")}
        </div>
      </div>
      <div className="text-xs text-zinc-400 tabular-nums shrink-0">
        {track.duration_ms != null ? formatDuration(track.duration_ms) : "—"}
      </div>
    </li>
  );
}

/**
 * The artwork endpoint is Bearer-only, so a bare `<img src>` would 401.
 * Fetch it once as a `data:` URL and cache it in state; fall back to a
 * neutral tile while it resolves or when there is no hash.
 */
function RemoteArtwork({ hash }: { hash: string | null }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    if (!hash) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSrc(null);
      return;
    }
    let cancelled = false;
    remoteArtwork(hash)
      .then((url) => {
        if (!cancelled) setSrc(url);
      })
      .catch(() => {
        if (!cancelled) setSrc(null);
      });
    return () => {
      cancelled = true;
    };
  }, [hash]);
  if (!src) {
    return (
      <div className="w-9 h-9 rounded bg-zinc-200 dark:bg-zinc-700 flex items-center justify-center shrink-0">
        <ListMusic size={14} className="text-zinc-400" />
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      className="w-9 h-9 rounded object-cover shrink-0"
      loading="lazy"
    />
  );
}
