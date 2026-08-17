import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { X, Music2, Radio } from "lucide-react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlayer } from "../../hooks/usePlayer";
import { useTrackCanvas } from "../../hooks/useTrackCanvas";
import {
  useCanvasEnabled,
  setCanvasEnabled,
} from "../../hooks/useCanvasEnabled";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { useAlbumMotionArtwork } from "../../hooks/useAlbumMotionArtwork";
import { useCoverSlideshow } from "../../hooks/useCoverSlideshow";
import { isRemoteTrack, isStreamTrack } from "../../lib/playerSources";
import {
  remoteArtwork,
  remoteGetArtist,
  remoteGetPlayQueue,
} from "../../lib/tauri/remoteServer";
import { Artwork } from "../common/Artwork";
import { MotionCoverOverlay } from "../player/MotionCoverOverlay";
import { CanvasStage } from "../player/CanvasStage";
import { CoverSlideshow } from "../player/CoverSlideshow";
import { CanvasToggleButton } from "../player/CanvasToggleButton";
import { ArtistLink } from "../common/ArtistLink";
import { Lightbox } from "../common/Lightbox";
import { enrichArtistByName, enrichArtistDeezer } from "../../lib/tauri/detail";
import { resolveArtwork } from "../../lib/tauri/artwork";
import {
  playerGetQueue,
  playerPlayTracks,
  type QueueTrackPayload,
} from "../../lib/tauri/player";
import { startRadio } from "../../lib/tauri/radio";

interface NowPlayingPanelProps {
  onNavigateToArtist: (artistId: number) => void;
  /** Navigate to a remote artist (RFC-005) — used when the current track
   *  is a remote stream, whose artist has a server id, not a local one. */
  onNavigateToRemoteArtist: (artistId: string) => void;
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
export function NowPlayingPanel({
  onNavigateToArtist,
  onNavigateToRemoteArtist,
}: NowPlayingPanelProps) {
  const { t } = useTranslation();
  const {
    toggleNowPlaying,
    toggleQueue,
    currentTrack,
    isNowPlayingOpen,
    activeProvider,
  } = usePlayer();

  // Enrichment (picture + bio) for the current artist. Re-fetched
  // whenever the primary artist_id changes.
  const [pictureSrc, setPictureSrc] = useState<string | null>(null);
  // Full-res artist photo for the cover slideshow (issue #466) — derived from
  // the SAME enrichment as `pictureSrc` (which resolves 1x for the bio
  // thumbnail), so there's no second `enrichArtistDeezer` fetch. Tagged with
  // the originating artist id so a rapid artist change can't flash the
  // previous artist's photo before the effect re-resolves (mirrors the
  // `useArtistImage` id-gate the immersive view uses).
  const [artistImageHi, setArtistImageHi] = useState<{
    id: number;
    src: string | null;
  } | null>(null);
  const [bioShort, setBioShort] = useState<string | null>(null);
  const [bioFull, setBioFull] = useState<string | null>(null);
  const [bioExpanded, setBioExpanded] = useState(false);
  const [isLightboxOpen, setIsLightboxOpen] = useState(false);
  const [nextTrack, setNextTrack] = useState<QueueTrackPayload | null>(null);

  // Pull the next queue item so we can mirror Spotify's "Next in queue"
  // teaser at the bottom of the panel. Refetched on track change and on
  // any backend-driven queue mutation.
  useEffect(() => {
    if (!isNowPlayingOpen) return;
    let cancelled = false;
    const fetchNext = () => {
      playerGetQueue()
        .then((q) => {
          if (cancelled) return;
          const idx = q.current_index;
          const upcoming =
            idx >= 0 && idx + 1 < q.items.length ? q.items[idx + 1] : null;
          setNextTrack(upcoming);
        })
        .catch(() => {
          if (!cancelled) setNextTrack(null);
        });
    };
    fetchNext();
    let unlisten: UnlistenFn | null = null;
    (async () => {
      try {
        unlisten = await listen("player:queue-changed", fetchNext);
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, [isNowPlayingOpen, currentTrack?.id]);

  useEffect(() => {
    // Reset enrichment state whenever the focused artist changes so
    // stale bios don't flash during the async fetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPictureSrc(null);
    setBioShort(null);
    setBioFull(null);
    setBioExpanded(false);
    const artistId = currentTrack?.artist_id;
    if (artistId == null) return;
    let cancelled = false;
    enrichArtistDeezer(artistId)
      .then((e) => {
        if (cancelled) return;
        const paths = {
          full: e.picture_path,
          x1: e.picture_path_1x,
          x2: e.picture_path_2x,
          remoteUrl: e.picture_url,
        };
        const resolved = resolveArtwork(paths, "1x");
        if (resolved) setPictureSrc(resolved);
        // "full" for the slideshow's large cover slot (a 1x thumbnail
        // upscales blurry there); same fetch, no extra network. Tagged with
        // `artistId` so a stale resolution can't drive the slideshow.
        setArtistImageHi({ id: artistId, src: resolveArtwork(paths, "full") });
        if (e.bio_short) setBioShort(e.bio_short);
        if (e.bio_full) setBioFull(e.bio_full);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [currentTrack?.artist_id]);

  // Remote-track artist (RFC-005). The synthesized remote Track has no
  // local artist_id, so the enrichment effect above bails; resolve the
  // server artist id from the live remote queue instead, and pull the
  // artist photo the server carries. Bio still needs Last.fm by name (the
  // panel's existing "set your key" message covers its absence).
  const isRemote = isRemoteTrack(currentTrack);
  const [remoteArtistId, setRemoteArtistId] = useState<string | null>(null);
  useEffect(() => {
    if (!isRemote) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setRemoteArtistId(null);
      return;
    }
    // Clear the previous remote artist's resolved state before re-resolving
    // — on a remote→remote change the local enrichment effect (keyed on the
    // always-null artist_id) never re-runs, so nothing else would.
    setRemoteArtistId(null);
    setPictureSrc(null);
    setBioShort(null);
    setBioFull(null);
    let cancelled = false;
    void (async () => {
      try {
        const queue = await remoteGetPlayQueue();
        if (cancelled || !queue) return;
        const artistId = queue.entries[queue.index]?.artist_id ?? null;
        setRemoteArtistId(artistId);

        // Photo + bio by name (Deezer / Last.fm) — same as a library
        // artist, since the server carries neither. The server's own
        // artwork_hash is the portrait fallback when Deezer has nothing.
        const name = currentTrack?.artist_name?.split(", ")[0] ?? null;
        let gotPicture = false;
        if (name) {
          const e = await enrichArtistByName(name);
          if (cancelled) return;
          const resolved = resolveArtwork(
            {
              full: e.picture_path,
              x1: e.picture_path_1x,
              x2: e.picture_path_2x,
              remoteUrl: e.picture_url,
            },
            "1x",
          );
          if (resolved) {
            setPictureSrc(resolved);
            gotPicture = true;
          }
          if (e.bio_short) setBioShort(e.bio_short);
          if (e.bio_full) setBioFull(e.bio_full);
        }
        if (!gotPicture && artistId) {
          const artist = await remoteGetArtist(artistId);
          if (cancelled || !artist.artwork_hash) return;
          const url = await remoteArtwork(artist.artwork_hash);
          if (!cancelled && url) setPictureSrc(url);
        }
      } catch (err) {
        console.error("[NowPlaying] resolve remote artist failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isRemote, currentTrack?.id, currentTrack?.artist_name]);

  const displayedBio = bioExpanded ? (bioFull ?? bioShort) : bioShort;
  const canExpand =
    bioFull != null && bioShort != null && bioFull.length > bioShort.length;

  const primaryArtistName = currentTrack?.artist_name?.split(", ")[0] ?? null;
  // Whether the current artist can be opened — a local id, or a resolved
  // remote id. Drives both the title-line link and the About-the-artist row.
  const canOpenArtist = isRemote
    ? remoteArtistId != null
    : currentTrack?.artist_id != null;
  const openCurrentArtist = () => {
    if (isRemote) {
      if (remoteArtistId != null) {
        onNavigateToRemoteArtist(remoteArtistId);
        toggleNowPlaying();
      }
      return;
    }
    if (currentTrack?.artist_id != null) {
      onNavigateToArtist(currentTrack.artist_id);
      toggleNowPlaying();
    }
  };

  // Per-track Canvas (issue #442) — mirrors the immersive view: the looping
  // clip replaces the static cover when set + toggle on + motion not reduced,
  // and the "Show Canvas" toggle only appears when the track has one.
  const canvasEnabled = useCanvasEnabled();
  const reducedMotion = usePrefersReducedMotion();
  const canvasPath = useTrackCanvas(currentTrack);
  const canvasActive = canvasEnabled && !reducedMotion && !!canvasPath;
  const canvasAvailable = !!canvasPath && !reducedMotion;

  // Cover ↔ artist slideshow (issue #466) — the ambient fallback, one rung
  // below the motion cover (Canvas > motion > slideshow > cover). Reuses the
  // artist photo already enriched above (`pictureSrc`).
  const slideshowEnabled = useCoverSlideshow().enabled;
  // `motionCover`, not `motion` — the latter is framer-motion's import here.
  const motionCover = useAlbumMotionArtwork(
    currentTrack?.artist_name,
    currentTrack?.album_title,
    currentTrack?.album_id,
  );
  // Radio / Spotify tracks have no library artist to slideshow (the artist
  // enrichment effect still runs to feed the "About the artist" bio, but the
  // slideshow itself stays off for them).
  const slideshowEligible =
    !!currentTrack &&
    activeProvider !== "spotify" &&
    !isStreamTrack(currentTrack);
  // Only surface the photo when it belongs to the current artist — a mismatch
  // (artist just changed, effect not re-resolved yet) reads as none.
  const artistSlideSrc =
    artistImageHi && artistImageHi.id === currentTrack?.artist_id
      ? artistImageHi.src
      : null;
  const slideshowActive =
    slideshowEnabled &&
    !reducedMotion &&
    !canvasActive &&
    !motionCover &&
    slideshowEligible &&
    !!artistSlideSrc;

  return (
    <motion.aside
      key="nowPlaying"
      initial={{ width: 0, opacity: 0 }}
      animate={{ width: 320, opacity: 1 }}
      exit={{ width: 0, opacity: 0 }}
      transition={{ type: "spring", stiffness: 320, damping: 32, mass: 0.8 }}
      className="h-full shrink-0 overflow-hidden border-l bg-surface-light border-zinc-200 text-zinc-800 dark:bg-surface-dark dark:border-zinc-800 dark:text-zinc-100"
    >
      <div className="flex flex-col h-full overflow-y-auto w-80">
        {/* Header */}
        <div className="flex items-center justify-between p-6 pb-4 sticky top-0 bg-surface-light dark:bg-surface-dark border-b border-zinc-100 dark:border-zinc-800 z-10">
          <h2 className="text-sm font-bold tracking-widest uppercase text-zinc-500 dark:text-zinc-400">
            {t("nowPlaying.title")}
          </h2>
          <div className="flex items-center gap-1">
            {canvasAvailable && (
              <CanvasToggleButton
                enabled={canvasEnabled}
                onToggle={() => setCanvasEnabled(!canvasEnabled)}
                size={18}
                className={`p-2 rounded-full transition-colors ${
                  canvasEnabled
                    ? "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                    : "text-zinc-500 hover:bg-zinc-100 dark:text-zinc-400 dark:hover:bg-zinc-800"
                }`}
              />
            )}
            <button
              type="button"
              onClick={toggleNowPlaying}
              aria-label={t("common.close")}
              className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            >
              <X size={18} />
            </button>
          </div>
        </div>

        {currentTrack ? (
          <div className="p-6 space-y-6">
            {/* Large artwork — keyboard-accessible lightbox trigger. The
                Canvas overlay lives on a shared relative wrapper so it still
                renders for a track that has a Canvas but no cover art. */}
            <div className="relative w-full">
              {currentTrack.artwork_path ? (
                <button
                  type="button"
                  onClick={() => setIsLightboxOpen(true)}
                  aria-label={t("common.viewArtwork")}
                  className="relative cursor-zoom-in focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded-2xl block w-full"
                >
                  <Artwork
                    path={currentTrack.artwork_path}
                    path1x={currentTrack.artwork_path_1x}
                    path2x={currentTrack.artwork_path_2x}
                    size="full"
                    alt={currentTrack.album_title ?? currentTrack.title}
                    className="w-full aspect-square shadow-lg"
                    iconSize={80}
                    rounded="2xl"
                  />
                  {!canvasActive && (
                    <MotionCoverOverlay
                      artist={currentTrack.artist_name}
                      album={currentTrack.album_title}
                      albumId={currentTrack.album_id}
                      rounded="2xl"
                      className="shadow-lg"
                    />
                  )}
                </button>
              ) : (
                <Artwork
                  path={currentTrack.artwork_path}
                  path1x={currentTrack.artwork_path_1x}
                  path2x={currentTrack.artwork_path_2x}
                  size="full"
                  alt={currentTrack.album_title ?? currentTrack.title}
                  className="w-full aspect-square shadow-lg"
                  iconSize={80}
                  rounded="2xl"
                />
              )}
              <CanvasStage
                path={canvasPath}
                enabled={canvasEnabled && !reducedMotion}
                rounded="2xl"
                className="shadow-lg"
              />
              <CoverSlideshow
                artistSrc={artistSlideSrc}
                enabled={slideshowActive}
                rounded="2xl"
                className="shadow-lg"
              />
            </div>

            {/* Track info */}
            <div className="space-y-1">
              <div className="text-xl font-bold text-zinc-900 dark:text-white leading-tight">
                {currentTrack.title}
              </div>
              <div className="text-sm text-zinc-500 dark:text-zinc-400">
                {isRemote ? (
                  canOpenArtist ? (
                    <button
                      type="button"
                      onClick={openCurrentArtist}
                      className="text-left hover:underline hover:text-emerald-600 dark:hover:text-emerald-400"
                    >
                      {currentTrack.artist_name}
                    </button>
                  ) : (
                    (currentTrack.artist_name ?? "")
                  )
                ) : (
                  <ArtistLink
                    name={currentTrack.artist_name}
                    artistIds={currentTrack.artist_ids}
                    onNavigate={(id) => {
                      onNavigateToArtist(id);
                      toggleNowPlaying();
                    }}
                  />
                )}
              </div>
              {currentTrack.album_title && (
                <div className="text-xs text-zinc-400">
                  {currentTrack.album_title}
                </div>
              )}
            </div>

            {/* Quick actions — Spotify's "Start radio" lives here too */}
            <button
              type="button"
              onClick={async () => {
                if (!currentTrack) return;
                try {
                  const ids = await startRadio(currentTrack.id);
                  if (ids.length > 0) {
                    await playerPlayTracks("radio", null, ids, 0);
                  }
                } catch (err) {
                  console.error("[NowPlaying] start radio failed", err);
                }
              }}
              className="w-full flex items-center justify-center gap-2 py-2.5 rounded-full border border-zinc-200 dark:border-zinc-700 hover:border-emerald-500 hover:text-emerald-500 text-sm font-semibold text-zinc-700 dark:text-zinc-200 transition-colors"
            >
              <Radio size={16} />
              {t("nowPlaying.startRadio")}
            </button>

            {/* About the artist */}
            {primaryArtistName && (
              <div className="space-y-3 pt-4 border-t border-zinc-100 dark:border-zinc-800">
                <h3 className="text-[10px] font-bold tracking-widest uppercase text-zinc-400">
                  {t("nowPlaying.aboutArtist")}
                </h3>
                <div className="flex items-center space-x-3">
                  {pictureSrc ? (
                    <img
                      src={pictureSrc}
                      alt={primaryArtistName}
                      loading="lazy"
                      className="w-14 h-14 rounded-full object-cover shrink-0"
                    />
                  ) : (
                    <div className="w-14 h-14 rounded-full bg-linear-to-br from-violet-100 to-violet-200 dark:from-violet-900/40 dark:to-violet-800/30 flex items-center justify-center shrink-0">
                      <span className="text-xl font-bold text-violet-500/70 dark:text-violet-400/60">
                        {primaryArtistName.trim().charAt(0).toUpperCase() ||
                          "?"}
                      </span>
                    </div>
                  )}
                  <button
                    type="button"
                    onClick={openCurrentArtist}
                    disabled={!canOpenArtist}
                    className="text-sm font-semibold text-zinc-800 dark:text-zinc-100 hover:underline hover:text-emerald-600 dark:hover:text-emerald-400 text-left truncate disabled:hover:no-underline disabled:hover:text-zinc-800 dark:disabled:hover:text-zinc-100 disabled:cursor-default"
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

            {/* Next in queue */}
            {nextTrack && (
              <div className="space-y-3 pt-4 border-t border-zinc-100 dark:border-zinc-800">
                <div className="flex items-center justify-between">
                  <h3 className="text-[10px] font-bold tracking-widest uppercase text-zinc-400">
                    {t("nowPlaying.nextInQueue")}
                  </h3>
                  <button
                    type="button"
                    onClick={() => {
                      toggleNowPlaying();
                      toggleQueue();
                    }}
                    className="text-[11px] font-semibold text-emerald-600 dark:text-emerald-400 hover:underline"
                  >
                    {t("nowPlaying.openQueue")}
                  </button>
                </div>
                <div className="flex items-center space-x-3">
                  <Artwork
                    path={nextTrack.artwork_path}
                    path1x={nextTrack.artwork_path_1x}
                    path2x={nextTrack.artwork_path_2x}
                    size="1x"
                    className="w-12 h-12 shrink-0"
                    iconSize={20}
                    alt={nextTrack.album_title ?? nextTrack.title}
                    rounded="md"
                  />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium truncate text-zinc-800 dark:text-zinc-200">
                      {nextTrack.title}
                    </div>
                    <div className="text-xs text-zinc-500 truncate">
                      {nextTrack.artist_name ?? "—"}
                    </div>
                  </div>
                </div>
              </div>
            )}
          </div>
        ) : (
          <div className="flex-1 flex flex-col items-center justify-center text-center px-6 py-12 text-zinc-400">
            <Music2 size={40} className="mb-3" />
            <p className="text-sm">{t("nowPlaying.empty")}</p>
          </div>
        )}
      </div>

      <Lightbox
        // resolveArtwork returns http / data URLs verbatim and only
        // convertFileSrc's a real local path — a remote track's inlined
        // `data:` cover would break under a bare convertFileSrc.
        src={resolveArtwork({ full: currentTrack?.artwork_path ?? null }, "full")}
        alt={currentTrack?.album_title ?? currentTrack?.title}
        isOpen={isLightboxOpen}
        onClose={() => setIsLightboxOpen(false)}
      />
    </motion.aside>
  );
}
