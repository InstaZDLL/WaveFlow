import { useEffect, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import {
  remoteGetArtist,
  type RemoteArtist,
} from "../../lib/tauri/remoteServer";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * A remote artist's detail view (RFC-005 sync_v2). Fetched live from the
 * server (`GET /api/v2/artists/{id}`): the artist image + their albums.
 * The server has no biography, so that stays a Last.fm-by-name concern of
 * the Now Playing panel, not this page.
 *
 * Not localized — behind the same off-by-default `sync_v2` feature.
 */
export function RemoteArtistView({
  remoteArtistId,
  onNavigateToRemoteAlbum,
}: {
  remoteArtistId: string | null;
  onNavigateToRemoteAlbum: (albumId: string) => void;
}) {
  const [artist, setArtist] = useState<RemoteArtist | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const seqRef = useRef(0);
  useEffect(() => {
    if (!remoteArtistId) return;
    const seq = ++seqRef.current;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    setError(null);
    remoteGetArtist(remoteArtistId)
      .then((a) => {
        if (seq === seqRef.current) setArtist(a);
      })
      .catch((err) => {
        if (seq === seqRef.current) setError(String(err));
      })
      .finally(() => {
        if (seq === seqRef.current) setLoading(false);
      });
  }, [remoteArtistId]);

  if (!remoteArtistId) return null;

  return (
    <div className="max-w-5xl mx-auto space-y-8">
      <header className="flex items-center gap-6">
        <RemoteArtwork
          hash={artist?.artwork_hash ?? null}
          className="w-32 h-32 rounded-full"
          iconSize={48}
        />
        <div className="min-w-0">
          <p className="text-[11px] font-bold uppercase tracking-widest text-zinc-400">
            Remote artist
          </p>
          <h1 className="text-4xl font-bold truncate text-zinc-900 dark:text-white">
            {artist?.name ?? "…"}
          </h1>
          <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
            {artist?.albums.length ?? 0} albums
          </p>
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
      ) : !artist || artist.albums.length === 0 ? (
        <p className="text-sm text-zinc-500 dark:text-zinc-400 py-8 text-center">
          No albums for this artist.
        </p>
      ) : (
        <section className="space-y-3">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
            Albums
          </div>
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-4">
            {artist.albums.map((album) => (
              <button
                key={album.id}
                type="button"
                onClick={() => onNavigateToRemoteAlbum(album.id)}
                className="group text-left"
              >
                <RemoteArtwork
                  hash={album.artwork_hash}
                  className="w-full aspect-square rounded-xl shadow-sm group-hover:shadow-md transition-shadow"
                  iconSize={32}
                />
                <div className="mt-2 text-sm font-medium truncate text-zinc-800 dark:text-zinc-100 group-hover:text-emerald-600 dark:group-hover:text-emerald-400">
                  {album.title}
                </div>
                {album.year != null && (
                  <div className="text-xs text-zinc-500">{album.year}</div>
                )}
              </button>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
