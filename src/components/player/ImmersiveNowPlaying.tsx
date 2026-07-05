import { useTranslation } from "react-i18next";
import { Heart, Star, Radio } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { MotionCoverOverlay } from "./MotionCoverOverlay";
import { ArtistLink } from "../common/ArtistLink";
import { MarqueeText } from "../common/MarqueeText";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";
import { SpectrumVisualizer } from "./SpectrumVisualizer";
import { usePlayer } from "../../hooks/usePlayer";
import { useWebRadioFavorites } from "../../hooks/useWebRadioFavorites";
import { isRadioTrack } from "../../lib/playerSources";

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
  const { currentTrack, currentRadioStation } = usePlayer();
  // Live radio: favorite the STATION (★) instead of liking a track (♥) —
  // a radio session has a negative sentinel id with no library row to
  // like. Mirrors the PlayerBar / mini-player treatment.
  const radioFavorites = useWebRadioFavorites();
  const stationFavorited =
    currentRadioStation != null &&
    radioFavorites.isFavorite(currentRadioStation.id);

  const title = currentTrack?.title ?? t("player.noTrack");
  const album = currentTrack?.album_title;

  return (
    // Whole stack (cover → metadata → transport) is centred together as
    // one group so the column never reads as "cover floating at top,
    // controls stranded at the bottom". The cover is sized so the full
    // stack still fits a 1080p viewport at 125 % DPI (see #54).
    <div className="h-full flex flex-col items-center justify-center text-white px-8 py-10 gap-7 min-h-0">
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
        <MotionCoverOverlay
          artist={currentTrack?.artist_name}
          album={currentTrack?.album_title}
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
        <h1 className="text-3xl md:text-4xl font-bold">
          <MarqueeText
            text={title}
            className="leading-tight pb-1"
          />
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
          <SpectrumVisualizer className="w-full h-16 mb-2 opacity-80" glow />
          <ProgressBar />
          <div className="flex items-center justify-between gap-6 mt-2">
            {/* Left cluster — like / station favorite. Lives down here
                (not in the hero) so the visualizer canvas above never
                sits underneath an interactive control. */}
            <div className="flex-1 min-w-0 flex justify-start">
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
              ) : currentTrack && !isRadioTrack(currentTrack) ? (
                // `!isRadioTrack` guards the hydration race + idle tail: a
                // radio sentinel track (negative id) must never show a ♥
                // like (no library row), even before `currentRadioStation`
                // arrives.
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
