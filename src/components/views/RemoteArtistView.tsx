import { useEffect, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import {
  remoteGetArtist,
  type RemoteArtist,
} from "../../lib/tauri/remoteServer";
import {
  enrichArtistByName,
  type DeezerArtistEnrichment,
} from "../../lib/tauri/detail";
import { resolveArtwork } from "../../lib/tauri/artwork";
import { RemoteArtwork } from "../common/RemoteArtwork";

/**
 * A remote artist's detail view (RFC-005 sync_v2). The server (`GET
 * /api/v2/artists/{id}`) carries the artist's albums but rarely a photo
 * and never a biography, so — exactly like a library artist — the photo,
 * the hero background and the bio come from the by-name enrichment
 * (Deezer + TheAudioDB + Last.fm). The server's own `artwork_hash`, when
 * present, is the fallback for the portrait.
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
  const [enrichment, setEnrichment] = useState<DeezerArtistEnrichment | null>(
    null,
  );
  const [bioExpanded, setBioExpanded] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const seqRef = useRef(0);
  useEffect(() => {
    if (!remoteArtistId) return;
    const seq = ++seqRef.current;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    setError(null);
    setEnrichment(null);
    setBioExpanded(false);
    remoteGetArtist(remoteArtistId)
      .then((a) => {
        if (seq !== seqRef.current) return;
        setArtist(a);
        // Photo / background / bio by name — the server has none of them.
        enrichArtistByName(a.name)
          .then((e) => {
            if (seq === seqRef.current) setEnrichment(e);
          })
          .catch(() => {});
      })
      .catch((err) => {
        if (seq === seqRef.current) setError(String(err));
      })
      .finally(() => {
        if (seq === seqRef.current) setLoading(false);
      });
  }, [remoteArtistId]);

  if (!remoteArtistId) return null;

  const pictureSrc = enrichment
    ? resolveArtwork(
        {
          full: enrichment.picture_path,
          x1: enrichment.picture_path_1x,
          x2: enrichment.picture_path_2x,
          remoteUrl: enrichment.picture_url,
        },
        "full",
      )
    : null;
  const backgroundSrc = enrichment
    ? resolveArtwork(
        {
          full: enrichment.background_path,
          remoteUrl: enrichment.background_url,
        },
        "full",
      )
    : null;
  const bioShort = enrichment?.bio_short ?? null;
  const bioFull = enrichment?.bio_full ?? null;
  const displayedBio = bioExpanded ? (bioFull ?? bioShort) : bioShort;
  const canExpand =
    bioFull != null && bioShort != null && bioFull.length > bioShort.length;
  const fansLabel =
    enrichment?.fans_count != null
      ? `${Intl.NumberFormat(undefined, { notation: "compact" }).format(
          enrichment.fans_count,
        )} fans`
      : null;

  return (
    <div className="max-w-5xl mx-auto space-y-8">
      {/* Hero — the TheAudioDB fanart paints full-bleed behind the header,
          the same treatment as a library artist. Falls back to a flat
          band when there's no background. */}
      <header className="relative overflow-hidden rounded-2xl">
        {backgroundSrc ? (
          <>
            <img
              src={backgroundSrc}
              alt=""
              aria-hidden="true"
              className="absolute inset-0 w-full h-full object-cover"
              loading="lazy"
            />
            <div className="absolute inset-0 bg-linear-to-t from-white via-white/85 to-white/40 dark:from-surface-dark dark:via-surface-dark/85 dark:to-surface-dark/40" />
          </>
        ) : (
          <div className="absolute inset-0 bg-zinc-50 dark:bg-zinc-900/40" />
        )}
        <div className="relative flex items-center gap-6 p-6">
          {pictureSrc ? (
            <img
              src={pictureSrc}
              alt={artist?.name ?? ""}
              className="w-32 h-32 rounded-full object-cover shadow-lg shrink-0"
              loading="lazy"
            />
          ) : (
            <RemoteArtwork
              hash={artist?.artwork_hash ?? null}
              className="w-32 h-32 rounded-full"
              iconSize={48}
            />
          )}
          <div className="min-w-0">
            <p className="text-[11px] font-bold uppercase tracking-widest text-zinc-400">
              Remote artist
            </p>
            <h1 className="text-4xl font-bold truncate text-zinc-900 dark:text-white">
              {artist?.name ?? "…"}
            </h1>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
              {artist?.albums.length ?? 0} albums
              {fansLabel && ` · ${fansLabel}`}
            </p>
          </div>
        </div>
      </header>

      {error && (
        <p className="text-xs text-red-600 dark:text-red-400 break-words">
          {error}
        </p>
      )}

      {displayedBio && (
        <section className="space-y-2">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
            Biography
          </div>
          <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400 whitespace-pre-line max-w-3xl">
            {displayedBio}
          </p>
          {canExpand && (
            <button
              type="button"
              onClick={() => setBioExpanded((p) => !p)}
              className="text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:underline"
            >
              {bioExpanded ? "Read less" : "Read more"}
            </button>
          )}
        </section>
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
