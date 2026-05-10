import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { X, Heart } from "lucide-react";
import { Artwork } from "../common/Artwork";
import { ArtistLink } from "../common/ArtistLink";
import { PlaybackControls } from "./PlaybackControls";
import { ProgressBar } from "./ProgressBar";
import { VolumeControl } from "./VolumeControl";
import { usePlayer } from "../../hooks/usePlayer";

interface FullscreenNowPlayingProps {
  onClose: () => void;
  onNavigateToArtist: (artistId: number) => void;
  isLiked: boolean;
  onToggleLike: () => void;
}

/**
 * Apple-Music-style immersive Now Playing overlay. Reuses the same
 * playback controls + progress bar as the bottom player so behaviour
 * stays identical (drag-to-seek, repeat / shuffle states, etc.). The
 * cover is the visual anchor — large + centred — over a blurred copy
 * of itself for the background.
 *
 * Closes on Escape or via the X button. Does **not** persist any
 * state — opening + closing is purely a UI concern owned by the
 * parent (PlayerBar).
 */
export function FullscreenNowPlaying({
  onClose,
  onNavigateToArtist,
  isLiked,
  onToggleLike,
}: FullscreenNowPlayingProps) {
  const { t } = useTranslation();
  const { currentTrack } = usePlayer();

  // Escape to close. Only mounted while the overlay is open so we
  // don't race the global keyboard-shortcut handler.
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [onClose]);

  const title = currentTrack?.title ?? t("player.noTrack");
  const album = currentTrack?.album_title;

  return (
    <div className="fixed inset-0 z-100 animate-fade-in">
      {/* Blurred artwork background — falls back to a flat dark
          gradient when the track has no cover. Same recipe as the
          fullscreen lyrics overlay so they feel like siblings. */}
      <div className="absolute inset-0 overflow-hidden">
        {currentTrack?.artwork_path ? (
          <Artwork
            path={currentTrack.artwork_path}
            path1x={currentTrack.artwork_path_1x}
            path2x={currentTrack.artwork_path_2x}
            size="full"
            className="w-full h-full scale-150 blur-3xl"
            alt=""
            rounded="md"
          />
        ) : (
          <div className="w-full h-full bg-linear-to-br from-zinc-800 to-zinc-950" />
        )}
        <div className="absolute inset-0 bg-black/65" />
      </div>

      {/* Foreground */}
      <div className="relative h-full flex flex-col text-white">
        {/* Top bar — close button only; the controls live at the
            bottom so the cover gets the visual centre. */}
        <div className="flex items-center justify-end px-8 py-6 shrink-0">
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="p-2.5 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
          >
            <X size={22} />
          </button>
        </div>

        {/* Cover hero */}
        <div className="flex-1 flex flex-col items-center justify-center px-8 min-h-0">
          <div className="w-full max-w-[min(60vh,32rem)] aspect-square mb-8">
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
          </div>

          {/* Track info — title + clickable artist + album. */}
          <div className="text-center max-w-2xl w-full mb-2">
            <h1 className="text-3xl md:text-4xl font-bold truncate">{title}</h1>
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
          </div>

          {/* Like + controls cluster. Like sits to the left of the
              transport so it stays one tap away without competing
              with play/pause for visual weight. */}
          {currentTrack && (
            <div className="mt-2 flex items-center gap-2">
              <button
                type="button"
                onClick={onToggleLike}
                aria-label={
                  isLiked ? t("liked.unlike") : t("liked.like")
                }
                className={`p-2 rounded-full transition-colors ${
                  isLiked
                    ? "text-pink-400 hover:text-pink-300"
                    : "text-white/60 hover:text-pink-400"
                }`}
              >
                <Heart size={20} className={isLiked ? "fill-current" : ""} />
              </button>
            </div>
          )}
        </div>

        {/* Bottom transport — progress on top, controls below.
            Width-capped so on ultrawide monitors the layout stays
            visually balanced instead of stretching edge-to-edge. */}
        <div className="px-8 pb-10 shrink-0">
          <div className="max-w-3xl mx-auto fullscreen-now-playing-controls">
            <ProgressBar />
            <div className="flex items-center justify-between gap-6 mt-2">
              <div className="flex-1 min-w-0" />
              <PlaybackControls />
              <div className="flex-1 min-w-0 flex justify-end">
                <VolumeControl />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

