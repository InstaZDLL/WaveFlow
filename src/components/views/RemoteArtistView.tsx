import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2 } from "lucide-react";
import {
  remoteGetArtist,
  type RemoteArtist,
} from "../../lib/tauri/remoteServer";
import {
  enrichArtistByName,
  type DeezerArtistEnrichment,
} from "../../lib/tauri/detail";
import {
  getSimilarArtistsByName,
  type SimilarArtist,
} from "../../lib/tauri/similar";
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
 * Localized under the shared `remote.*` i18n namespace.
 */
export function RemoteArtistView({
  remoteArtistId,
  onNavigateToRemoteAlbum,
  onNavigateToArtist,
}: {
  remoteArtistId: string | null;
  onNavigateToRemoteAlbum: (albumId: string) => void;
  /** Open a local library artist — used by the similar-artist cards that
   *  resolved to a row the user already owns. */
  onNavigateToArtist: (artistId: number) => void;
}) {
  const { t, i18n } = useTranslation();
  const [artist, setArtist] = useState<RemoteArtist | null>(null);
  const [enrichment, setEnrichment] = useState<DeezerArtistEnrichment | null>(
    null,
  );
  const [similar, setSimilar] = useState<SimilarArtist[]>([]);
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
    // Clear the previous artist up front so its header / artwork / albums
    // can't linger (or stay clickable) while the new one loads.
    setArtist(null);
    setEnrichment(null);
    setSimilar([]);
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
        // Similar artists by name (Last.fm → Deezer), same as a library
        // artist. In-library matches carry a library_artist_id to link on.
        getSimilarArtistsByName(a.name)
          .then((list) => {
            if (seq === seqRef.current) setSimilar(list);
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
      ? t("remote.artist.fansCount", {
          count: enrichment.fans_count,
          // Group/abbreviate with the language the UI is actually rendering in,
          // not the browser's — they diverge whenever the user picks a locale.
          value: Intl.NumberFormat(i18n.language, {
            notation: "compact",
          }).format(enrichment.fans_count),
        })
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
              {t("remote.artist.label")}
            </p>
            <h1 className="text-4xl font-bold truncate text-zinc-900 dark:text-white">
              {artist?.name ?? "…"}
            </h1>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mt-1">
              {t("remote.artist.albumCount", {
                count: artist?.albums.length ?? 0,
              })}
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
            {t("remote.artist.biography")}
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
              {bioExpanded
                ? t("remote.artist.readLess")
                : t("remote.artist.readMore")}
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
          {t("remote.artist.noAlbums")}
        </p>
      ) : (
        <section className="space-y-3">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
            {t("remote.artist.albumsHeading")}
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

      {similar.length > 0 && (
        <section className="space-y-3">
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
            {t("remote.artist.similar")}
          </div>
          <div className="grid grid-cols-3 sm:grid-cols-4 md:grid-cols-6 gap-4">
            {similar.map((s) => {
              const src = resolveArtwork(
                { full: s.picture_path, remoteUrl: s.picture_url },
                "1x",
              );
              const inLibrary = s.library_artist_id != null;
              const body = (
                <>
                  {src ? (
                    <img
                      src={src}
                      alt={s.name}
                      loading="lazy"
                      className="w-full aspect-square rounded-full object-cover shadow-sm"
                    />
                  ) : (
                    <div className="w-full aspect-square rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 flex items-center justify-center">
                      <span className="text-2xl font-bold text-violet-500/70 dark:text-violet-400/60">
                        {s.name.trim().charAt(0).toUpperCase() || "?"}
                      </span>
                    </div>
                  )}
                  <div
                    className={`mt-2 text-sm font-medium text-center truncate ${
                      inLibrary
                        ? "text-zinc-800 dark:text-zinc-100 group-hover:text-emerald-600 dark:group-hover:text-emerald-400"
                        : "text-zinc-500 dark:text-zinc-400"
                    }`}
                  >
                    {s.name}
                  </div>
                </>
              );
              // In-library suggestions link to the local artist page; the
              // rest are discovery-only (no remote artist id to open).
              return inLibrary ? (
                <button
                  key={s.name}
                  type="button"
                  onClick={() => onNavigateToArtist(s.library_artist_id!)}
                  className="group text-left"
                  title={s.name}
                >
                  {body}
                </button>
              ) : (
                <div key={s.name} title={s.name}>
                  {body}
                </div>
              );
            })}
          </div>
        </section>
      )}
    </div>
  );
}
