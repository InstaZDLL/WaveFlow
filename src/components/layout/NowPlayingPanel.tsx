import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Music2, Mic2 } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { enrichArtistDeezer } from "../../lib/tauri/detail";

interface NowPlayingPanelProps {
  onNavigateToArtist: (artistId: number) => void;
}

/**
 * Spotify-style right-edge panel showing the currently-playing track
 * with a large artwork, clickable artists, and an "About the artist"
 * section populated from the Deezer + Last.fm caches.
 *
 * Shares the w-80 right-edge slot with `QueuePanel` via mutual
 * exclusion in `PlayerContext` — opening one closes the other.
 *
 * Lyrics are NOT rendered here in v1. The `Mic` icon in `PlayerBar`
 * will open a separate lyrics panel once that feature lands.
 */
export function NowPlayingPanel({ onNavigateToArtist }: NowPlayingPanelProps) {
  const { t } = useTranslation();
  const { isNowPlayingOpen, toggleNowPlaying, currentTrack } = usePlayer();

  // Enrichment (picture + bio) for the current artist. Re-fetched
  // whenever the primary artist_id changes.
  const [pictureUrl, setPictureUrl] = useState<string | null>(null);
  const [bioShort, setBioShort] = useState<string | null>(null);
  const [bioFull, setBioFull] = useState<string | null>(null);
  const [bioExpanded, setBioExpanded] = useState(false);

  useEffect(() => {
    // Reset enrichment state whenever the focused artist changes so
    // stale bios don't flash during the async fetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPictureUrl(null);
    setBioShort(null);
    setBioFull(null);
    setBioExpanded(false);
    const artistId = currentTrack?.artist_id;
    if (artistId == null) return;
    let cancelled = false;
    enrichArtistDeezer(artistId)
      .then((e) => {
        if (cancelled) return;
        if (e.picture_url) setPictureUrl(e.picture_url);
        if (e.bio_short) setBioShort(e.bio_short);
        if (e.bio_full) setBioFull(e.bio_full);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [currentTrack?.artist_id]);

  const displayedBio = bioExpanded ? (bioFull ?? bioShort) : bioShort;
  const canExpand =
    bioFull != null && bioShort != null && bioFull.length > bioShort.length;

  const primaryArtistName = currentTrack?.artist_name?.split(", ")[0] ?? null;

  return (
    <div
      className={`absolute top-0 right-0 h-full w-80 shadow-2xl transform transition-transform duration-300 z-40 border-l bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-900 dark:border-zinc-800 dark:text-zinc-100
        ${isNowPlayingOpen ? "translate-x-0" : "translate-x-full"}`}
    >
      <div className="flex flex-col h-full overflow-y-auto">
        {/* Header */}
        <div className="flex items-center justify-between p-6 pb-4 sticky top-0 bg-white dark:bg-zinc-900 border-b border-zinc-100 dark:border-zinc-800 z-10">
          <h2 className="text-sm font-bold tracking-widest uppercase text-zinc-500 dark:text-zinc-400">
            {t("nowPlaying.title")}
          </h2>
          <button
            type="button"
            onClick={toggleNowPlaying}
            aria-label={t("common.close")}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
          >
            <X size={18} />
          </button>
        </div>

        {currentTrack ? (
          <div className="p-6 space-y-6">
            {/* Large artwork */}
            <Artwork
              path={currentTrack.artwork_path}
              alt={currentTrack.album_title ?? currentTrack.title}
              className="w-full aspect-square shadow-lg"
              iconSize={80}
              rounded="2xl"
            />

            {/* Track info */}
            <div className="space-y-1">
              <div className="text-xl font-bold text-zinc-900 dark:text-white leading-tight">
                {currentTrack.title}
              </div>
              <div className="text-sm text-zinc-500 dark:text-zinc-400">
                <ArtistLink
                  name={currentTrack.artist_name}
                  artistIds={currentTrack.artist_ids}
                  onNavigate={(id) => {
                    onNavigateToArtist(id);
                    toggleNowPlaying();
                  }}
                />
              </div>
              {currentTrack.album_title && (
                <div className="text-xs text-zinc-400">
                  {currentTrack.album_title}
                </div>
              )}
            </div>

            {/* About the artist */}
            {primaryArtistName && (
              <div className="space-y-3 pt-4 border-t border-zinc-100 dark:border-zinc-800">
                <h3 className="text-[10px] font-bold tracking-widest uppercase text-zinc-400">
                  {t("nowPlaying.aboutArtist")}
                </h3>
                <div className="flex items-center space-x-3">
                  {pictureUrl ? (
                    <img
                      src={pictureUrl}
                      alt={primaryArtistName}
                      loading="lazy"
                      className="w-14 h-14 rounded-full object-cover shrink-0"
                    />
                  ) : (
                    <div className="w-14 h-14 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 flex items-center justify-center shrink-0">
                      <span className="text-xl font-bold text-violet-500/70 dark:text-violet-400/60">
                        {primaryArtistName.trim().charAt(0).toUpperCase() || "?"}
                      </span>
                    </div>
                  )}
                  <button
                    type="button"
                    onClick={() => {
                      if (currentTrack.artist_id != null) {
                        onNavigateToArtist(currentTrack.artist_id);
                        toggleNowPlaying();
                      }
                    }}
                    className="text-sm font-semibold text-zinc-800 dark:text-zinc-100 hover:underline hover:text-emerald-600 dark:hover:text-emerald-400 text-left truncate"
                  >
                    {primaryArtistName}
                  </button>
                </div>
                {displayedBio ? (
                  <div className="space-y-2">
                    <p className="text-xs leading-relaxed text-zinc-600 dark:text-zinc-400 whitespace-pre-line">
                      {displayedBio}
                    </p>
                    {canExpand && (
                      <button
                        type="button"
                        onClick={() => setBioExpanded((p) => !p)}
                        className="text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:underline"
                      >
                        {bioExpanded
                          ? t("nowPlaying.readLess")
                          : t("nowPlaying.readMore")}
                      </button>
                    )}
                  </div>
                ) : (
                  <p className="text-xs italic text-zinc-400">
                    {t("nowPlaying.noBio")}
                  </p>
                )}
              </div>
            )}

            {/* Lyrics placeholder */}
            <div className="space-y-2 pt-4 border-t border-zinc-100 dark:border-zinc-800">
              <h3 className="text-[10px] font-bold tracking-widest uppercase text-zinc-400 flex items-center space-x-2">
                <Mic2 size={12} />
                <span>{t("nowPlaying.lyrics.title")}</span>
              </h3>
              <p className="text-xs italic text-zinc-400">
                {t("nowPlaying.lyrics.comingSoon")}
              </p>
            </div>
          </div>
        ) : (
          <div className="flex-1 flex flex-col items-center justify-center text-center px-6 py-12 text-zinc-400">
            <Music2 size={40} className="mb-3" />
            <p className="text-sm">{t("nowPlaying.empty")}</p>
          </div>
        )}
      </div>
    </div>
  );
}
