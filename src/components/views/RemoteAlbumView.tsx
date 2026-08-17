import { useCallback, useEffect, useRef, useState } from "react";
import { Clock, Heart, Loader2, Play } from "lucide-react";
import {
  remoteGetAlbum,
  remotePlayTracks,
  remoteSetFavorite,
  type RemoteAlbum,
  type RemoteTrack,
} from "../../lib/tauri/remoteServer";
import { formatDuration } from "../../lib/tauri/track";
import { notifyRemoteChanged } from "../../hooks/useRemoteSource";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * A remote album's detail view (RFC-005 sync_v2). Fetched live from the
 * server (`GET /api/v2/albums/{id}`); its tracks play as a native remote
 * queue. Reached by clicking an album in the remote playlist table.
 *
 * Not localized — behind the same off-by-default `sync_v2` feature as the
 * rest of the remote surface.
 */
export function RemoteAlbumView({
  remoteAlbumId,
  onNavigateToRemoteArtist,
}: {
  remoteAlbumId: string | null;
  onNavigateToRemoteArtist: (artistId: string) => void;
}) {
  const [album, setAlbum] = useState<RemoteAlbum | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const seqRef = useRef(0);
  useEffect(() => {
    if (!remoteAlbumId) return;
    const seq = ++seqRef.current;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    setError(null);
    remoteGetAlbum(remoteAlbumId)
      .then((a) => {
        if (seq === seqRef.current) setAlbum(a);
      })
      .catch((err) => {
        if (seq === seqRef.current) setError(String(err));
      })
      .finally(() => {
        if (seq === seqRef.current) setLoading(false);
      });
  }, [remoteAlbumId]);

  const playFrom = useCallback(
    async (index: number) => {
      if (!album) return;
      setBusy(true);
      setError(null);
      try {
        await remotePlayTracks(
          album.tracks.map((t) => t.id),
          index,
        );
      } catch (err) {
        setError(String(err));
      } finally {
        setBusy(false);
      }
    },
    [album],
  );

  const toggleLike = useCallback((track: RemoteTrack) => {
    const next = !track.starred;
    setAlbum((prev) =>
      prev
        ? {
            ...prev,
            tracks: prev.tracks.map((t) =>
              t.id === track.id ? { ...t, starred: next } : t,
            ),
          }
        : prev,
    );
    remoteSetFavorite("track", track.id, next)
      .then(() => notifyRemoteChanged())
      .catch((err) => {
        console.error("[RemoteAlbumView] toggle like failed", err);
        setAlbum((prev) =>
          prev
            ? {
                ...prev,
                tracks: prev.tracks.map((t) =>
                  t.id === track.id ? { ...t, starred: track.starred } : t,
                ),
              }
            : prev,
        );
      });
  }, []);

  if (!remoteAlbumId) return null;

  const totalMs =
    album?.tracks.reduce((sum, t) => sum + (t.duration_ms ?? 0), 0) ?? 0;

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <header className="flex items-center gap-5 p-5 rounded-2xl bg-emerald-50/70 dark:bg-emerald-900/15">
        <RemoteArtwork
          hash={album?.artwork_hash ?? null}
          className="w-28 h-28 rounded-2xl shadow-lg"
          iconSize={44}
        />
        <div className="flex-1 min-w-0">
          <p className="text-[11px] font-bold uppercase tracking-widest text-zinc-400">
            Remote album
          </p>
          <h1 className="text-3xl font-bold truncate text-zinc-900 dark:text-white">
            {album?.title ?? "…"}
          </h1>
          <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
            {album?.artist && album.artist_id ? (
              <button
                type="button"
                onClick={() => onNavigateToRemoteArtist(album.artist_id!)}
                className="hover:text-emerald-600 dark:hover:text-emerald-400 hover:underline"
              >
                {album.artist}
              </button>
            ) : (
              album?.artist
            )}
            {album?.year != null && (
              <>
                {album?.artist ? " · " : ""}
                {album.year}
              </>
            )}
          </p>
          <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-0.5">
            {album?.tracks.length ?? 0} tracks · {formatDuration(totalMs)}
          </p>
        </div>
        <button
          type="button"
          onClick={() => void playFrom(0)}
          disabled={busy || !album || album.tracks.length === 0}
          className="shrink-0 inline-flex items-center gap-1.5 px-4 py-2 rounded-full bg-emerald-500 hover:bg-emerald-600 text-white text-sm font-semibold disabled:opacity-50"
        >
          <Play size={16} className="fill-current" />
          Play
        </button>
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
      ) : !album || album.tracks.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400 py-8 text-center">
          This album has no tracks.
        </p>
      ) : (
        <div>
          <div className="grid grid-cols-[1.5rem_minmax(0,3fr)_minmax(0,1.6fr)_3.5rem_1.5rem] gap-3 items-center px-3 pb-2 text-[10px] font-bold tracking-widest text-zinc-400 uppercase border-b border-zinc-200 dark:border-zinc-800">
            <span className="text-right">#</span>
            <span>Title</span>
            <span>Artist</span>
            <span className="flex justify-end">
              <Clock size={13} />
            </span>
            <span />
          </div>
          <ul className="mt-1">
            {album.tracks.map((track, index) => (
              <li
                key={`${track.id}-${index}`}
                className="group grid grid-cols-[1.5rem_minmax(0,3fr)_minmax(0,1.6fr)_3.5rem_1.5rem] gap-3 items-center px-3 h-11 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/40"
              >
                <div className="text-right text-xs text-zinc-400 tabular-nums">
                  <span className="group-hover:hidden">{index + 1}</span>
                  <button
                    type="button"
                    onClick={() => void playFrom(index)}
                    disabled={busy}
                    className="hidden group-hover:inline-flex text-emerald-600 dark:text-emerald-400 disabled:opacity-40"
                    aria-label="Play"
                  >
                    <Play size={14} className="fill-current" />
                  </button>
                </div>
                <div className="min-w-0 text-sm font-medium truncate text-zinc-800 dark:text-zinc-100">
                  {track.title ?? "Awaiting metadata…"}
                </div>
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
                <div className="text-right text-xs text-zinc-400 tabular-nums">
                  {track.duration_ms != null
                    ? formatDuration(track.duration_ms)
                    : "—"}
                </div>
                <button
                  type="button"
                  onClick={() => toggleLike(track)}
                  className={`p-1 rounded transition-colors ${
                    track.starred
                      ? "text-pink-500"
                      : "text-zinc-300 dark:text-zinc-600 opacity-0 group-hover:opacity-100 hover:text-pink-500"
                  }`}
                  aria-label={track.starred ? "Unlike" : "Like"}
                  aria-pressed={track.starred}
                >
                  <Heart
                    size={15}
                    className={track.starred ? "fill-current" : ""}
                  />
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
