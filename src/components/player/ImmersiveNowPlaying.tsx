import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Heart, Star, Radio } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { MotionCoverOverlay } from "./MotionCoverOverlay";
import { CanvasStage } from "./CanvasStage";
import { CoverSlideshow } from "./CoverSlideshow";
import { ArtistLink } from "../common/ArtistLink";
import { MarqueeText } from "../common/MarqueeText";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";
import { SpectrumVisualizer } from "./SpectrumVisualizer";
import { VisualizerColorButton } from "./VisualizerColorButton";
import { usePlayer } from "../../hooks/usePlayer";
import { useVisualizerColor } from "../../hooks/useVisualizerColor";
import { getVisualizerEnabled } from "../../lib/tauri/visualizer";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import { usePlayerTrackContextMenu } from "../../hooks/usePlayerTrackContextMenu";
import { useTrackCanvas } from "../../hooks/useTrackCanvas";
import { useCanvasEnabled } from "../../hooks/useCanvasEnabled";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { useAlbumMotionArtwork } from "../../hooks/useAlbumMotionArtwork";
import { useCoverSlideshow } from "../../hooks/useCoverSlideshow";
import { useArtistImage } from "../../hooks/useArtistImage";
import { isRadioTrack, isStreamTrack } from "../../lib/playerSources";

interface ImmersiveNowPlayingProps {
  /** Dismisses the immersive view (used after an artist navigation). */
  onClose: () => void;
  onNavigateToArtist: (artistId: number) => void;
  isLiked: boolean;
  onToggleLike: () => void;
}

/**
 * Left column of the immersive view (issue #328): the cover hero, track
 * metadata, spectrum visualizer, progress bar, and transport controls.
 * Lifted from the old `FullscreenNowPlaying` body minus its top bar —
 * the orchestrator (`ImmersiveView`) owns the shared close / share /
 * lyrics-toggle chrome so it isn't duplicated per column.
 *
 * Fills its flex parent at full height and centres the hero, with the
 * transport pinned to the bottom, so the column reads the same whether
 * it shares the screen with the lyrics column or stands alone.
 */
export function ImmersiveNowPlaying({
  onClose,
  onNavigateToArtist,
  isLiked,
  onToggleLike,
}: ImmersiveNowPlayingProps) {
  const { t } = useTranslation();
  const { currentTrack, currentRadioStation, activeProvider } = usePlayer();
  // Right-click the title to reach the same track menu the list views have
  // (Show in Explorer, Properties, queue ops…) — mail reporter request.
  // Only for a real library track: radio (negative sentinel id), Spotify
  // playback, and any streamed track with no local file have nothing the
  // file-oriented actions can act on, so require a real `file_path`.
  const trackMenu = usePlayerTrackContextMenu();
  const menuTrack =
    currentTrack &&
    activeProvider !== "spotify" &&
    !isRadioTrack(currentTrack) &&
    !!currentTrack.file_path
      ? currentTrack
      : null;
  // Live radio: favorite the STATION (★) instead of liking a track (♥) —
  // a radio session has a negative sentinel id with no library row to
  // like. Mirrors the PlayerBar / mini-player treatment.
  const radioFavorites = useWebRadioFavorites();
  const stationFavorited =
    currentRadioStation != null &&
    radioFavorites.isFavorite(currentRadioStation.id);

  // Spectrum-visualizer colour (issue #468). The cycle button only shows when
  // the visualizer itself is enabled (a per-profile backend toggle read once
  // on mount — the immersive view remounts each time it's opened). The chosen
  // colour feeds the visualizer's fill; `rainbow` tints per bar.
  const {
    colorId,
    color: visualizerColor,
    rainbow,
    ready: visualizerColorReady,
    cycle,
  } = useVisualizerColor();
  const [visualizerOn, setVisualizerOn] = useState(false);
  useEffect(() => {
    let cancelled = false;
    getVisualizerEnabled()
      .then((on) => {
        if (!cancelled) setVisualizerOn(on);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  // Per-track Canvas (issue #442) — a looping clip replaces the static cover
  // when one is set, the global toggle is on, and motion isn't reduced. It
  // takes precedence over the plugin motion cover (Canvas > motion > cover).
  const canvasEnabled = useCanvasEnabled();
  const reducedMotion = usePrefersReducedMotion();
  const canvasPath = useTrackCanvas(currentTrack);
  const canvasActive = canvasEnabled && !reducedMotion && !!canvasPath;

  // Cover ↔ artist slideshow (issue #466) — the ambient fallback backdrop,
  // one rung below the motion cover: Canvas > motion cover > slideshow >
  // static cover. So it only runs when no Canvas and no motion cover own the
  // slot, the global toggle is on, motion isn't reduced, and the artist has a
  // photo. `useAlbumMotionArtwork` is deduped process-wide (MotionCoverOverlay
  // reads the same key), so the extra call here is free.
  const slideshowEnabled = useCoverSlideshow().enabled;
  const motion = useAlbumMotionArtwork(
    currentTrack?.artist_name,
    currentTrack?.album_title,
    currentTrack?.album_id,
  );
  // Radio (negative sentinel id) and Spotify tracks have no library artist to
  // enrich, so they never get a slideshow — same eligibility the track menu
  // uses above.
  const slideshowEligible =
    !!currentTrack &&
    activeProvider !== "spotify" &&
    !isStreamTrack(currentTrack);
  // Only enrich the artist (a network call the immersive view doesn't
  // otherwise make) when the slideshow could actually run — off by default,
  // never while a Canvas or motion cover owns the slot, and only for an
  // eligible track — so this stays free unless the user opted in.
  const artistImage = useArtistImage(
    slideshowEnabled && !reducedMotion && !canvasActive && !motion && slideshowEligible
      ? currentTrack?.artist_id
      : null,
  );
  const slideshowActive =
    slideshowEnabled &&
    !reducedMotion &&
    !canvasActive &&
    !motion &&
    slideshowEligible &&
    !!artistImage;

  const title = currentTrack?.title ?? t("player.noTrack");
  const album = currentTrack?.album_title;

  return (
    // Whole stack (cover → metadata → transport) is centred together as
    // one group so the column never reads as "cover floating at top,
    // controls stranded at the bottom". The cover is sized so the full
    // stack still fits a 1080p viewport at 125 % DPI (see #54).
    <div className="h-full flex flex-col items-center justify-center text-white px-8 py-10 gap-7 min-h-0">
      {trackMenu.render()}
      <div className="relative w-full max-w-[min(42vh,24rem)] aspect-square shrink-0">
        <Artwork
          path={currentTrack?.artwork_path ?? null}
          path1x={currentTrack?.artwork_path_1x ?? null}
          path2x={currentTrack?.artwork_path_2x ?? null}
          size="full"
          className="w-full h-full shadow-2xl"
          iconSize={96}
          alt={title}
          rounded="2xl"
        />
        {!canvasActive && (
          <MotionCoverOverlay
            artist={currentTrack?.artist_name}
            album={currentTrack?.album_title}
            albumId={currentTrack?.album_id}
            rounded="2xl"
            className="shadow-2xl"
          />
        )}
        <CanvasStage
          path={canvasPath}
          enabled={canvasEnabled && !reducedMotion}
          rounded="2xl"
          className="shadow-2xl"
        />
        <CoverSlideshow
          artistSrc={artistImage}
          enabled={slideshowActive}
          rounded="2xl"
          className="shadow-2xl"
        />
      </div>

      {/* Track info — title + clickable artist + album. */}
      <div className="text-center max-w-2xl w-full shrink-0">
        {/* Long titles scroll instead of being cut by an ellipsis. The
            `pb-1` + `leading-tight` on the marquee container give
            descenders (g / y / p) room so the `overflow: hidden` (needed
            for both truncate + the marquee) doesn't clip them. */}
        {/* The title carries the track menu here — there is no row to
            right-click in this view. Focusable (and announced via
            `aria-haspopup`) only when a menu is actually available, so
            the Menu key / Shift+F10 reach it without a mouse (issue
            #436). Deliberately NOT `role="button"`: it opens a menu but
            it is still the heading, and Enter/Space are left alone. */}
        <h1
          className="text-3xl md:text-4xl font-bold focus:outline-none focus-visible:ring-2 focus-visible:ring-white/70 rounded"
          tabIndex={menuTrack ? 0 : undefined}
          aria-haspopup={menuTrack ? "menu" : undefined}
          onContextMenu={
            menuTrack ? (e) => trackMenu.open(e, menuTrack) : undefined
          }
          onKeyDown={
            menuTrack
              ? (e) => {
                  if (e.target !== e.currentTarget) return;
                  trackMenu.openFromKeyboard(e, menuTrack);
                }
              : undefined
          }
        >
          <MarqueeText text={title} className="leading-tight pb-1" />
        </h1>
        <div className="mt-2 text-lg text-white/80 flex items-center justify-center gap-3 flex-wrap">
          {currentTrack?.artist_name && (
            <ArtistLink
              name={currentTrack.artist_name}
              artistIds={currentTrack.artist_ids}
              onNavigate={(id) => {
                onNavigateToArtist(id);
                onClose();
              }}
            />
          )}
          {album && (
            <>
              <span className="text-white/40">·</span>
              <span className="truncate">{album}</span>
            </>
          )}
        </div>
        {/* Live radio: the station identity under the now-playing song
            (title/artist above carry the ICY song). */}
        {currentRadioStation && (
          <div className="mt-3 flex items-center justify-center gap-2 text-sm text-white/60">
            <Radio size={15} className="shrink-0" />
            <span className="truncate">
              {currentRadioStation.artist
                ? `${currentRadioStation.title} · ${currentRadioStation.artist}`
                : currentRadioStation.title}
            </span>
          </div>
        )}
      </div>

      {/* Transport — visualizer, progress, controls. */}
      <div className="w-full max-w-2xl shrink-0">
        <div className="fullscreen-now-playing-controls">
          {/* Spectrum visualizer — renders an empty canvas + draws
              nothing when the backend toggle is off, so it's safe to
              always mount. `glow` = white bars suited to the dim
              backdrop. */}
          <SpectrumVisualizer
            className="w-full h-16 mb-2 opacity-80"
            color={visualizerColor}
            rainbow={rainbow}
            glow
          />
          <ProgressBar />
          <div className="flex items-center justify-between gap-6 mt-2">
            {/* Left cluster — like / station favorite. Lives down here
                (not in the hero) so the visualizer canvas above never
                sits underneath an interactive control. */}
            <div className="flex-1 min-w-0 flex justify-start items-center gap-1">
              {currentRadioStation ? (
                <button
                  type="button"
                  onClick={() =>
                    radioFavorites.toggleFavorite(currentRadioStation)
                  }
                  aria-label={
                    stationFavorited
                      ? t("webRadio.removeFavorite")
                      : t("webRadio.addFavorite")
                  }
                  aria-pressed={stationFavorited}
                  className={`p-2 rounded-full transition-colors ${
                    stationFavorited
                      ? "text-amber-400 hover:text-amber-300"
                      : "text-white/60 hover:text-amber-400"
                  }`}
                >
                  <Star
                    size={20}
                    fill={stationFavorited ? "currentColor" : "none"}
                  />
                </button>
              ) : currentTrack && !isStreamTrack(currentTrack) ? (
                // `!isStreamTrack` guards the hydration race + idle tail: a
                // radio or remote-queue sentinel track (negative id) must
                // never show a ♥ like (no library row), even before
                // `currentRadioStation` arrives.
                <button
                  type="button"
                  onClick={onToggleLike}
                  aria-label={isLiked ? t("liked.unlike") : t("liked.like")}
                  aria-pressed={isLiked}
                  className={`p-2 rounded-full transition-colors ${
                    isLiked
                      ? "text-pink-400 hover:text-pink-300"
                      : "text-white/60 hover:text-pink-400"
                  }`}
                >
                  <Heart size={20} className={isLiked ? "fill-current" : ""} />
                </button>
              ) : null}
              {/* Visualizer colour cycle (issue #468) — only when the
                  visualizer is on AND the stored colour has loaded, so an
                  early click can't clobber it with a default-derived value. */}
              {visualizerOn && visualizerColorReady && (
                <VisualizerColorButton
                  colorId={colorId}
                  color={visualizerColor}
                  rainbow={rainbow}
                  onCycle={() => void cycle()}
                />
              )}
            </div>
            <PlaybackControls />
            <div className="flex-1 min-w-0 flex justify-end">
              <VolumeControl />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
